#!/usr/bin/env node
/**
 * Live qualification for sharing one Cursor SDK Bridge across Trouve threads.
 *
 * The pinned Bridge exposes one process-wide custom-tool callback endpoint. This
 * probe proves that one process can safely host two concurrent local agents when
 * callbacks are routed by the exact owning agent id. Production-adapter route
 * settlement and quarantine are covered by the Rust adapter tests. This exercises
 * per-agent workspaces and tool catalogs, concurrent sends, cancellation
 * isolation, warm close/resume, and cold resume after a Bridge restart.
 *
 * The probe performs six paid local SDK turns. It never prints account identity
 * or CURSOR_API_KEY and removes all temporary state unless --keep-state is set.
 */

import { constants as fsConstants } from "node:fs";
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  BRIDGE_VERSION,
  CURSOR_NATIVE_TOOL_DENYLIST,
  QualificationError,
  assistantText,
  assertUniqueToolLifecycle,
  exactTerminalResult,
  parseTimeoutSeconds,
  redact,
  resolveBridge,
  sdkMessages,
  send,
  startBridge,
  terminalStatusIsFinished,
  terminateProcessTree,
  unary,
} from "./qualify_cursor_sdk_bridge.mjs";
import { startCallbackServer } from "./qualify_cursor_sdk_bridge_full.mjs";

const REPOSITORY_ROOT = fileURLToPath(new URL("../", import.meta.url));
const SLOT_A = "a";
const SLOT_B = "b";
const TOOLS = Object.freeze({
  initialA: "trouve_shared_initial_a",
  initialB: "trouve_shared_initial_b",
  cancelA: "trouve_shared_cancel_a",
  surviveB: "trouve_shared_survive_b",
  resumeA: "trouve_shared_resume_a",
  resumeB: "trouve_shared_resume_b",
});
const RESULTS = Object.freeze({
  initialA: "SHARED_INITIAL_A_OK",
  initialB: "SHARED_INITIAL_B_OK",
  surviveB: "SHARED_CANCEL_ISOLATION_B_OK",
  resumeA: "SHARED_COLD_RESUME_A_OK",
  resumeB: "SHARED_COLD_RESUME_B_OK",
});

const help = `Usage: node scripts/qualify_cursor_sdk_bridge_shared.mjs [options]

Requires CURSOR_API_KEY. Downloads the pinned Cursor SDK Bridge v${BRIDGE_VERSION}
unless --bridge is supplied, then performs six billable local SDK turns.

Options:
  --bridge PATH     Use an existing cursor-sdk-bridge binary
  --model ID        Cursor model id (default: composer-2 when available)
  --timeout SECONDS Timeout for each operation/turn (default: 300)
  --keep-state      Keep the isolated temporary directory for inspection
  --help            Show this help
`;

function parseArgs(argv) {
  const parsed = {
    bridge: process.env.CURSOR_SDK_BRIDGE_BIN,
    model: process.env.CURSOR_QUALIFICATION_MODEL,
    timeoutSeconds: 300,
    keepState: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--help") {
      process.stdout.write(help);
      process.exit(0);
    } else if (argument === "--keep-state") {
      parsed.keepState = true;
    } else if (argument === "--bridge" || argument === "--model" || argument === "--timeout") {
      const value = argv[index + 1];
      if (value === undefined) throw new QualificationError(`${argument} requires a value`);
      index += 1;
      if (argument === "--bridge") parsed.bridge = resolve(value);
      if (argument === "--model") parsed.model = value;
      if (argument === "--timeout") parsed.timeoutSeconds = parseTimeoutSeconds(value);
    } else {
      throw new QualificationError(`unknown argument: ${argument}`);
    }
  }
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

function objectSchema(properties = {}, required = []) {
  return {
    type: "object",
    properties,
    required,
    additionalProperties: false,
  };
}

function customTools(names) {
  return Object.fromEntries(names.map((name) => [
    name,
    {
      description: `Shared-Bridge qualification tool ${name}.`,
      inputSchema: objectSchema({ token: { type: "string" } }, ["token"]),
    },
  ]));
}

function agentOptions(apiKey, model, workspace, names, slot) {
  return {
    model: { id: model },
    apiKey,
    name: `Trouve shared Bridge qualification ${slot}`,
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
      customTools: customTools(names),
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

function statusIsCancelled(status) {
  return (
    status === 5 ||
    status === "5" ||
    status === "RUN_LIFECYCLE_STATUS_CANCELLED"
  );
}

function callbackFor(callback, toolName) {
  const calls = callback.calls.filter((call) => call.toolName === toolName);
  if (calls.length !== 1) {
    throw new QualificationError(
      `${toolName}: expected exactly one callback, observed ${calls.length}`,
    );
  }
  return calls[0];
}

function assertFinishedTurn(frames, callback, agentId, toolName, resultMarker, label) {
  const terminal = exactTerminalResult(frames, label);
  if (!terminalStatusIsFinished(terminal.status)) {
    throw new QualificationError(`${label} ended with non-finished status ${terminal.status}`);
  }
  const call = callbackFor(callback, toolName);
  // startCallbackServer intentionally retains callback identity/lifecycle but
  // not argument payloads. The route-specific handler above already checked
  // the exact token before returning a result; corroborate the retained agent
  // identity here without pretending the summary record contains arguments.
  if (call.agentId !== agentId) {
    throw new QualificationError(`${label}: callback identity crossed agents`);
  }
  if (typeof call.toolCallId !== "string" || call.toolCallId.length === 0) {
    throw new QualificationError(`${label}: callback omitted its tool-call id`);
  }
  const messages = sdkMessages(frames);
  const lifecycle = messages.filter((message) => message.type === "tool_call");
  assertUniqueToolLifecycle(lifecycle, label);
  const streamIds = new Set(lifecycle.map((message) => message.call_id).filter(Boolean));
  if (!streamIds.has(call.toolCallId)) {
    throw new QualificationError(`${label}: callback and stream call ids did not correlate`);
  }
  const streamNames = new Set(lifecycle.map((message) => message.name).filter(Boolean));
  if ([...streamNames].some((name) => name !== "mcp" && name !== toolName)) {
    throw new QualificationError(`${label}: a native or cross-agent tool appeared in the stream`);
  }
  const finalText = String(terminal.result?.result ?? assistantText(messages));
  if (!finalText.includes(resultMarker)) {
    throw new QualificationError(`${label}: assistant did not use ${resultMarker}`);
  }
  return {
    run_id: terminal.runId ?? terminal.result?.runId,
    callback_tool: toolName,
    callback_agent_id_matched: true,
    callback_stream_id_correlated: true,
    native_tools_present: false,
  };
}

async function runToolTurn(bridge, agentId, toolName, resultMarker, timeoutMilliseconds) {
  return send(
    bridge,
    {
      agentId,
      message: {
        text:
          `Call ${toolName} exactly once with {"token":"${toolName}"}. ` +
          `Do not call any other tool. After it succeeds, reply with exactly ${resultMarker}.`,
      },
      options: { enableDeltas: true, enableSteps: true },
    },
    timeoutMilliseconds,
  );
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

async function closeCallback(callback) {
  if (callback === undefined) return;
  callback.server.closeAllConnections?.();
  await new Promise((accept) => callback.server.close(accept));
}

async function stopBridge(bridge, timeoutMilliseconds) {
  if (bridge === undefined) return;
  try {
    await unary(
      bridge,
      "SdkBridgeControlService",
      "Shutdown",
      { graceSeconds: 1 },
      Math.min(timeoutMilliseconds, 10_000),
    );
  } finally {
    await terminateProcessTree(bridge.child);
  }
}

async function createAgents(bridge, optionsA, optionsB, timeoutMilliseconds) {
  const [createdA, createdB] = await Promise.all([
    unary(
      bridge,
      "SdkAgentService",
      "CreateAgent",
      { options: optionsA },
      timeoutMilliseconds,
    ),
    unary(
      bridge,
      "SdkAgentService",
      "CreateAgent",
      { options: optionsB },
      timeoutMilliseconds,
    ),
  ]);
  for (const [slot, created] of [[SLOT_A, createdA], [SLOT_B, createdB]]) {
    if (typeof created.agentId !== "string" || created.agentId.length === 0) {
      throw new QualificationError(`CreateAgent ${slot} omitted agentId`);
    }
  }
  if (createdA.agentId === createdB.agentId) {
    throw new QualificationError("two concurrent CreateAgent calls returned the same agentId");
  }
  return { a: createdA.agentId, b: createdB.agentId };
}

async function resumeAgents(bridge, agents, optionsA, optionsB, timeoutMilliseconds) {
  const [resumedA, resumedB] = await Promise.all([
    unary(
      bridge,
      "SdkAgentService",
      "ResumeAgent",
      { agentId: agents.a, options: optionsA },
      timeoutMilliseconds,
    ),
    unary(
      bridge,
      "SdkAgentService",
      "ResumeAgent",
      { agentId: agents.b, options: optionsB },
      timeoutMilliseconds,
    ),
  ]);
  if (resumedA.agentId !== agents.a || resumedB.agentId !== agents.b) {
    throw new QualificationError("ResumeAgent changed an agent id in the shared store");
  }
}

async function closeAgents(bridge, agents, timeoutMilliseconds) {
  await Promise.all([
    unary(
      bridge,
      "SdkAgentService",
      "CloseAgent",
      { agentId: agents.a },
      Math.min(timeoutMilliseconds, 10_000),
    ),
    unary(
      bridge,
      "SdkAgentService",
      "CloseAgent",
      { agentId: agents.b },
      Math.min(timeoutMilliseconds, 10_000),
    ),
  ]);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const apiKey = process.env.CURSOR_API_KEY;
  if (!apiKey) {
    throw new QualificationError(
      "CURSOR_API_KEY is required for the shared-process qualification",
    );
  }
  const timeoutMilliseconds = args.timeoutSeconds * 1_000;
  const temporaryRoot = await mkdtemp(join(tmpdir(), "trouve-cursor-shared-"));
  const stateRoot = join(temporaryRoot, "state");
  const workspaceA = join(temporaryRoot, "workspace-a");
  const workspaceB = join(temporaryRoot, "workspace-b");
  await Promise.all([
    mkdir(stateRoot),
    mkdir(workspaceA),
    mkdir(workspaceB),
  ]);
  await Promise.all([
    writeFile(join(workspaceA, "WORKSPACE.txt"), "workspace-a\n"),
    writeFile(join(workspaceB, "WORKSPACE.txt"), "workspace-b\n"),
  ]);
  await access(REPOSITORY_ROOT, fsConstants.R_OK);

  const expectedAgents = new Map();
  const cancellationAStarted = deferred();
  const cancelledProbeTeardown = deferred();
  const survivalBStarted = deferred();
  const survivalBRelease = deferred();
  let bridge;
  let callback;
  let cleanupPromise;
  const cleanup = () => {
    if (cleanupPromise !== undefined) return cleanupPromise;
    cleanupPromise = (async () => {
      cancelledProbeTeardown.resolve();
      survivalBRelease.resolve();
      const activeBridge = bridge;
      const activeCallback = callback;
      bridge = undefined;
      callback = undefined;
      const failures = [];
      try {
        await stopBridge(activeBridge, timeoutMilliseconds);
      } catch (error) {
        failures.push(error);
      }
      try {
        await closeCallback(activeCallback);
      } catch (error) {
        failures.push(error);
      }
      if (!args.keepState) {
        try {
          await rm(temporaryRoot, { recursive: true, force: true });
        } catch (error) {
          failures.push(error);
        }
      }
      if (failures.length > 0) {
        throw new AggregateError(failures, "shared-process qualification cleanup failed");
      }
    })();
    return cleanupPromise;
  };

  const handler = (slot, toolName, resultMarker) => async (argsValue, record) => {
    if (record.agentId !== expectedAgents.get(slot) || argsValue.token !== toolName) {
      throw new QualificationError(`${toolName}: callback crossed its agent route`);
    }
    return { value: resultMarker };
  };
  const handlers = new Map([
    [TOOLS.initialA, handler(SLOT_A, TOOLS.initialA, RESULTS.initialA)],
    [TOOLS.initialB, handler(SLOT_B, TOOLS.initialB, RESULTS.initialB)],
    [TOOLS.resumeA, handler(SLOT_A, TOOLS.resumeA, RESULTS.resumeA)],
    [TOOLS.resumeB, handler(SLOT_B, TOOLS.resumeB, RESULTS.resumeB)],
    [TOOLS.cancelA, async (argsValue, record) => {
      if (record.agentId !== expectedAgents.get(SLOT_A) || argsValue.token !== TOOLS.cancelA) {
        throw new QualificationError("cancel callback crossed its agent route");
      }
      cancellationAStarted.resolve(record);
      // Hold this direct-Bridge callback through cancellation so the probe can
      // observe Cursor's transport behavior. The production adapter's route
      // supervisor is exercised separately; this handler is released only for
      // probe teardown after cancellation isolation has already been proven.
      await cancelledProbeTeardown.promise;
      return { value: "cancelled" };
    }],
    [TOOLS.surviveB, async (argsValue, record) => {
      if (record.agentId !== expectedAgents.get(SLOT_B) || argsValue.token !== TOOLS.surviveB) {
        throw new QualificationError("survival callback crossed its agent route");
      }
      survivalBStarted.resolve(record);
      await survivalBRelease.promise;
      return { value: RESULTS.surviveB };
    }],
  ]);

  try {
    const resolvedBridge = await resolveBridge(args.bridge, temporaryRoot, timeoutMilliseconds);
    callback = await startCallbackServer(handlers, timeoutMilliseconds);
    bridge = await startBridge({
      binary: resolvedBridge.binary,
      workspace: workspaceA,
      stateRoot,
      apiKey,
      callback,
      timeoutMilliseconds,
    });
    const version = await unary(
      bridge,
      "SdkBridgeControlService",
      "GetVersion",
      {},
      timeoutMilliseconds,
    );
    if (version.protocolVersion !== "sdk.v1") {
      throw new QualificationError(`unsupported Cursor SDK protocol: ${version.protocolVersion}`);
    }
    const catalog = await unary(
      bridge,
      "SdkCursorService",
      "ListModels",
      { options: { apiKey } },
      timeoutMilliseconds,
    );
    if (!Array.isArray(catalog.items) || catalog.items.length === 0) {
      throw new QualificationError("Cursor ListModels returned no models");
    }
    const model =
      args.model ??
      catalog.items.find((item) => item.id === "composer-2")?.id ??
      catalog.items[0].id;
    if (!catalog.items.some((item) => item.id === model)) {
      throw new QualificationError(`model ${model} is unavailable`);
    }
    process.stderr.write(
      `Running six paid shared Cursor SDK Bridge qualification turns with model ${model}...\n`,
    );

    const toolsA = [TOOLS.initialA, TOOLS.cancelA, TOOLS.resumeA];
    const toolsB = [TOOLS.initialB, TOOLS.surviveB, TOOLS.resumeB];
    const optionsA = agentOptions(apiKey, model, workspaceA, toolsA, SLOT_A);
    const optionsB = agentOptions(apiKey, model, workspaceB, toolsB, SLOT_B);
    const agents = await createAgents(bridge, optionsA, optionsB, timeoutMilliseconds);
    expectedAgents.set(SLOT_A, agents.a);
    expectedAgents.set(SLOT_B, agents.b);

    const [initialFramesA, initialFramesB] = await Promise.all([
      runToolTurn(bridge, agents.a, TOOLS.initialA, RESULTS.initialA, timeoutMilliseconds),
      runToolTurn(bridge, agents.b, TOOLS.initialB, RESULTS.initialB, timeoutMilliseconds),
    ]);
    const initial = {
      a: assertFinishedTurn(
        initialFramesA,
        callback,
        agents.a,
        TOOLS.initialA,
        RESULTS.initialA,
        "shared initial A",
      ),
      b: assertFinishedTurn(
        initialFramesB,
        callback,
        agents.b,
        TOOLS.initialB,
        RESULTS.initialB,
        "shared initial B",
      ),
    };
    await closeAgents(bridge, agents, timeoutMilliseconds);
    await resumeAgents(bridge, agents, optionsA, optionsB, timeoutMilliseconds);

    const cancelRunA = deferred();
    const cancelRunB = deferred();
    const cancelledSend = send(
      bridge,
      {
        agentId: agents.a,
        message: {
          text:
            `Call ${TOOLS.cancelA} exactly once with {"token":"${TOOLS.cancelA}"}, ` +
            "then wait for its result.",
        },
        options: { enableDeltas: true, enableSteps: true },
      },
      timeoutMilliseconds,
      (frame) => {
        const runId = messageRunId(frame);
        if (runId !== undefined) cancelRunA.resolve(runId);
      },
    ).then(
      (frames) => ({ frames, error: null }),
      (error) => ({ frames: null, error }),
    );
    const survivingSend = send(
      bridge,
      {
        agentId: agents.b,
        message: {
          text:
            `Call ${TOOLS.surviveB} exactly once with {"token":"${TOOLS.surviveB}"}. ` +
            `After it succeeds, reply with exactly ${RESULTS.surviveB}.`,
        },
        options: { enableDeltas: true, enableSteps: true },
      },
      timeoutMilliseconds,
      (frame) => {
        const runId = messageRunId(frame);
        if (runId !== undefined) cancelRunB.resolve(runId);
      },
    ).then(
      (frames) => ({ frames, error: null }),
      (error) => ({ frames: null, error }),
    );
    const [runIdA, runIdB, cancelledRecord, survivingRecord] = await withTimeout(
      Promise.all([
        cancelRunA.promise,
        cancelRunB.promise,
        cancellationAStarted.promise,
        survivalBStarted.promise,
      ]),
      Math.min(timeoutMilliseconds, 60_000),
      "parallel cancellation setup",
    );
    await unary(
      bridge,
      "SdkAgentService",
      "CancelRun",
      { runId: runIdA, agentId: agents.a },
      timeoutMilliseconds,
    );
    survivalBRelease.resolve();
    const [cancelledOutcome, survivingOutcome] = await Promise.all([
      cancelledSend,
      survivingSend,
    ]);
    await survivingRecord.settled.promise;
    if (cancelledOutcome.error !== null) throw cancelledOutcome.error;
    if (survivingOutcome.error !== null) throw survivingOutcome.error;
    const cancelledFrames = cancelledOutcome.frames;
    const survivingFrames = survivingOutcome.frames;
    const cancelledTerminal = exactTerminalResult(cancelledFrames, "shared cancelled A");
    if (!statusIsCancelled(cancelledTerminal.status)) {
      throw new QualificationError(
        `cancelled agent ended with non-cancelled status ${cancelledTerminal.status}`,
      );
    }
    const survived = assertFinishedTurn(
      survivingFrames,
      callback,
      agents.b,
      TOOLS.surviveB,
      RESULTS.surviveB,
      "shared surviving B",
    );
    if (callbackFor(callback, TOOLS.cancelA).agentId !== agents.a) {
      throw new QualificationError("cancelled callback crossed its agent route");
    }
    let cancelledCallbackSettledBeforeTeardown = false;
    void cancelledRecord.settled.promise.then(() => {
      cancelledCallbackSettledBeforeTeardown = true;
    });
    await Promise.resolve();
    const cancellation = {
      cancelled_run_id: runIdA,
      surviving_run_id: runIdB,
      cancelled_callback_settled_by_bridge_before_probe_teardown:
        cancelledCallbackSettledBeforeTeardown,
      bridge_disconnected_cancelled_callback: cancelledRecord.cancelledAtMs !== null,
      surviving_callback_completed: survivingRecord.ok,
      surviving_turn: survived,
      adapter_route_settlement_covered_by:
        "cursor_adapter_cancellation_settles_route_and_keeps_shared_bridge_usable",
    };
    if (!cancellation.surviving_callback_completed) {
      throw new QualificationError("parallel cancellation did not isolate the surviving agent");
    }
    cancelledProbeTeardown.resolve();
    await cancelledRecord.settled.promise;

    await closeAgents(bridge, agents, timeoutMilliseconds);
    const warmRssBytes = await processRssBytes(bridge.child.pid);
    await stopBridge(bridge, timeoutMilliseconds);
    bridge = undefined;
    await closeCallback(callback);
    callback = undefined;

    callback = await startCallbackServer(handlers, timeoutMilliseconds);
    bridge = await startBridge({
      binary: resolvedBridge.binary,
      workspace: workspaceB,
      stateRoot,
      apiKey,
      callback,
      timeoutMilliseconds,
    });
    await resumeAgents(bridge, agents, optionsA, optionsB, timeoutMilliseconds);
    const [resumeFramesA, resumeFramesB] = await Promise.all([
      runToolTurn(bridge, agents.a, TOOLS.resumeA, RESULTS.resumeA, timeoutMilliseconds),
      runToolTurn(bridge, agents.b, TOOLS.resumeB, RESULTS.resumeB, timeoutMilliseconds),
    ]);
    const coldResume = {
      a: assertFinishedTurn(
        resumeFramesA,
        callback,
        agents.a,
        TOOLS.resumeA,
        RESULTS.resumeA,
        "shared cold resume A",
      ),
      b: assertFinishedTurn(
        resumeFramesB,
        callback,
        agents.b,
        TOOLS.resumeB,
        RESULTS.resumeB,
        "shared cold resume B",
      ),
    };
    await closeAgents(bridge, agents, timeoutMilliseconds);

    const result = {
      candidate: "cursor-sdk-bridge-shared-process",
      result: "pass",
      pinned_release: BRIDGE_VERSION,
      bridge_version: version.bridgeVersion,
      protocol_version: version.protocolVersion,
      model,
      process_boundary: "one-per-cursor-backend",
      maximum_concurrent_bridge_processes: 1,
      shared_sqlite_store: true,
      per_agent_api_key: true,
      per_agent_workspace: true,
      per_agent_tool_catalog: true,
      callback_route_key: "agent_id",
      concurrent_sends: true,
      cancellation_isolated: true,
      warm_close_resume: true,
      cold_process_resume: true,
      native_tools_present: false,
      warm_bridge_rss_bytes: warmRssBytes,
      initial,
      cancellation,
      cold_resume: coldResume,
    };
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  } finally {
    await cleanup();
    if (args.keepState) {
      process.stderr.write(`Kept shared-process qualification state at ${temporaryRoot}\n`);
    }
  }
}

if (resolve(process.argv[1] ?? "") === fileURLToPath(import.meta.url)) {
  try {
    await main();
  } catch (error) {
    const message = redact(error instanceof Error ? error.message : error, [
      process.env.CURSOR_API_KEY,
    ]);
    process.stderr.write(`Shared Cursor SDK Bridge qualification failed: ${message}\n`);
    process.exitCode = 1;
  }
}
