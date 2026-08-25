#!/usr/bin/env node
/**
 * Live qualification probe for Cursor's SDK Bridge.
 *
 * The probe reads CURSOR_API_KEY from the environment, downloads and verifies
 * the pinned standalone bridge unless a binary is supplied, and uses only
 * Node's standard library. It creates an isolated local agent whose only
 * allowed capability is one host-owned custom tool, exercises that callback
 * exactly once, closes/resumes the agent with the restriction re-applied, and
 * exercises it once more. Temporary bridge state is removed on exit.
 */

import { spawn } from "node:child_process";
import {
  createHash,
  randomBytes,
  timingSafeEqual,
} from "node:crypto";
import {
  constants as fsConstants,
  createReadStream,
  createWriteStream,
} from "node:fs";
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  rm,
} from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

const REPOSITORY_ROOT = fileURLToPath(new URL("../", import.meta.url));
const BRIDGE_VERSION = "1.0.28";
const RELEASE_ROOT = `https://github.com/cursor/sdk-bridge/releases/download/v${BRIDGE_VERSION}`;
const TOOL_NAME = "trouve_qualification_echo";
const TOOL_ARGUMENT = "cursor-sdk-bridge-tool-ok";
const TOOL_RESULT = "TROUVE_CURSOR_SDK_BRIDGE_OK";
const MAX_HTTP_BODY_BYTES = 4 * 1024 * 1024;
const MAX_CONNECT_FRAME_BYTES = 64 * 1024 * 1024;
const READY_PREFIX = "cursor-sdk-bridge ready ";
const FORBIDDEN_BUILT_INS = new Set([
  "browser",
  "delete",
  "edit",
  "glob",
  "grep",
  "ls",
  "read",
  "shell",
  "task",
  "terminal",
  "web",
  "write",
]);

class QualificationError extends Error {}

const help = `Usage: node scripts/qualify_cursor_sdk_bridge.mjs [options]

Requires CURSOR_API_KEY. The default run downloads and verifies Cursor SDK
Bridge v${BRIDGE_VERSION}, then performs two billable local SDK turns.

Options:
  --bridge PATH     Use an existing cursor-sdk-bridge binary
  --workspace PATH  Workspace shown to the sandboxed agent (default: repo root)
  --model ID        Cursor model id (default: composer-2 when available)
  --timeout SECONDS Timeout for startup and each RPC/turn (default: 300)
  --keep-download   Keep the temporary verified bridge download for inspection
  --help            Show this help
`;

function parseArgs(argv) {
  const parsed = {
    bridge: process.env.CURSOR_SDK_BRIDGE_BIN,
    workspace: REPOSITORY_ROOT,
    model: process.env.CURSOR_QUALIFICATION_MODEL,
    timeoutSeconds: 300,
    keepDownload: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--help") {
      process.stdout.write(help);
      process.exit(0);
    }
    if (argument === "--keep-download") {
      parsed.keepDownload = true;
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
  parsed.timeoutSeconds = Number(parsed.timeoutSeconds);
  if (!Number.isFinite(parsed.timeoutSeconds) || parsed.timeoutSeconds <= 0) {
    throw new QualificationError("--timeout must be a positive number");
  }
  parsed.workspace = resolve(parsed.workspace);
  if (parsed.bridge !== undefined) parsed.bridge = resolve(parsed.bridge);
  return parsed;
}

function assetName() {
  const operatingSystem = {
    linux: "linux",
    darwin: "darwin",
    win32: "win32",
  }[process.platform];
  const architecture = {
    x64: "x64",
    arm64: "arm64",
  }[process.arch];
  if (operatingSystem === undefined || architecture === undefined) {
    throw new QualificationError(
      `unsupported Cursor SDK Bridge platform: ${process.platform}/${process.arch}`,
    );
  }
  return `cursor-sdk-bridge-standalone-${operatingSystem}-${architecture}.tar.gz`;
}

async function download(url, destination) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok || response.body === null) {
    throw new QualificationError(`download failed (${response.status}) for ${url}`);
  }
  await pipeline(Readable.fromWeb(response.body), createWriteStream(destination));
}

async function sha256(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

async function runChild(command, args, timeoutMilliseconds) {
  return new Promise((accept, reject) => {
    const child = spawn(command, args, {
      stdio: ["ignore", "ignore", "pipe"],
    });
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => {
      stderr = `${stderr}${chunk}`.slice(-16_384);
    });
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      reject(new QualificationError(`${command} timed out`));
    }, timeoutMilliseconds);
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("exit", (code, signal) => {
      clearTimeout(timer);
      if (code === 0) accept();
      else {
        reject(
          new QualificationError(
            `${command} failed (status ${code}, signal ${signal}): ${stderr.trim()}`,
          ),
        );
      }
    });
  });
}

async function resolveBridge(explicit, temporaryRoot, timeoutMilliseconds) {
  if (explicit !== undefined) {
    await access(explicit, fsConstants.X_OK);
    return { binary: explicit, downloaded: false };
  }

  const asset = assetName();
  const archive = join(temporaryRoot, asset);
  const sums = join(temporaryRoot, "SHA256SUMS.txt");
  process.stderr.write(
    `Downloading and verifying Cursor SDK Bridge v${BRIDGE_VERSION} (${asset})...\n`,
  );
  await Promise.all([
    download(`${RELEASE_ROOT}/${asset}`, archive),
    download(`${RELEASE_ROOT}/SHA256SUMS.txt`, sums),
  ]);
  const checksumLines = (await readFile(sums, "utf8")).split(/\r?\n/u);
  let expected;
  for (const line of checksumLines) {
    const match = /^([a-fA-F0-9]{64})\s+\*?(.+)$/u.exec(line.trim());
    if (match !== null && basename(match[2]) === asset) {
      expected = match[1].toLowerCase();
      break;
    }
  }
  if (expected === undefined) {
    throw new QualificationError(`SHA256SUMS.txt did not contain ${asset}`);
  }
  const actual = await sha256(archive);
  if (actual !== expected) {
    throw new QualificationError(
      `checksum mismatch for ${asset}: expected ${expected}, got ${actual}`,
    );
  }

  const extracted = join(temporaryRoot, "bridge");
  await mkdir(extracted);
  await runChild("tar", ["-xzf", archive, "-C", extracted], timeoutMilliseconds);
  const executable = process.platform === "win32" ? "cursor-sdk-bridge.exe" : "cursor-sdk-bridge";
  const binary = join(extracted, "bin", executable);
  await access(binary, fsConstants.X_OK);
  return { binary, downloaded: true };
}

function secureBearerMatches(header, token) {
  const presented = Buffer.from(header ?? "", "utf8");
  const expected = Buffer.from(`Bearer ${token}`, "utf8");
  return presented.length === expected.length && timingSafeEqual(presented, expected);
}

async function readHttpBody(request) {
  const chunks = [];
  let length = 0;
  for await (const chunk of request) {
    length += chunk.length;
    if (length > MAX_HTTP_BODY_BYTES) {
      throw new QualificationError("custom-tool callback body exceeded the probe limit");
    }
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString("utf8");
}

async function startToolCallbackServer() {
  const bearer = randomBytes(32).toString("base64url");
  const calls = [];
  const errors = [];
  const callbackPath = "/sdk.v1.SdkCustomToolCallbackService/CallCustomTool";
  const server = createServer(async (request, response) => {
    try {
      const path = new URL(request.url ?? "/", "http://127.0.0.1").pathname;
      if (request.method !== "POST" || path !== callbackPath) {
        response.writeHead(404).end();
        return;
      }
      if (!secureBearerMatches(request.headers.authorization, bearer)) {
        response.writeHead(401, { "content-type": "application/json" });
        response.end(JSON.stringify({ code: "unauthenticated", message: "Unauthorized" }));
        return;
      }
      const body = JSON.parse(await readHttpBody(request));
      if (body.toolName !== TOOL_NAME) {
        throw new QualificationError(`unexpected callback tool: ${body.toolName}`);
      }
      if (JSON.stringify(body.args) !== JSON.stringify({ token: TOOL_ARGUMENT })) {
        throw new QualificationError(
          `unexpected callback arguments: ${JSON.stringify(body.args)}`,
        );
      }
      if (typeof body.agentId !== "string" || body.agentId.length === 0) {
        throw new QualificationError("custom-tool callback omitted agentId");
      }
      calls.push(body);
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ result: { value: TOOL_RESULT } }));
    } catch (error) {
      errors.push(error);
      response.writeHead(400, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          code: "invalid_argument",
          message: error instanceof Error ? error.message : String(error),
        }),
      );
    }
  });
  await new Promise((accept, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", accept);
  });
  const address = server.address();
  if (address === null || typeof address === "string") {
    server.close();
    throw new QualificationError("custom-tool callback server did not bind TCP");
  }
  return {
    bearer,
    calls,
    errors,
    server,
    url: `http://127.0.0.1:${address.port}`,
  };
}

function redact(value, secrets) {
  let result = String(value);
  for (const secret of secrets) {
    if (secret) result = result.split(secret).join("[REDACTED]");
  }
  return result;
}

async function startBridge({
  binary,
  workspace,
  stateRoot,
  apiKey,
  callback,
  timeoutMilliseconds,
}) {
  const diagnostics = [];
  const secrets = [apiKey, callback.bearer];
  const child = spawn(binary, [], {
    detached: process.platform !== "win32",
    env: {
      ...process.env,
      CURSOR_API_KEY: apiKey,
      CURSOR_SDK_BRIDGE_STATE_ROOT: stateRoot,
      CURSOR_SDK_BRIDGE_WORKSPACE: workspace,
      CURSOR_SDK_CLIENT_LANGUAGE: "node",
      CURSOR_SDK_TOOL_CALLBACK_AUTH_TOKEN: callback.bearer,
      CURSOR_SDK_TOOL_CALLBACK_URL: callback.url,
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  child.stderr.setEncoding("utf8");

  let buffered = "";
  let settleReady;
  let rejectReady;
  const readyPromise = new Promise((accept, reject) => {
    settleReady = accept;
    rejectReady = reject;
  });
  let settled = false;
  const finishReady = (value) => {
    if (!settled) {
      settled = true;
      settleReady(value);
    }
  };
  const failReady = (error) => {
    if (!settled) {
      settled = true;
      rejectReady(error);
    }
  };
  const inspectLine = (line) => {
    if (line.startsWith(READY_PREFIX)) {
      try {
        finishReady(JSON.parse(line.slice(READY_PREFIX.length)));
      } catch (error) {
        failReady(new QualificationError(`invalid bridge ready line: ${error}`));
      }
      return;
    }
    if (line.trim()) diagnostics.push(redact(line, secrets));
    if (diagnostics.length > 40) diagnostics.shift();
  };
  child.stderr.on("data", (chunk) => {
    buffered += chunk;
    let newline;
    while ((newline = buffered.indexOf("\n")) >= 0) {
      inspectLine(buffered.slice(0, newline).replace(/\r$/u, ""));
      buffered = buffered.slice(newline + 1);
    }
  });
  child.once("error", failReady);
  child.once("exit", (code, signal) => {
    if (buffered) inspectLine(buffered);
    failReady(
      new QualificationError(
        `Cursor SDK Bridge exited before ready (status ${code}, signal ${signal})${
          diagnostics.length > 0 ? `:\n${diagnostics.join("\n")}` : ""
        }`,
      ),
    );
  });
  const timer = setTimeout(
    () => failReady(new QualificationError("Cursor SDK Bridge startup timed out")),
    timeoutMilliseconds,
  );
  let ready;
  try {
    ready = await readyPromise;
  } catch (error) {
    await terminateProcessTree(child);
    throw error;
  } finally {
    clearTimeout(timer);
  }
  if (
    ready.schemaVersion !== 1 ||
    ready.transport !== "tcp" ||
    ready.protocol !== "connect" ||
    typeof ready.url !== "string"
  ) {
    await terminateProcessTree(child);
    throw new QualificationError("Cursor SDK Bridge returned an unsupported discovery payload");
  }
  const token =
    typeof ready.authToken === "string"
      ? ready.authToken
      : (await readFile(ready.authTokenFile, "utf8")).trim();
  if (!token) {
    await terminateProcessTree(child);
    throw new QualificationError("Cursor SDK Bridge returned an empty bearer token");
  }
  secrets.push(token);
  return {
    child,
    diagnostics,
    secrets,
    token,
    url: ready.url,
  };
}

async function terminateProcessTree(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise((accept) => child.once("exit", accept));
  try {
    if (process.platform === "win32") child.kill("SIGTERM");
    else process.kill(-child.pid, "SIGTERM");
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
  const graceful = await Promise.race([
    exited.then(() => true),
    new Promise((accept) => setTimeout(() => accept(false), 5_000)),
  ]);
  if (graceful) return;
  try {
    if (process.platform === "win32") child.kill("SIGKILL");
    else process.kill(-child.pid, "SIGKILL");
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
  await Promise.race([
    exited,
    new Promise((accept) => setTimeout(accept, 5_000)),
  ]);
}

function requestTimeout(timeoutMilliseconds) {
  const controller = new AbortController();
  const timer = setTimeout(
    () => controller.abort(new QualificationError("Cursor SDK Bridge RPC timed out")),
    timeoutMilliseconds,
  );
  return { controller, timer };
}

async function unary(client, service, method, body, timeoutMilliseconds) {
  const { controller, timer } = requestTimeout(timeoutMilliseconds);
  try {
    const response = await fetch(`${client.url}/sdk.v1.${service}/${method}`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${client.token}`,
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    const text = await response.text();
    let value;
    try {
      value = text ? JSON.parse(text) : {};
    } catch (error) {
      throw new QualificationError(`${method} returned invalid JSON: ${error}`);
    }
    if (!response.ok || (value.code !== undefined && value.message !== undefined)) {
      throw new QualificationError(
        `${method} failed (${response.status}): ${JSON.stringify(value)}`,
      );
    }
    return value;
  } finally {
    clearTimeout(timer);
  }
}

function connectFrame(value) {
  const payload = Buffer.from(JSON.stringify(value), "utf8");
  const header = Buffer.alloc(5);
  header.writeUInt8(0, 0);
  header.writeUInt32BE(payload.length, 1);
  return Buffer.concat([header, payload]);
}

async function serverStream(
  client,
  service,
  method,
  request,
  timeoutMilliseconds,
  onMessage,
) {
  const { controller, timer } = requestTimeout(timeoutMilliseconds);
  try {
    const response = await fetch(`${client.url}/sdk.v1.${service}/${method}`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${client.token}`,
        "connect-protocol-version": "1",
        "content-type": "application/connect+json",
      },
      body: connectFrame(request),
      signal: controller.signal,
    });
    if (!response.ok || response.body === null) {
      const detail = await response.text();
      throw new QualificationError(
        `${method} failed (${response.status}): ${detail.slice(0, 4_096)}`,
      );
    }
    const messages = [];
    let buffered = Buffer.alloc(0);
    let sawEnd = false;
    for await (const raw of Readable.fromWeb(response.body)) {
      buffered = Buffer.concat([buffered, Buffer.from(raw)]);
      while (buffered.length >= 5) {
        const flags = buffered.readUInt8(0);
        const length = buffered.readUInt32BE(1);
        if (length > MAX_CONNECT_FRAME_BYTES) {
          throw new QualificationError(`Connect frame exceeded ${MAX_CONNECT_FRAME_BYTES} bytes`);
        }
        if (buffered.length < 5 + length) break;
        const payload = buffered.subarray(5, 5 + length);
        buffered = buffered.subarray(5 + length);
        if ((flags & 0x01) !== 0) {
          throw new QualificationError("compressed Connect frames are not supported by this probe");
        }
        let value;
        try {
          value = payload.length > 0 ? JSON.parse(payload.toString("utf8")) : {};
        } catch (error) {
          throw new QualificationError(`${method} emitted invalid JSON: ${error}`);
        }
        if ((flags & 0x02) !== 0) {
          sawEnd = true;
          if (value.error !== undefined) {
            throw new QualificationError(
              `${method} stream failed: ${JSON.stringify(value.error)}`,
            );
          }
        } else {
          messages.push(value);
          if (onMessage !== undefined) await onMessage(value);
        }
      }
    }
    if (buffered.length !== 0) {
      throw new QualificationError("Send ended with a partial Connect frame");
    }
    if (!sawEnd) {
      throw new QualificationError(`${method} omitted the Connect end-stream frame`);
    }
    return messages;
  } finally {
    clearTimeout(timer);
  }
}

async function send(client, request, timeoutMilliseconds, onMessage) {
  return serverStream(
    client,
    "SdkAgentService",
    "Send",
    request,
    timeoutMilliseconds,
    onMessage,
  );
}

function terminalStatusIsFinished(status) {
  return status === 3 || /(?:FINISHED|COMPLETED)$/u.test(String(status));
}

function toolIsForbidden(name) {
  const normalized = String(name).toLowerCase().replace(/[^a-z0-9]+/gu, "_");
  if (normalized.includes("mcp") || normalized.includes(TOOL_NAME)) return false;
  const segments = normalized.split("_").filter(Boolean);
  return segments.some((segment) => FORBIDDEN_BUILT_INS.has(segment));
}

function sdkMessages(frames) {
  return frames
    .map((frame) => frame.sdkMessage?.message)
    .filter((message) => message !== undefined);
}

function assistantText(messages) {
  const chunks = [];
  for (const message of messages) {
    if (message.type !== "assistant") continue;
    const content = message.message?.content;
    if (!Array.isArray(content)) continue;
    for (const block of content) {
      if (block?.type === "text" && typeof block.text === "string") chunks.push(block.text);
    }
  }
  return chunks.join("");
}

async function qualifyTurn(client, callback, agentId, ordinal, timeoutMilliseconds) {
  const callbackStart = callback.calls.length;
  const callbackErrorStart = callback.errors.length;
  const frames = await send(
    client,
    {
      agentId,
      message: {
        text:
          `Call the custom tool ${TOOL_NAME} exactly once with ` +
          `{"token":"${TOOL_ARGUMENT}"}. Then attempt to call Cursor's native shell ` +
          `tool with the harmless command "printf cursor-native-shell-probe". If the ` +
          `native shell tool is unavailable, do not substitute another tool. The custom ` +
          `tool result is an object with a value field; reply with only that value.`,
      },
      options: { enableDeltas: true, enableSteps: true },
    },
    timeoutMilliseconds,
  );
  if (callback.errors.length !== callbackErrorStart) {
    throw callback.errors.at(-1);
  }
  const callbackCount = callback.calls.length - callbackStart;
  if (callbackCount !== 1) {
    throw new QualificationError(
      `turn ${ordinal}: expected one custom-tool callback, observed ${callbackCount}`,
    );
  }
  const callbackCall = callback.calls[callbackStart];

  const messages = sdkMessages(frames);
  const system = messages.find((message) => message.type === "system");
  // SDKSystemMessage.tools is optional and some Bridge/runtime combinations
  // omit it. Use it as corroborating telemetry when present; the authoritative
  // negative check is the explicit native-shell request above plus the
  // streamed tool events and the exactly-once host callback.
  const reportedTools = Array.isArray(system?.tools) ? system.tools : null;
  const forbidden = reportedTools?.filter(toolIsForbidden) ?? [];
  if (forbidden.length > 0) {
    throw new QualificationError(
      `turn ${ordinal}: built-in tools escaped confinement: ${forbidden.join(", ")}`,
    );
  }
  const toolMessages = messages.filter((message) => message.type === "tool_call");
  const callIds = new Set(toolMessages.map((message) => message.call_id).filter(Boolean));
  const toolNames = [...new Set(toolMessages.map((message) => message.name).filter(Boolean))];
  const unexpectedToolNames = toolNames.filter(
    (name) => name !== "mcp" && !String(name).includes(TOOL_NAME),
  );
  if (
    callIds.size !== 1 ||
    toolNames.length === 0 ||
    unexpectedToolNames.length > 0
  ) {
    throw new QualificationError(
      `turn ${ordinal}: tool stream escaped the single custom MCP call ` +
        `(ids=${[...callIds].join(", ")}, names=${toolNames.join(", ")})`,
    );
  }
  const streamedCallId = [...callIds][0];
  const callbackCallId = callbackCall.toolCallId;
  if (callbackCallId !== undefined && callbackCallId !== streamedCallId) {
    throw new QualificationError(
      `turn ${ordinal}: MCP stream call ${streamedCallId} did not correlate with ` +
        `callback call ${callbackCallId}`,
    );
  }
  if (
    toolMessages.some((message) => message.status === "error") ||
    !toolMessages.some((message) => message.status === "completed")
  ) {
    throw new QualificationError(`turn ${ordinal}: custom MCP call did not complete cleanly`);
  }
  const resultFrame = frames.find((frame) => frame.result !== undefined)?.result;
  if (resultFrame === undefined || !terminalStatusIsFinished(resultFrame.status)) {
    throw new QualificationError(
      `turn ${ordinal}: run did not finish successfully (${resultFrame?.status})`,
    );
  }
  if (!frames.some((frame) => frame.done !== undefined)) {
    throw new QualificationError(`turn ${ordinal}: stream omitted done`);
  }
  const finalText = resultFrame.result?.result ?? assistantText(messages);
  if (!String(finalText).includes(TOOL_RESULT)) {
    throw new QualificationError(`turn ${ordinal}: assistant did not use the tool result`);
  }
  return {
    turn: ordinal,
    run_id: resultFrame.runId ?? resultFrame.result?.runId,
    custom_tool_callbacks: callbackCount,
    effective_tools: reportedTools,
    effective_tools_reported: reportedTools !== null,
    streamed_tool_names: toolNames,
    callback_tool_name: callbackCall.toolName,
    stream_callback_id_correlated:
      callbackCallId === undefined ? null : callbackCallId === streamedCallId,
    assistant_used_result: true,
    built_in_tools_present: false,
    native_shell_negative_probe: true,
  };
}

function agentOptions(apiKey, model, workspace) {
  return {
    model: { id: model },
    apiKey,
    name: "Trouve Cursor SDK Bridge qualification",
    tools: { names: ["mcp"] },
    disallowedTools: [],
    mcpServers: {},
    agents: {},
    local: {
      cwd: [workspace],
      settingSources: [],
      // The standalone Bridge does not bundle/support Cursor's native sandbox
      // on every host. Confinement for this candidate is the explicit
      // tools=["mcp"] allow-list above: no filesystem, shell, task, or other
      // native tool reaches the model, and the one MCP callback is host-owned.
      sandboxOptions: { enabled: false },
      store: { type: "sqlite" },
      autoReview: false,
      customTools: {
        [TOOL_NAME]: {
          description:
            "Qualification-only echo. Return the supplied token through Trouve's host callback.",
          inputSchema: {
            type: "object",
            properties: { token: { type: "string" } },
            required: ["token"],
            additionalProperties: false,
          },
        },
      },
    },
  };
}

async function safeUnary(client, service, method, body, timeoutMilliseconds) {
  if (client === undefined) return;
  try {
    await unary(client, service, method, body, timeoutMilliseconds);
  } catch {
    // Best-effort cleanup must not replace the qualification result.
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const apiKey = process.env.CURSOR_API_KEY;
  if (!apiKey) {
    throw new QualificationError(
      "CURSOR_API_KEY is required. Create a Cursor user/service API key, then run " +
        "`CURSOR_API_KEY=... node scripts/qualify_cursor_sdk_bridge.mjs`.",
    );
  }
  await access(args.workspace, fsConstants.R_OK);
  const timeoutMilliseconds = args.timeoutSeconds * 1_000;
  const temporaryRoot = await mkdtemp(join(tmpdir(), "trouve-cursor-sdk-bridge-"));
  const stateRoot = join(temporaryRoot, "state");
  await mkdir(stateRoot);
  let bridge;
  let callback;
  let agentId;
  try {
    const resolvedBridge = await resolveBridge(
      args.bridge,
      temporaryRoot,
      timeoutMilliseconds,
    );
    callback = await startToolCallbackServer();
    bridge = await startBridge({
      binary: resolvedBridge.binary,
      workspace: args.workspace,
      stateRoot,
      apiKey,
      callback,
      timeoutMilliseconds,
    });

    const ping = await unary(
      bridge,
      "SdkBridgeControlService",
      "Ping",
      {},
      timeoutMilliseconds,
    );
    if (ping.message !== "pong") throw new QualificationError("bridge Ping did not return pong");
    const version = await unary(
      bridge,
      "SdkBridgeControlService",
      "GetVersion",
      {},
      timeoutMilliseconds,
    );
    if (version.protocolVersion !== "sdk.v1") {
      throw new QualificationError(
        `unsupported Cursor SDK protocol: ${version.protocolVersion}`,
      );
    }
    const authenticated = await unary(
      bridge,
      "SdkCursorService",
      "Me",
      { options: { apiKey } },
      timeoutMilliseconds,
    );
    if (authenticated.user === undefined) {
      throw new QualificationError("Cursor Me did not return an authenticated user");
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
      args.model ?? catalog.items.find((item) => item.id === "composer-2")?.id ?? catalog.items[0].id;
    if (!catalog.items.some((item) => item.id === model)) {
      throw new QualificationError(
        `model ${model} is not available; choose one from Cursor ListModels`,
      );
    }

    process.stderr.write(
      `Running two paid Cursor SDK qualification turns with model ${model}...\n`,
    );
    const options = agentOptions(apiKey, model, args.workspace);
    const created = await unary(
      bridge,
      "SdkAgentService",
      "CreateAgent",
      { options },
      timeoutMilliseconds,
    );
    agentId = created.agentId;
    if (typeof agentId !== "string" || agentId.length === 0) {
      throw new QualificationError("CreateAgent omitted agentId");
    }
    const turns = [
      await qualifyTurn(bridge, callback, agentId, 1, timeoutMilliseconds),
    ];

    await unary(
      bridge,
      "SdkAgentService",
      "CloseAgent",
      { agentId },
      timeoutMilliseconds,
    );
    const resumed = await unary(
      bridge,
      "SdkAgentService",
      "ResumeAgent",
      { agentId, options },
      timeoutMilliseconds,
    );
    if (resumed.agentId !== agentId) {
      throw new QualificationError("ResumeAgent returned a different agentId");
    }
    turns.push(await qualifyTurn(bridge, callback, agentId, 2, timeoutMilliseconds));

    process.stdout.write(
      `${JSON.stringify(
        {
          candidate: "cursor-sdk-bridge",
          result: "pass",
          bridge_version: version.bridgeVersion,
          protocol_version: version.protocolVersion,
          pinned_release: BRIDGE_VERSION,
          api_key_authentication: true,
          model,
          built_ins_confined: true,
          confinement: "explicit-tool-allowlist",
          cursor_native_sandbox: false,
          native_shell_negative_probe: true,
          exactly_once: true,
          resume: true,
          isolated_state_removed: !args.keepDownload,
          turns,
        },
        null,
        2,
      )}\n`,
    );
  } finally {
    if (bridge !== undefined && agentId !== undefined) {
      await safeUnary(
        bridge,
        "SdkAgentService",
        "CloseAgent",
        { agentId },
        Math.min(timeoutMilliseconds, 10_000),
      );
    }
    if (bridge !== undefined) {
      await safeUnary(
        bridge,
        "SdkBridgeControlService",
        "Shutdown",
        { graceSeconds: 1 },
        Math.min(timeoutMilliseconds, 10_000),
      );
      await terminateProcessTree(bridge.child);
    }
    if (callback !== undefined) {
      await new Promise((accept) => callback.server.close(accept));
    }
    if (!args.keepDownload) await rm(temporaryRoot, { recursive: true, force: true });
    else process.stderr.write(`Kept qualification files at ${temporaryRoot}\n`);
  }
}

export {
  BRIDGE_VERSION,
  QualificationError,
  assistantText,
  connectFrame,
  redact,
  resolveBridge,
  safeUnary,
  sdkMessages,
  send,
  serverStream,
  startBridge,
  terminalStatusIsFinished,
  terminateProcessTree,
  unary,
};

if (resolve(process.argv[1] ?? "") === fileURLToPath(import.meta.url)) {
  try {
    await main();
  } catch (error) {
    const apiKey = process.env.CURSOR_API_KEY;
    const message = redact(error instanceof Error ? error.message : error, [apiKey]);
    process.stderr.write(`Cursor SDK Bridge qualification failed: ${message}\n`);
    process.exitCode = 1;
  }
}
