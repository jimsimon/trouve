#!/usr/bin/env node
/**
 * Complete local qualification matrix for Cursor's SDK Bridge candidate.
 *
 * This extends the small release smoke test with host-owned permission
 * outcomes, structured and image content, multiple tool schemas, read
 * concurrency, cancellation and recovery, durable replay, plan mode, usage,
 * cold Bridge-process resume, authentication boundaries, and operational
 * metrics. It never prints account identity or CURSOR_API_KEY and stores all
 * agent state under a disposable temp directory.
 */

import { randomBytes, timingSafeEqual } from "node:crypto";
import { constants as fsConstants } from "node:fs";
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
} from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";

import {
  BRIDGE_VERSION,
  CURSOR_NATIVE_TOOL_DENYLIST,
  QualificationError,
  assistantText,
  assertUniqueToolLifecycle,
  bridgeReleaseAttestation,
  combineQualificationAndCleanupErrors,
  createProcessCleanupBoundary,
  exactTerminalResult,
  installSignalCleanup,
  isUnsupportedRpcMethodError,
  parseTimeoutSeconds,
  readBoundedJsonResponse,
  redact,
  resolveBridge,
  runCleanupSteps,
  sdkMessages,
  send,
  serverStream,
  startBridge,
  terminalStatusIsFinished,
  terminateProcessTree,
  unary,
  verifyToolAllowlist,
} from "./qualify_cursor_sdk_bridge.mjs";

const REPOSITORY_ROOT = fileURLToPath(new URL("../", import.meta.url));
const CALLBACK_PATH = "/sdk.v1.SdkCustomToolCallbackService/CallCustomTool";
const MAX_CALLBACK_BODY_BYTES = 4 * 1024 * 1024;
const MAX_QUALIFICATION_CALLBACKS = 256;
const MAX_QUALIFICATION_CALLBACK_CONCURRENCY = 16;
const SCHEMA_PROBE_COUNT = 128;
export const RED_PIXEL_PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

const TOOLS = {
  read: "trouve_qualification_read",
  deny: "trouve_qualification_permission_denied",
  image: "trouve_qualification_structured_image",
  parallelA: "trouve_qualification_parallel_read_a",
  parallelB: "trouve_qualification_parallel_read_b",
  block: "trouve_qualification_block",
};

const RESULTS = {
  read: "TROUVE_CURSOR_READ_OK",
  preRestartRead: "TROUVE_CURSOR_PRE_RESTART_OK",
  coldResumeRead: "TROUVE_CURSOR_COLD_RESUME_OK",
  denied: "TROUVE_CURSOR_PERMISSION_DENIED",
  image: "TROUVE_CURSOR_IMAGE_OK",
  parallelA: "TROUVE_CURSOR_PARALLEL_",
  parallelB: "OK",
  parallelFinal: "TROUVE_CURSOR_PARALLEL_OK",
  blockReleased: "TROUVE_CURSOR_BLOCK_RELEASED",
};

const help = `Usage: node scripts/qualify_cursor_sdk_bridge_full.mjs [options]

Requires CURSOR_API_KEY. This is a live qualification and performs several
billable local Cursor SDK turns.

Options:
  --health-only       Test direct subscription health without starting a Bridge
  --bridge PATH       Use an existing binary (reported as unverified, not pinned)
  --workspace PATH    Read-only qualification workspace (default: repo root)
  --model ID          Cursor model id (default: composer-2 when available)
  --timeout SECONDS   Timeout for each operation/turn (default: 300)
  --keep-state        Keep the isolated temporary directory for inspection
  --help              Show this help
`;

function parseArgs(argv) {
  const parsed = {
    bridge: process.env.CURSOR_SDK_BRIDGE_BIN,
    workspace: REPOSITORY_ROOT,
    model: process.env.CURSOR_QUALIFICATION_MODEL,
    timeoutSeconds: 300,
    keepState: false,
    healthOnly: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--help") {
      process.stdout.write(help);
      process.exit(0);
    }
    if (argument === "--keep-state") {
      parsed.keepState = true;
      continue;
    }
    if (argument === "--health-only") {
      parsed.healthOnly = true;
      continue;
    }
    const key = {
      "--bridge": "bridge",
      "--workspace": "workspace",
      "--model": "model",
      "--timeout": "timeoutSeconds",
    }[argument];
    if (key === undefined || index + 1 >= argv.length) {
      throw new QualificationError(`unexpected or incomplete argument: ${argument}`);
    }
    parsed[key] = argv[index + 1];
    index += 1;
  }
  parsed.timeoutSeconds = parseTimeoutSeconds(parsed.timeoutSeconds);
  parsed.workspace = resolve(parsed.workspace);
  if (parsed.bridge !== undefined) parsed.bridge = resolve(parsed.bridge);
  return parsed;
}

function deferred() {
  let resolvePromise;
  let rejectPromise;
  const promise = new Promise((accept, reject) => {
    resolvePromise = accept;
    rejectPromise = reject;
  });
  return { promise, resolve: resolvePromise, reject: rejectPromise };
}

function delay(milliseconds) {
  return new Promise((accept) => setTimeout(accept, milliseconds));
}

async function withTimeout(promise, milliseconds, label) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new QualificationError(`${label} timed out`)),
          milliseconds,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function bearerMatches(header, token) {
  const presented = Buffer.from(header ?? "", "utf8");
  const expected = Buffer.from(`Bearer ${token}`, "utf8");
  return presented.length === expected.length && timingSafeEqual(presented, expected);
}

export function createCallbackAdmission(
  maximumTotal = MAX_QUALIFICATION_CALLBACKS,
  maximumConcurrent = MAX_QUALIFICATION_CALLBACK_CONCURRENCY,
) {
  let total = 0;
  let active = 0;
  return {
    tryAcquire() {
      if (total >= maximumTotal || active >= maximumConcurrent) return undefined;
      total += 1;
      active += 1;
      let released = false;
      return () => {
        if (released) return;
        released = true;
        active -= 1;
      };
    },
    snapshot() {
      return { total, active };
    },
  };
}

async function readRequestBody(request, timeoutMilliseconds) {
  const chunks = [];
  let length = 0;
  const timer = setTimeout(
    () => request.destroy(new QualificationError("callback request body timed out")),
    Math.min(timeoutMilliseconds, 30_000),
  );
  try {
    for await (const chunk of request) {
      length += chunk.length;
      if (length > MAX_CALLBACK_BODY_BYTES) {
        request.destroy();
        throw new QualificationError("callback request exceeded the qualification limit");
      }
      chunks.push(chunk);
    }
    return Buffer.concat(chunks).toString("utf8");
  } finally {
    clearTimeout(timer);
  }
}

export async function startCallbackServer(handlers, timeoutMilliseconds) {
  const bearer = randomBytes(32).toString("base64url");
  const calls = [];
  const failures = [];
  const admission = createCallbackAdmission();
  const server = createServer(async (request, response) => {
    let releaseAdmission;
    let record;
    const cancelled = deferred();
    let cancelledAtMs = null;
    response.once("close", () => {
      if (response.writableEnded) return;
      cancelledAtMs = performance.now();
      if (record !== undefined) record.cancelledAtMs = cancelledAtMs;
      cancelled.resolve();
    });
    try {
      const path = new URL(request.url ?? "/", "http://127.0.0.1").pathname;
      if (request.method !== "POST" || path !== CALLBACK_PATH) {
        response.writeHead(404).end();
        return;
      }
      if (!bearerMatches(request.headers.authorization, bearer)) {
        response.writeHead(401, { "content-type": "application/json" });
        response.end(JSON.stringify({ code: "unauthenticated", message: "Unauthorized" }));
        return;
      }
      releaseAdmission = admission.tryAcquire();
      if (releaseAdmission === undefined) {
        request.resume();
        response.writeHead(429, { "content-type": "application/json" });
        response.end(
          JSON.stringify({
            code: "resource_exhausted",
            message: "qualification callback capacity exhausted",
          }),
        );
        return;
      }
      const body = JSON.parse(await readRequestBody(request, timeoutMilliseconds));
      if (
        typeof body.toolName !== "string" ||
        typeof body.agentId !== "string" ||
        body.args === null ||
        typeof body.args !== "object" ||
        Array.isArray(body.args)
      ) {
        throw new QualificationError("malformed custom-tool callback");
      }
      const handler = handlers.get(body.toolName);
      if (handler === undefined) {
        throw new QualificationError(`unexpected custom tool: ${body.toolName}`);
      }
      record = {
        toolName: body.toolName,
        toolCallId: body.toolCallId,
        agentId: body.agentId,
        startedAtMs: performance.now(),
        completedAtMs: null,
        cancelledAtMs,
        cancelled,
        settled: deferred(),
        ok: false,
      };
      calls.push(record);
      const result = await handler(body.args, record);
      record.completedAtMs = performance.now();
      record.ok = true;
      if (!response.destroyed) {
        response.writeHead(200, { "content-type": "application/json" });
        response.end(JSON.stringify({ result }));
      }
    } catch (error) {
      failures.push(error);
      if (!response.destroyed) {
        response.writeHead(400, { "content-type": "application/json" });
        response.end(
          JSON.stringify({
            code: "invalid_argument",
            message: error instanceof Error ? error.message : String(error),
          }),
        );
      }
    } finally {
      releaseAdmission?.();
      record?.settled.resolve();
    }
  });
  await new Promise((accept, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", accept);
  });
  const address = server.address();
  if (address === null || typeof address === "string") {
    server.close();
    throw new QualificationError("callback server did not bind TCP");
  }
  return {
    bearer,
    admission,
    calls,
    failures,
    server,
    url: `http://127.0.0.1:${address.port}`,
  };
}

function objectSchema(properties = {}, required = []) {
  return {
    type: "object",
    properties,
    required,
    additionalProperties: false,
  };
}

function buildCustomTools() {
  const definitions = {
    [TOOLS.read]: {
      description: "Return the qualification read sentinel.",
      inputSchema: objectSchema({ token: { type: "string" } }, ["token"]),
    },
    [TOOLS.deny]: {
      description: "Exercise a Trouve-owned denied mutation result.",
      inputSchema: objectSchema({ operation: { type: "string" } }, ["operation"]),
    },
    [TOOLS.image]: {
      description: "Return text, structured content, and an inline PNG result.",
      inputSchema: objectSchema({ token: { type: "string" } }, ["token"]),
    },
    [TOOLS.parallelA]: {
      description: "Delayed read-only qualification callback A.",
      inputSchema: objectSchema(),
    },
    [TOOLS.parallelB]: {
      description: "Delayed read-only qualification callback B.",
      inputSchema: objectSchema(),
    },
    [TOOLS.block]: {
      description: "Block until the host releases this cancellation probe.",
      inputSchema: objectSchema(),
    },
  };
  for (let index = 0; index < SCHEMA_PROBE_COUNT; index += 1) {
    const name = `trouve_schema_probe_${String(index).padStart(3, "0")}`;
    definitions[name] = {
      description: `Synthetic effective-schema capacity probe ${index}.`,
      inputSchema: objectSchema({
        value: {
          type: "string",
          description: "A value that is never requested during qualification.",
        },
      }),
    };
  }
  return definitions;
}

function fullAgentOptions(apiKey, model, workspace, customTools) {
  return {
    model: { id: model },
    apiKey,
    name: "Trouve Cursor SDK Bridge full qualification",
    tools: { names: ["mcp"] },
    disallowedTools: [...CURSOR_NATIVE_TOOL_DENYLIST],
    mcpServers: {},
    agents: {},
    local: {
      cwd: [workspace],
      settingSources: [],
      sandboxOptions: { enabled: false },
      store: { type: "sqlite" },
      autoReview: false,
      customTools,
    },
  };
}

function messageRunId(frame) {
  const message = frame.sdkMessage?.message;
  if (typeof message?.run_id === "string") return message.run_id;
  if (typeof frame.result?.runId === "string") return frame.result.runId;
  if (typeof frame.done?.runId === "string") return frame.done.runId;
  return undefined;
}

function terminalFrame(frames) {
  return frames.find((frame) => frame.result !== undefined)?.result;
}

function statusIsCancelled(status) {
  return (
    status === 5 ||
    status === "5" ||
    status === "RUN_LIFECYCLE_STATUS_CANCELLED"
  );
}

function numberValue(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number : 0;
}

function normalizeUsage(usage) {
  if (usage === undefined || usage === null) return null;
  const normalized = {
    input_tokens: numberValue(usage.inputTokens),
    output_tokens: numberValue(usage.outputTokens),
    cache_read_tokens: numberValue(usage.cacheReadTokens),
    cache_write_tokens: numberValue(usage.cacheWriteTokens),
    total_tokens: numberValue(usage.totalTokens),
  };
  if (usage.reasoningTokens !== undefined) {
    normalized.reasoning_tokens = numberValue(usage.reasoningTokens);
  }
  return normalized;
}

function usageFromFrames(frames) {
  const terminal = terminalFrame(frames);
  const terminalUsage = normalizeUsage(terminal?.result?.usage);
  if (terminalUsage !== null) return terminalUsage;
  const usageMessage = sdkMessages(frames).find((message) => message.type === "usage");
  return normalizeUsage(usageMessage?.usage);
}

function sorted(values) {
  return [...values].sort();
}

function arraysEqual(left, right) {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

export function inspectToolCalls(frames, callbacks, expectedTools, expectToolError, label) {
  const messages = sdkMessages(frames);
  const toolMessages = messages.filter((message) => message.type === "tool_call");
  assertUniqueToolLifecycle(toolMessages, label);
  const actualTools = sorted(callbacks.map((call) => call.toolName));
  const expected = sorted(expectedTools);
  if (!arraysEqual(actualTools, expected)) {
    throw new QualificationError(
      `callbacks differed: expected ${expected.join(", ")}, got ${actualTools.join(", ")}`,
    );
  }
  const streamNames = sorted(
    new Set(toolMessages.map((message) => message.name).filter(Boolean)),
  );
  const unexpectedNames = streamNames.filter(
    (name) => name !== "mcp" && !expectedTools.includes(name),
  );
  if (unexpectedNames.length > 0) {
    throw new QualificationError(
      `native or unexpected stream tools appeared: ${unexpectedNames.join(", ")}`,
    );
  }
  const streamIds = new Set(
    toolMessages.map((message) => message.call_id).filter(Boolean),
  );
  if (streamIds.size !== expectedTools.length) {
    throw new QualificationError(
      `expected ${expectedTools.length} stream call ids, observed ${streamIds.size}`,
    );
  }
  const callbackIds = [];
  for (const callback of callbacks) {
    if (typeof callback.toolCallId !== "string" || callback.toolCallId.length === 0) {
      throw new QualificationError(
        `callback ${callback.toolName} omitted its tool-call id`,
      );
    }
    callbackIds.push(callback.toolCallId);
  }
  const uniqueCallbackIds = new Set(callbackIds);
  if (
    uniqueCallbackIds.size !== callbacks.length ||
    !arraysEqual(sorted(uniqueCallbackIds), sorted(streamIds))
  ) {
    throw new QualificationError(
      `${label}: callback and stream call ids were not one-to-one ` +
        `(callbacks=${callbackIds.join(",") || "none"}, ` +
        `stream=${[...streamIds].join(",") || "none"})`,
    );
  }
  const errorIds = new Set(
    toolMessages
      .filter((message) => message.status === "error")
      .map((message) => message.call_id)
      .filter(Boolean),
  );
  if (!expectToolError && errorIds.size > 0) {
    throw new QualificationError("a custom tool unexpectedly completed with error status");
  }
  if (
    expectToolError &&
    !arraysEqual(sorted(errorIds), sorted(streamIds))
  ) {
    throw new QualificationError(
      `${label}: denied custom-tool callbacks did not finish with error stream events`,
    );
  }
  const completedIds = new Set(
    toolMessages
      .filter((message) => message.status === "completed" || message.status === "error")
      .map((message) => message.call_id)
      .filter(Boolean),
  );
  const terminalEventsComplete = completedIds.size === expectedTools.length;
  if (!terminalEventsComplete) {
    const statuses = sorted(
      new Set(toolMessages.map((message) => message.status).filter(Boolean)),
    );
    throw new QualificationError(
      `${label}: not every custom tool call reached a terminal stream event ` +
        `(statuses=${statuses.join(",") || "none"})`,
    );
  }
  return {
    callback_count: callbacks.length,
    callback_tools: actualTools,
    stream_names: streamNames,
    call_ids_correlated: callbacks.every(
      (callback) => streamIds.has(callback.toolCallId),
    ),
    terminal_stream_events_complete: terminalEventsComplete,
    native_tools_present: false,
  };
}

async function runTurn({
  bridge,
  callback,
  agentId,
  label,
  prompt,
  expectedTools,
  expectedText,
  timeoutMilliseconds,
  images,
  mode,
  force = false,
  expectToolError = false,
}) {
  const callbackStart = callback.calls.length;
  const failureStart = callback.failures.length;
  const startedAt = performance.now();
  const frames = await send(
    bridge,
    {
      agentId,
      message: {
        text: prompt,
        ...(images === undefined ? {} : { images }),
      },
      options: {
        enableDeltas: true,
        enableSteps: true,
        ...(mode === undefined ? {} : { mode }),
        ...(force ? { local: { force: true } } : {}),
      },
    },
    timeoutMilliseconds,
  );
  if (callback.failures.length !== failureStart) {
    throw callback.failures.at(-1);
  }
  const callbacks = callback.calls.slice(callbackStart);
  if (callbacks.some((call) => call.agentId !== agentId)) {
    throw new QualificationError(`${label}: callback agent id did not match the active agent`);
  }
  const tools = inspectToolCalls(
    frames,
    callbacks,
    expectedTools,
    expectToolError,
    label,
  );
  const terminal = exactTerminalResult(frames, label);
  if (!terminalStatusIsFinished(terminal.status)) {
    throw new QualificationError(
      `${label} ended with non-finished status ${terminal?.status}`,
    );
  }
  const messages = sdkMessages(frames);
  const finalText = String(terminal.result?.result ?? assistantText(messages));
  if (expectedText !== undefined && !finalText.includes(expectedText)) {
    throw new QualificationError(`${label} did not use expected result ${expectedText}`);
  }
  if (!finalText.trim()) throw new QualificationError(`${label} produced no final text`);
  const messageTypes = sorted(new Set(messages.map((message) => message.type)));
  return {
    frames,
    summary: {
      label,
      run_id: terminal.runId ?? terminal.result?.runId,
      duration_ms: Math.round(performance.now() - startedAt),
      vendor_duration_ms: numberValue(terminal.result?.durationMs),
      model: terminal.result?.model?.id ?? null,
      usage: usageFromFrames(frames),
      message_types: messageTypes,
      interaction_updates: frames.filter((frame) => frame.interactionUpdate !== undefined)
        .length,
      conversation_steps: frames.filter((frame) => frame.step !== undefined).length,
      assistant_used_result: expectedText === undefined || finalText.includes(expectedText),
      requested_mode: mode ?? null,
      ...tools,
    },
  };
}

async function observeCompletedRun(bridge, runId, timeoutMilliseconds) {
  const replay = await serverStream(
    bridge,
    "SdkAgentService",
    "ObserveRun",
    { runId },
    timeoutMilliseconds,
  );
  const terminal = exactTerminalResult(replay, "ObserveRun replay");
  if (!terminalStatusIsFinished(terminal.status)) {
    throw new QualificationError("ObserveRun did not replay a finished result");
  }
  const offsets = replay
    .map((frame) => frame.offset)
    .filter((offset) => typeof offset === "string" && offset.length > 0);
  if (offsets.length === 0) {
    throw new QualificationError("ObserveRun replay omitted durable offsets");
  }
  const afterOffset = offsets[0];
  const offsetIndex = replay.findIndex((frame) => frame.offset === afterOffset);
  const expectedSuffix = replay.slice(offsetIndex + 1);
  const resumed = await serverStream(
    bridge,
    "SdkAgentService",
    "ObserveRun",
    { runId, afterOffset },
    timeoutMilliseconds,
  );
  const resumedOffsets = resumed
    .map((frame) => frame.offset)
    .filter((offset) => typeof offset === "string" && offset.length > 0);
  if (resumedOffsets.includes(afterOffset)) {
    throw new QualificationError("ObserveRun afterOffset was not exclusive");
  }
  if (JSON.stringify(resumed) !== JSON.stringify(expectedSuffix)) {
    throw new QualificationError(
      "ObserveRun afterOffset did not return the exact replay suffix",
    );
  }
  if (resumed.filter((frame) => frame.done !== undefined).length !== 1) {
    throw new QualificationError("resumed ObserveRun replay did not contain exactly one done");
  }
  return {
    full_replay: true,
    durable_offsets: offsets.length,
    exclusive_offset_resume: true,
  };
}

async function qualifyCancellation({
  bridge,
  callback,
  agentId,
  timeoutMilliseconds,
  blockControl,
}) {
  const callbackStart = callback.calls.length;
  const runIdReady = deferred();
  const startedAt = performance.now();
  const observedSend = send(
    bridge,
    {
      agentId,
      message: {
        text: `Call ${TOOLS.block} exactly once, then wait for its result.`,
      },
      options: { enableDeltas: true, enableSteps: true },
    },
    timeoutMilliseconds,
    (frame) => {
      const runId = messageRunId(frame);
      if (runId !== undefined) runIdReady.resolve(runId);
    },
  ).then(
    (frames) => ({ frames, error: null }),
    (error) => ({ frames: null, error }),
  );

  const [runId, callbackRecord] = await withTimeout(
    Promise.all([runIdReady.promise, blockControl.started.promise]).then(
      ([resolvedRunId, record]) => [resolvedRunId, record],
    ),
    Math.min(timeoutMilliseconds, 60_000),
    "cancellation probe startup",
  );
  await unary(
    bridge,
    "SdkAgentService",
    "CancelRun",
    { runId, agentId },
    timeoutMilliseconds,
  );
  const [outcome] = await Promise.all([
    observedSend,
    withTimeout(
      callbackRecord.cancelled.promise,
      Math.min(timeoutMilliseconds, 10_000),
      "custom-tool callback cancellation",
    ),
  ]);
  await withTimeout(
    callbackRecord.settled.promise,
    Math.min(timeoutMilliseconds, 10_000),
    "custom-tool callback handler settlement",
  );
  const callbackAdmission = callback.admission.snapshot();
  if (callbackAdmission.active !== 0) {
    throw new QualificationError(
      `custom-tool callback admission remained active after cancellation (${callbackAdmission.active})`,
    );
  }
  if (outcome.error !== null) throw outcome.error;
  const frames = outcome.frames;
  const terminal = exactTerminalResult(frames, "cancelled run");
  if (!statusIsCancelled(terminal.status)) {
    throw new QualificationError(
      `cancelled run ended with status ${terminal?.status}`,
    );
  }
  const callbacks = callback.calls.slice(callbackStart);
  inspectToolCalls(frames, callbacks, [TOOLS.block], true, "cancelled run");
  blockControl.release.resolve({ value: RESULTS.blockReleased });
  return {
    run_id: runId,
    cancellation_acknowledged: true,
    terminal_status: String(terminal.status),
    done: true,
    callback_request_cancelled: true,
    callback_handler_settled: true,
    callback_admission_active: callbackAdmission.active,
    callback_count: callbacks.length,
    duration_ms: Math.round(performance.now() - startedAt),
  };
}

async function bridgeBearerFailsClosed(bridge, timeoutMilliseconds) {
  const response = await fetch(`${bridge.url}/sdk.v1.SdkBridgeControlService/Ping`, {
    method: "POST",
    headers: {
      authorization: "Bearer deliberately-invalid-qualification-token",
      "content-type": "application/json",
    },
    body: "{}",
    signal: AbortSignal.timeout(Math.min(timeoutMilliseconds, 10_000)),
  });
  const rejected = response.status === 401;
  await response.body?.cancel();
  return rejected;
}

async function callbackBearerFailsClosed(callback, timeoutMilliseconds) {
  const response = await fetch(`${callback.url}${CALLBACK_PATH}`, {
    method: "POST",
    headers: {
      authorization: "Bearer deliberately-invalid-qualification-token",
      "content-type": "application/json",
    },
    body: JSON.stringify({
      toolName: TOOLS.read,
      args: { token: "unauthorized" },
      agentId: "unauthorized",
    }),
    signal: AbortSignal.timeout(Math.min(timeoutMilliseconds, 10_000)),
  });
  const rejected = response.status === 401;
  await response.body?.cancel();
  return rejected;
}

async function processRssBytes(pid) {
  if (process.platform !== "linux") return null;
  try {
    const status = await readFile(`/proc/${pid}/status`, "utf8");
    const match = /^VmRSS:\s+(\d+)\s+kB$/mu.exec(status);
    return match === null ? null : Number(match[1]) * 1024;
  } catch {
    return null;
  }
}

async function getLocalUsage(bridge, agentId, timeoutMilliseconds) {
  await delay(750);
  try {
    const result = await unary(
      bridge,
      "SdkAgentService",
      "GetUsage",
      { agentId },
      timeoutMilliseconds,
    );
    return {
      supported: true,
      usage: normalizeUsage(result.usage?.usage),
      cost: result.usage?.cost ?? null,
      run_entries: Array.isArray(result.usage?.runs) ? result.usage.runs.length : 0,
    };
  } catch (error) {
    if (!isUnsupportedRpcMethodError(error, "GetUsage")) throw error;
    return {
      supported: false,
      reason: error instanceof Error ? error.message : String(error),
    };
  }
}

export async function qualifySubscriptionHealth(apiKey, timeoutMilliseconds) {
  if (apiKey.length === 0) {
    throw new QualificationError(
      "CURSOR_API_KEY is required for subscription-health qualification",
    );
  }
  const origin = "https://api2.cursor.sh";
  const exchange = await fetch(`${origin}/auth/exchange_user_api_key`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${apiKey}`,
      "content-type": "application/json",
    },
    body: "{}",
    signal: AbortSignal.timeout(Math.min(timeoutMilliseconds, 30_000)),
  });
  const exchangeBody = await readBoundedJsonResponse(
    exchange,
    "Cursor API-key exchange",
  );
  const accessToken =
    exchange.ok && typeof exchangeBody.accessToken === "string"
      ? exchangeBody.accessToken
      : "";
  if (accessToken.length === 0) {
    throw new QualificationError(
      `Cursor API-key exchange failed closed (HTTP ${exchange.status})`,
    );
  }

  const dashboard = async (method) => {
    const response = await fetch(
      `${origin}/aiserver.v1.DashboardService/${method}`,
      {
        method: "POST",
        headers: {
          authorization: `Bearer ${accessToken}`,
          "connect-protocol-version": "1",
          "content-type": "application/json",
        },
        body: "{}",
        signal: AbortSignal.timeout(Math.min(timeoutMilliseconds, 30_000)),
      },
    );
    const body = await readBoundedJsonResponse(response, `Cursor ${method}`);
    if (!response.ok) {
      throw new QualificationError(
        `Cursor ${method} failed after API-key exchange (HTTP ${response.status})`,
      );
    }
    return body;
  };
  const usage = await dashboard("GetCurrentPeriodUsage");
  const plan = await dashboard("GetPlanInfo");
  const planUsageFields = [
    "totalPercentUsed",
    "apiPercentUsed",
    "autoPercentUsed",
  ].filter((field) => Number.isFinite(usage.planUsage?.[field]));
  if (!isNonEmptyTimestamp(usage.billingCycleEnd) || planUsageFields.length === 0) {
    throw new QualificationError(
      "Cursor GetCurrentPeriodUsage omitted the billing cycle or plan meters",
    );
  }
  if (
    typeof plan.planInfo?.planName !== "string" ||
    plan.planInfo.planName.length === 0
  ) {
    throw new QualificationError("Cursor GetPlanInfo omitted the plan name");
  }
  return {
    result: "passed",
    transport: "direct-api-key-exchange-and-dashboard-connect",
    cursor_cli_invoked: false,
    cursor_auth_file_read: false,
    access_token_persisted: false,
    billing_cycle_reported: true,
    plan_usage_fields: planUsageFields,
    spend_window_reported:
      (usage.spendLimitUsage?.individualUsed !== undefined &&
        usage.spendLimitUsage?.individualLimit !== undefined) ||
      (usage.spendLimitUsage?.pooledUsed !== undefined &&
        usage.spendLimitUsage?.pooledLimit !== undefined),
    plan_name_reported: true,
    experimental_dashboard_rpc: true,
  };
}

export function isNonEmptyTimestamp(value) {
  if (typeof value !== "string") return false;
  const match = /^(\d{4})-(\d{2})-(\d{2})[Tt](\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:[Zz]|([+-])(\d{2}):(\d{2}))$/u.exec(value);
  if (match === null) return false;

  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const hour = Number(match[4]);
  const minute = Number(match[5]);
  const second = Number(match[6]);
  const offsetHour = match[8] === undefined ? 0 : Number(match[8]);
  const offsetMinute = match[9] === undefined ? 0 : Number(match[9]);
  if (
    year === 0 ||
    month < 1 ||
    month > 12 ||
    hour > 23 ||
    minute > 59 ||
    second > 59 ||
    offsetHour > 23 ||
    offsetMinute > 59
  ) {
    return false;
  }
  const leapYear = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const daysInMonth = [31, leapYear ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  return day >= 1 && day <= daysInMonth[month - 1];
}

function assistantConversationText(conversation) {
  if (!Array.isArray(conversation)) return [];
  const text = [];
  for (const entry of conversation) {
    if (entry?.type !== "agentConversationTurn" || !Array.isArray(entry.turn?.steps)) {
      continue;
    }
    for (const step of entry.turn.steps) {
      if (
        step?.type === "assistantMessage" &&
        typeof step.message?.text === "string"
      ) {
        text.push(step.message.text);
      }
    }
  }
  return text;
}

export function agentMessagesContainEvidence(messages, expectedEvidence) {
  if (!Array.isArray(messages) || typeof expectedEvidence !== "string") return false;
  return messages.some((message) => {
    if (message?.type !== "assistant" || !Array.isArray(message.message?.content)) {
      return false;
    }
    return message.message.content.some((block) =>
      block?.type === "text" &&
      typeof block.text === "string" &&
      block.text.includes(expectedEvidence));
  });
}

export function parseConversationEvidence(conversationJson, label, expectedEvidence) {
  if (typeof conversationJson !== "string" || conversationJson.length === 0) {
    throw new QualificationError(`${label} omitted conversation JSON`);
  }
  let conversation;
  try {
    conversation = JSON.parse(conversationJson);
  } catch (error) {
    throw new QualificationError(`${label} returned invalid conversation JSON: ${error}`);
  }
  if (!assistantConversationText(conversation).some((text) => text.includes(expectedEvidence))) {
    throw new QualificationError(`${label} omitted expected durable evidence ${expectedEvidence}`);
  }
  return conversation;
}

async function fullQualification(args) {
  const apiKey = process.env.CURSOR_API_KEY;
  if (!apiKey) {
    throw new QualificationError("CURSOR_API_KEY is required for full qualification");
  }
  await access(args.workspace, fsConstants.R_OK);
  const timeoutMilliseconds = args.timeoutSeconds * 1000;
  const temporaryRoot = await mkdtemp(join(tmpdir(), "trouve-cursor-full-"));
  const stateRoot = join(temporaryRoot, "state");
  await mkdir(stateRoot);

  let activeReads = 0;
  let maxActiveReads = 0;
  let blockControl = null;
  const handlers = new Map();
  handlers.set(TOOLS.read, async (input) => {
    const result = new Map([
      ["full-read", RESULTS.read],
      ["pre-restart-read", RESULTS.preRestartRead],
      ["cold-resume-read", RESULTS.coldResumeRead],
    ]).get(input.token);
    if (result === undefined) {
      throw new QualificationError("read callback token differed");
    }
    return { value: result };
  });
  handlers.set(TOOLS.deny, async () => ({
    content: [{ type: "text", text: RESULTS.denied }],
    isError: true,
    structuredContent: { permission: "denied", owner: "trouve" },
  }));
  handlers.set(TOOLS.image, async (input) => {
    if (input.token !== "red") {
      throw new QualificationError("image callback did not identify the attached image");
    }
    return {
      content: [
        { type: "text", text: RESULTS.image },
        { type: "image", data: RED_PIXEL_PNG, mimeType: "image/png" },
      ],
      structuredContent: { result: RESULTS.image, width: 1, height: 1 },
    };
  });
  const delayedRead = (value) => async () => {
    activeReads += 1;
    maxActiveReads = Math.max(maxActiveReads, activeReads);
    try {
      await delay(900);
      return { value };
    } finally {
      activeReads -= 1;
    }
  };
  handlers.set(TOOLS.parallelA, delayedRead(RESULTS.parallelA));
  handlers.set(TOOLS.parallelB, delayedRead(RESULTS.parallelB));
  handlers.set(TOOLS.block, async (_input, record) => {
    if (blockControl === null) {
      throw new QualificationError("block callback invoked outside cancellation test");
    }
    blockControl.started.resolve(record);
    return Promise.race([
      blockControl.release.promise,
      record.cancelled.promise.then(() => ({ value: RESULTS.blockReleased })),
    ]);
  });
  for (let index = 0; index < SCHEMA_PROBE_COUNT; index += 1) {
    const name = `trouve_schema_probe_${String(index).padStart(3, "0")}`;
    handlers.set(name, async () => ({ value: "UNEXPECTED_SCHEMA_PROBE_CALL" }));
  }

  let callback;
  let bridge;
  let agentId;
  let resolvedBridge;
  const bridgeChildren = new Set();
  const operational = {};
  let cleanupPromise;
  const cleanup = (signal) => {
    if (cleanupPromise !== undefined) return cleanupPromise;
    cleanupPromise = (async () => {
      if (blockControl !== null) {
        blockControl.release.resolve({ value: RESULTS.blockReleased });
      }
      const activeBridge = bridge;
      const activeAgentId = agentId;
      const activeCallback = callback;
      bridge = undefined;
      agentId = undefined;
      callback = undefined;
      const cleanupSteps = [];
      const processCleanup = createProcessCleanupBoundary();
      if (signal === undefined && activeBridge !== undefined && activeAgentId !== undefined) {
        cleanupSteps.push(["close SDK agent", () => unary(
          activeBridge,
          "SdkAgentService",
          "CloseAgent",
          { agentId: activeAgentId },
          Math.min(timeoutMilliseconds, 10_000),
        )]);
      }
      if (signal === undefined && activeBridge !== undefined) {
        cleanupSteps.push(["shut down SDK Bridge", () => unary(
          activeBridge,
          "SdkBridgeControlService",
          "Shutdown",
          { graceSeconds: 1 },
          Math.min(timeoutMilliseconds, 10_000),
        )]);
      }
      for (const child of bridgeChildren) {
        cleanupSteps.push([
          "terminate SDK Bridge process tree",
          () => processCleanup.terminate(() => terminateProcessTree(child)),
        ]);
      }
      bridgeChildren.clear();
      if (activeCallback !== undefined) {
        cleanupSteps.push(["close custom-tool callback server", async () => {
          activeCallback.server.closeAllConnections?.();
          await new Promise((accept) => activeCallback.server.close(accept));
        }]);
      }
      if (signal !== undefined || !args.keepState) {
        cleanupSteps.push([
          "remove qualification state",
          () => processCleanup.remove(
            "qualification state",
            () => rm(temporaryRoot, { recursive: true, force: true }),
          ),
        ]);
      } else {
        process.stderr.write(`Kept qualification state at ${temporaryRoot}\n`);
      }
      await runCleanupSteps(cleanupSteps);
    })();
    return cleanupPromise;
  };
  const signalCleanup = installSignalCleanup(cleanup, {
    report: (error) => process.stderr.write(
      `Cursor signal cleanup failed: ${redact(error, [apiKey])}\n`,
    ),
  });
  let qualificationError;
  let qualificationResult;
  try {
    resolvedBridge = await resolveBridge(args.bridge, temporaryRoot, timeoutMilliseconds);
    const releaseAttestation = bridgeReleaseAttestation(resolvedBridge);
    const binaryStat = await stat(resolvedBridge.binary);
    operational.bridge_binary_bytes = binaryStat.size;
    callback = await startCallbackServer(handlers, timeoutMilliseconds);
    const callbackAuthClosed = await callbackBearerFailsClosed(
      callback,
      timeoutMilliseconds,
    );
    if (!callbackAuthClosed) {
      throw new QualificationError("callback bearer authentication did not fail closed");
    }

    const bridgeStartedAt = performance.now();
    bridge = await startBridge({
      binary: resolvedBridge.binary,
      workspace: args.workspace,
      stateRoot,
      apiKey,
      callback,
      timeoutMilliseconds,
      beforeSpawn: signalCleanup.throwIfSignalled,
      onSpawn: (child) => bridgeChildren.add(child),
    });
    operational.bridge_startup_ms = Math.round(performance.now() - bridgeStartedAt);
    operational.bridge_ready_rss_bytes = await processRssBytes(bridge.child.pid);

    if (!(await bridgeBearerFailsClosed(bridge, timeoutMilliseconds))) {
      throw new QualificationError("Bridge bearer authentication did not fail closed");
    }
    const ping = await unary(
      bridge,
      "SdkBridgeControlService",
      "Ping",
      {},
      timeoutMilliseconds,
    );
    if (ping.message !== "pong") throw new QualificationError("Ping did not return pong");
    const version = await unary(
      bridge,
      "SdkBridgeControlService",
      "GetVersion",
      {},
      timeoutMilliseconds,
    );
    if (version.protocolVersion !== "sdk.v1") {
      throw new QualificationError(`unexpected protocol ${version.protocolVersion}`);
    }
    const me = await unary(
      bridge,
      "SdkCursorService",
      "Me",
      { options: { apiKey } },
      timeoutMilliseconds,
    );
    if (me.user === undefined) throw new QualificationError("Me returned no user");
    const subscriptionHealth = await qualifySubscriptionHealth(
      apiKey,
      timeoutMilliseconds,
    );
    const catalog = await unary(
      bridge,
      "SdkCursorService",
      "ListModels",
      { options: { apiKey } },
      timeoutMilliseconds,
    );
    if (!Array.isArray(catalog.items) || catalog.items.length === 0) {
      throw new QualificationError("ListModels returned no models");
    }
    const model =
      args.model ??
      catalog.items.find((item) => item.id === "composer-2")?.id ??
      catalog.items[0].id;
    if (!catalog.items.some((item) => item.id === model)) {
      throw new QualificationError(`model ${model} is unavailable`);
    }

    const customTools = buildCustomTools();
    const options = fullAgentOptions(apiKey, model, args.workspace, customTools);
    const toolPolicy = await verifyToolAllowlist(
      bridge,
      options,
      timeoutMilliseconds,
    );
    process.stderr.write(
      `Running full Cursor SDK Bridge qualification with model ${model}...\n`,
    );
    const created = await unary(
      bridge,
      "SdkAgentService",
      "CreateAgent",
      { options },
      timeoutMilliseconds,
    );
    agentId = created.agentId;
    if (typeof agentId !== "string" || !agentId) {
      throw new QualificationError("CreateAgent omitted agentId");
    }

    const turns = [];
    const readTurn = await runTurn({
      bridge,
      callback,
      agentId,
      label: "allow-read-under-validated-tool-policy",
      prompt:
        `Call ${TOOLS.read} exactly once with {"token":"pre-restart-read"}, then reply ` +
        `only with the callback result's value field.`,
      expectedTools: [TOOLS.read],
      expectedText: RESULTS.preRestartRead,
      timeoutMilliseconds,
    });
    turns.push(readTurn.summary);

    const replay = await observeCompletedRun(
      bridge,
      readTurn.summary.run_id,
      timeoutMilliseconds,
    );
    const deniedTurn = await runTurn({
      bridge,
      callback,
      agentId,
      label: "host-owned-permission-denial",
      prompt:
        `Call ${TOOLS.deny} exactly once with {"operation":"write"}. Treat the ` +
        `tool's denied result as authoritative and reply only with its text content.`,
      expectedTools: [TOOLS.deny],
      expectedText: RESULTS.denied,
      timeoutMilliseconds,
      expectToolError: true,
    });
    turns.push(deniedTurn.summary);

    const imageTurn = await runTurn({
      bridge,
      callback,
      agentId,
      label: "image-input-observation-and-tool-result-images",
      prompt:
        `Inspect the attached image, then call ${TOOLS.image} exactly once with ` +
        `{"token":"<color>"}, replacing <color> with the single lowercase English ` +
        `name of its dominant pixel color. Do not call the tool if the image is unavailable. ` +
        `Accept its text, structured, and image content, then reply only with ` +
        `the structured result field.`,
      images: [
        {
          data: { data: RED_PIXEL_PNG, mimeType: "image/png" },
          dimension: { width: 1, height: 1 },
        },
      ],
      expectedTools: [TOOLS.image],
      expectedText: RESULTS.image,
      timeoutMilliseconds,
    });
    turns.push(imageTurn.summary);

    const parallelTurn = await runTurn({
      bridge,
      callback,
      agentId,
      label: "parallel-read-callbacks",
      prompt:
        `Issue ${TOOLS.parallelA} and ${TOOLS.parallelB} together in the same ` +
        `parallel tool-call batch before awaiting either. After both return, reply only ` +
        `with their value fields concatenated in tool-name order without a separator.`,
      expectedTools: [TOOLS.parallelA, TOOLS.parallelB],
      expectedText: RESULTS.parallelFinal,
      timeoutMilliseconds,
    });
    turns.push(parallelTurn.summary);

    blockControl = { started: deferred(), release: deferred() };
    const cancellation = await qualifyCancellation({
      bridge,
      callback,
      agentId,
      timeoutMilliseconds,
      blockControl,
    });
    blockControl = null;

    const recoveryTurn = await runTurn({
      bridge,
      callback,
      agentId,
      label: "post-cancellation-recovery",
      prompt:
        `Call ${TOOLS.read} exactly once with {"token":"full-read"} and reply only ` +
        `with the callback result's value field.`,
      expectedTools: [TOOLS.read],
      expectedText: RESULTS.read,
      timeoutMilliseconds,
      force: true,
    });
    turns.push(recoveryTurn.summary);

    const planTurn = await runTurn({
      bridge,
      callback,
      agentId,
      label: "plan-mode-request-observation",
      prompt:
        "In plan mode, call no tools and give a one-sentence read-only plan for inspecting a repository.",
      expectedTools: [],
      timeoutMilliseconds,
      mode: "AGENT_MODE_OPTION_PLAN",
    });
    turns.push(planTurn.summary);
    const preRestartRunId = readTurn.summary.run_id;
    if (typeof preRestartRunId !== "string" || preRestartRunId.length === 0) {
      throw new QualificationError("pre-restart turn omitted its run id");
    }

    const localUsageBeforeRestart = await getLocalUsage(
      bridge,
      agentId,
      timeoutMilliseconds,
    );
    operational.bridge_warm_rss_bytes = await processRssBytes(bridge.child.pid);

    await unary(
      bridge,
      "SdkAgentService",
      "CloseAgent",
      { agentId },
      timeoutMilliseconds,
    );
    await unary(
      bridge,
      "SdkBridgeControlService",
      "Shutdown",
      { graceSeconds: 1 },
      Math.min(timeoutMilliseconds, 10_000),
    );
    await terminateProcessTree(bridge.child);
    bridgeChildren.delete(bridge.child);
    bridge = undefined;

    const restartStartedAt = performance.now();
    bridge = await startBridge({
      binary: resolvedBridge.binary,
      workspace: args.workspace,
      stateRoot,
      apiKey,
      callback,
      timeoutMilliseconds,
      beforeSpawn: signalCleanup.throwIfSignalled,
      onSpawn: (child) => bridgeChildren.add(child),
    });
    operational.bridge_cold_restart_ms = Math.round(performance.now() - restartStartedAt);
    const resumed = await unary(
      bridge,
      "SdkAgentService",
      "ResumeAgent",
      { agentId, options },
      timeoutMilliseconds,
    );
    if (resumed.agentId !== agentId) {
      throw new QualificationError("cold ResumeAgent returned a different agent id");
    }
    const coldReplay = await observeCompletedRun(
      bridge,
      preRestartRunId,
      timeoutMilliseconds,
    );
    const coldTurn = await runTurn({
      bridge,
      callback,
      agentId,
      label: "cold-bridge-resume",
      prompt:
        `Call ${TOOLS.read} exactly once with {"token":"cold-resume-read"} and reply only ` +
        `with the callback result's value field.`,
      expectedTools: [TOOLS.read],
      expectedText: RESULTS.coldResumeRead,
      timeoutMilliseconds,
    });
    turns.push(coldTurn.summary);

    const getRun = await unary(
      bridge,
      "SdkAgentService",
      "GetRun",
      { runId: coldTurn.summary.run_id, options: { agentId } },
      timeoutMilliseconds,
    );
    if (!terminalStatusIsFinished(getRun.run?.status)) {
      throw new QualificationError("GetRun did not return the cold resumed run");
    }
    const historicalRun = await unary(
      bridge,
      "SdkAgentService",
      "GetRun",
      { runId: preRestartRunId, options: { agentId } },
      timeoutMilliseconds,
    );
    if (!terminalStatusIsFinished(historicalRun.run?.status)) {
      throw new QualificationError("cold restart lost the pre-restart run");
    }
    const listedRuns = await unary(
      bridge,
      "SdkAgentService",
      "ListRuns",
      { agentId, options: { limit: 20 } },
      timeoutMilliseconds,
    );
    if (
      !Array.isArray(listedRuns.items) ||
      !listedRuns.items.some((run) => run.runId === coldTurn.summary.run_id) ||
      !listedRuns.items.some((run) => run.runId === preRestartRunId)
    ) {
      throw new QualificationError("ListRuns omitted cold or pre-restart history");
    }
    const conversation = await unary(
      bridge,
      "SdkAgentService",
      "GetRunConversation",
      { runId: coldTurn.summary.run_id },
      timeoutMilliseconds,
    );
    parseConversationEvidence(
      conversation.conversationJson,
      "GetRunConversation for cold resumed run",
      RESULTS.coldResumeRead,
    );
    const historicalConversation = await unary(
      bridge,
      "SdkAgentService",
      "GetRunConversation",
      { runId: preRestartRunId },
      timeoutMilliseconds,
    );
    parseConversationEvidence(
      historicalConversation.conversationJson,
      "GetRunConversation for pre-restart run",
      RESULTS.preRestartRead,
    );
    const agentMessages = await unary(
      bridge,
      "SdkAgentService",
      "ListAgentMessages",
      { agentId, options: { limit: 20, offset: 0 } },
      timeoutMilliseconds,
    );
    if (
      !Array.isArray(agentMessages.messages) ||
      agentMessages.messages.length === 0 ||
      !agentMessagesContainEvidence(agentMessages.messages, RESULTS.preRestartRead) ||
      !agentMessagesContainEvidence(agentMessages.messages, RESULTS.coldResumeRead)
    ) {
      throw new QualificationError("ListAgentMessages omitted durable pre- or post-restart evidence");
    }

    const localUsageAfterRestart = await getLocalUsage(
      bridge,
      agentId,
      timeoutMilliseconds,
    );
    const usageTurns = turns.filter((turn) =>
      Object.values(turn.usage ?? {}).some((value) => numberValue(value) > 0),
    ).length;
    const effectiveToolLists = turns.filter((turn) =>
      turn.message_types.includes("system"),
    ).length;
    const blockers = [];
    if (maxActiveReads < 2) {
      blockers.push("the model/runtime serialized the explicitly parallel read callbacks");
    }
    if (usageTurns !== turns.length) {
      blockers.push(
        `only ${usageTurns}/${turns.length} completed turns exposed token usage`,
      );
    }
    if (!deniedTurn.summary.terminal_stream_events_complete) {
      blockers.push(
        "a host-denied custom tool had no terminal tool_call stream event; callback and final-run evidence remain authoritative",
      );
    }

    qualificationResult = {
      candidate: "cursor-sdk-bridge",
      result: blockers.length === 0 ? "pass" : "qualification-complete-with-blockers",
      decision:
        blockers.length === 0
          ? "proceed-with-sdk-bridge-adapter"
          : "hold-sdk-bridge-promotion",
      release: {
        archive_version: releaseAttestation.pinnedRelease,
        source: releaseAttestation.source,
        bridge_version: version.bridgeVersion,
        protocol_version: version.protocolVersion,
        capabilities: version.capabilities ?? [],
        model,
        model_catalog_entries: catalog.items.length,
      },
      authentication: {
        api_key: "passed",
        bridge_bearer_fails_closed: true,
        callback_bearer_fails_closed: true,
        account_identity_not_recorded: true,
      },
      tools_and_permissions: {
        registered_custom_tools: Object.keys(customTools).length,
        synthetic_schema_probes: SCHEMA_PROBE_COUNT,
        confinement: "sdk-tool-allowlist-contract",
        tool_policy_validation: toolPolicy,
        cursor_native_sandbox: false,
        host_allow_result: "passed",
        host_denied_is_error_result: "passed",
        host_denied_terminal_stream_event:
          deniedTurn.summary.terminal_stream_events_complete,
        input_image_request_completed: true,
        tool_result_text_structured_image_callback_completed: true,
        image_payload_consumption_claimed: false,
        parallel_read_callbacks: maxActiveReads >= 2,
        max_parallel_read_callbacks: maxActiveReads,
      },
      lifecycle: {
        close_resume: true,
        cold_bridge_process_resume: true,
        pre_restart_history_after_cold_resume: true,
        durable_observe_replay: replay,
        durable_observe_replay_after_cold_restart: coldReplay,
        cancellation,
        post_cancellation_recovery: true,
        get_run: true,
        list_runs: true,
        get_run_conversation: true,
        list_agent_messages: true,
        steering: "disabled-by-cursor-backend-capability",
      },
      non_gating_observations: {
        plan_mode_request: {
          requested: planTurn.summary.requested_mode === "AGENT_MODE_OPTION_PLAN",
          qualification_status: "not-attested",
          authoritative_effective_mode_evidence: "unavailable-from-sdk.v1",
          semantic_attestation: false,
          safety_boundary: "trouve-tool-executor",
        },
      },
      usage: {
        turns_with_usage: usageTurns,
        completed_turns: turns.length,
        before_restart: localUsageBeforeRestart,
        after_restart: localUsageAfterRestart,
        subscription_health: subscriptionHealth,
      },
      streaming: {
        turns,
        system_messages_seen: effectiveToolLists,
        unknown_message_types_tolerated: true,
      },
      operations: {
        ...operational,
        isolated_state_removed: !args.keepState,
      },
      blockers,
    };
  } catch (error) {
    qualificationError = error;
  } finally {
    try {
      await cleanup();
    } catch (cleanupError) {
      qualificationError = combineQualificationAndCleanupErrors(
        qualificationError,
        cleanupError,
      );
    } finally {
      signalCleanup.dispose();
    }
  }
  if (qualificationError !== undefined) throw qualificationError;
  return qualificationResult;
}

export function qualificationExitCode(result) {
  return result?.decision === "hold-sdk-bridge-promotion" ? 1 : 0;
}

if (resolve(process.argv[1] ?? "") === fileURLToPath(import.meta.url)) {
  try {
    const args = parseArgs(process.argv.slice(2));
    const result = args.healthOnly
      ? {
          candidate: "cursor-subscription-health",
          ...(await qualifySubscriptionHealth(
            process.env.CURSOR_API_KEY ?? "",
            args.timeoutSeconds * 1000,
          )),
        }
      : await fullQualification(args);
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
    process.exitCode = qualificationExitCode(result);
  } catch (error) {
    const message = redact(
      error instanceof Error ? error.message : error,
      [process.env.CURSOR_API_KEY],
    );
    process.stderr.write(`Cursor qualification failed: ${message}\n`);
    process.exitCode = 1;
  }
}
