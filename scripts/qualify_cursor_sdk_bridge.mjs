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
import { isIP } from "node:net";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { Readable, Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

const REPOSITORY_ROOT = fileURLToPath(new URL("../", import.meta.url));
const BRIDGE_VERSION = "1.0.28";
const RELEASE_ROOT = `https://github.com/cursor/sdk-bridge/releases/download/v${BRIDGE_VERSION}`;
// Reviewed independently of the mutable release assets. The fetched manifest
// remains a useful consistency check, but it is not the execution trust root.
const BRIDGE_SHA256 = Object.freeze({
  "cursor-sdk-bridge-standalone-darwin-arm64.tar.gz":
    "52ebfdab4e7806270122bea6c8f972646516297343c483e6700b37d444515af5",
  "cursor-sdk-bridge-standalone-darwin-x64.tar.gz":
    "ba59c6eaad62338118e59ceb6d24006e06f7c75b28e32dbc13950c4027511c3c",
  "cursor-sdk-bridge-standalone-linux-arm64.tar.gz":
    "0222f5c60c88b82063a0547bd938945c777c2a470def69de6464c04470ae0560",
  "cursor-sdk-bridge-standalone-linux-x64.tar.gz":
    "5357a42d3faa668a3ef25c6669fe576544b032dd17fabbbfa515355cd8d33c19",
  "cursor-sdk-bridge-standalone-win32-x64.tar.gz":
    "8af767f8b60f48ccf9147ce89085cd1956a5a1b8c66d26ff078cc1bd193f2ebb",
});
const TOOL_NAME = "trouve_qualification_echo";
const TOOL_ARGUMENT = "cursor-sdk-bridge-tool-ok";
const TOOL_RESULT = "TROUVE_CURSOR_SDK_BRIDGE_OK";
const MAX_HTTP_BODY_BYTES = 4 * 1024 * 1024;
const MAX_BRIDGE_ARCHIVE_BYTES = 512 * 1024 * 1024;
const MAX_CHECKSUM_MANIFEST_BYTES = 1024 * 1024;
const MAX_CONNECT_FRAME_BYTES = 64 * 1024 * 1024;
const MAX_CONNECT_STREAM_BYTES = 64 * 1024 * 1024;
const MAX_CONNECT_STREAM_FRAMES = 100_000;
const MAX_PENDING_DIAGNOSTIC_CHARS = 16_384;
const MAX_TIMER_DELAY_MILLISECONDS = 2_147_483_647;
const READY_PREFIX = "cursor-sdk-bridge ready ";
const INVALID_TOOL_NAME = "trouve_qualification_invalid_builtin";
const KNOWN_NATIVE_TOOL_PROBE = "shell";
// Public ToolName vocabulary from the pinned @cursor/sdk 1.0.28 package.
// `mcp` is the only native capability Trouve intentionally allows.
const CURSOR_NATIVE_TOOL_DENYLIST = Object.freeze([
  "shell",
  "read",
  "edit",
  "grep",
  "glob",
  "ls",
  "task",
  "webSearch",
  "delete",
  "readLints",
  "webFetch",
  "semSearch",
  "updateTodos",
  "readTodos",
  "askQuestion",
  "await",
  "generateImage",
  "applyAgentDiff",
]);
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
const canonicalToolName = (name) => String(name).toLowerCase().replace(/[^a-z0-9]+/gu, "");
const FORBIDDEN_NATIVE_TOOL_NAMES = new Set(
  CURSOR_NATIVE_TOOL_DENYLIST.map(canonicalToolName),
);

class QualificationError extends Error {}

class ConnectRpcError extends QualificationError {
  constructor(method, status, payload) {
    super(`${method} failed (${status}): ${JSON.stringify(payload)}`);
    this.method = method;
    this.httpStatus = status;
    this.code = payload?.code;
  }
}

function isUnsupportedRpcMethodError(error, method) {
  return (
    error instanceof ConnectRpcError &&
    error.method === method &&
    ["unimplemented", 12, "12"].includes(error.code)
  );
}

const help = `Usage: node scripts/qualify_cursor_sdk_bridge.mjs [options]

Requires CURSOR_API_KEY. The default run downloads and verifies Cursor SDK
Bridge v${BRIDGE_VERSION}, then performs two billable local SDK turns.

Options:
  --bridge PATH     Use an existing cursor-sdk-bridge binary
  --workspace PATH  Workspace shown to the sandboxed agent (default: repo root)
  --model ID        Cursor model id (default: composer-2 when available)
  --timeout SECONDS Timeout for startup and each RPC/turn (default: 300)
  --keep-download   Keep verified download files; runtime state is still removed
  --help            Show this help
`;

function parseTimeoutSeconds(value) {
  const seconds = Number(value);
  if (
    !Number.isFinite(seconds) ||
    seconds <= 0 ||
    seconds * 1_000 > MAX_TIMER_DELAY_MILLISECONDS
  ) {
    throw new QualificationError(
      `--timeout must be a positive number no greater than ${MAX_TIMER_DELAY_MILLISECONDS / 1_000}`,
    );
  }
  return seconds;
}

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
  parsed.timeoutSeconds = parseTimeoutSeconds(parsed.timeoutSeconds);
  parsed.workspace = resolve(parsed.workspace);
  if (parsed.bridge !== undefined) parsed.bridge = resolve(parsed.bridge);
  return parsed;
}

function assetName(platform = process.platform, cpu = process.arch) {
  const operatingSystem = {
    linux: "linux",
    darwin: "darwin",
    win32: "win32",
  }[platform];
  const architecture = {
    x64: "x64",
    arm64: "arm64",
  }[cpu];
  if (
    operatingSystem === undefined ||
    architecture === undefined ||
    (platform === "win32" && cpu === "arm64")
  ) {
    throw new QualificationError(
      `unsupported Cursor SDK Bridge platform: ${platform}/${cpu}`,
    );
  }
  return `cursor-sdk-bridge-standalone-${operatingSystem}-${architecture}.tar.gz`;
}

function expectedBridgeChecksum(asset) {
  const checksum = BRIDGE_SHA256[asset];
  if (checksum === undefined) {
    throw new QualificationError(`no reviewed checksum is pinned for ${asset}`);
  }
  return checksum;
}

async function download(url, destination, signal, limit) {
  const response = await fetch(url, {
    redirect: "follow",
    signal,
  });
  if (!response.ok || response.body === null) {
    throw new QualificationError(`download failed (${response.status}) for ${url}`);
  }
  let received = 0;
  const limiter = new Transform({
    transform(chunk, _encoding, callback) {
      received += chunk.length;
      if (received > limit) {
        callback(new QualificationError(`download exceeded ${limit} bytes for ${url}`));
      } else {
        callback(null, chunk);
      }
    },
  });
  try {
    const contentLength = Number(response.headers.get("content-length"));
    if (Number.isFinite(contentLength) && contentLength > limit) {
      throw new QualificationError(`download exceeded ${limit} bytes for ${url}`);
    }
    await pipeline(
      Readable.fromWeb(response.body),
      limiter,
      createWriteStream(destination),
      { signal },
    );
  } catch (error) {
    try {
      await response.body.cancel();
    } catch {
      // The pipeline may already own or close the response stream.
    }
    await rm(destination, { force: true });
    throw error;
  }
}

async function readBoundedJsonResponse(response, label, limit = MAX_HTTP_BODY_BYTES) {
  if (response.body === null) return {};
  const chunks = [];
  let received = 0;
  for await (const chunk of Readable.fromWeb(response.body)) {
    received += chunk.length;
    if (received > limit) {
      throw new QualificationError(`${label} response exceeded ${limit} bytes`);
    }
    chunks.push(chunk);
  }
  if (received === 0) return {};
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch (error) {
    throw new QualificationError(`${label} returned invalid JSON: ${error}`);
  }
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
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      void terminateTimedOutChild(child, command).catch(reject);
    }, timeoutMilliseconds);
    child.once("error", (error) => {
      clearTimeout(timer);
      if (!timedOut) reject(error);
    });
    child.once("exit", (code, signal) => {
      clearTimeout(timer);
      if (timedOut) return;
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
  const downloads = new AbortController();
  const downloadTimer = setTimeout(
    () => downloads.abort(new QualificationError("Cursor SDK Bridge download timed out")),
    timeoutMilliseconds,
  );
  try {
    const pending = [
      [`${RELEASE_ROOT}/${asset}`, archive, MAX_BRIDGE_ARCHIVE_BYTES],
      [`${RELEASE_ROOT}/SHA256SUMS.txt`, sums, MAX_CHECKSUM_MANIFEST_BYTES],
    ].map(async ([url, destination, limit]) => {
      try {
        await download(url, destination, downloads.signal, limit);
      } catch (error) {
        // Abort the peer immediately, then let allSettled below acknowledge
        // both pipelines before temporary state can be removed.
        downloads.abort(error);
        throw error;
      }
    });
    const results = await Promise.allSettled(pending);
    const failed = results.find((result) => result.status === "rejected");
    if (failed !== undefined) throw failed.reason;
  } finally {
    clearTimeout(downloadTimer);
  }
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
  const reviewed = expectedBridgeChecksum(asset);
  if (expected !== reviewed) {
    throw new QualificationError(
      `release checksum for ${asset} differs from the reviewed checksum`,
    );
  }
  const actual = await sha256(archive);
  if (actual !== reviewed) {
    throw new QualificationError(
      `checksum mismatch for ${asset}: expected ${reviewed}, got ${actual}`,
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

async function readHttpBody(request, timeoutMilliseconds) {
  const chunks = [];
  let length = 0;
  const timer = setTimeout(
    () => request.destroy(new QualificationError("custom-tool callback body timed out")),
    Math.min(timeoutMilliseconds, 30_000),
  );
  try {
    for await (const chunk of request) {
      length += chunk.length;
      if (length > MAX_HTTP_BODY_BYTES) {
        request.destroy();
        throw new QualificationError("custom-tool callback body exceeded the probe limit");
      }
      chunks.push(chunk);
    }
    return Buffer.concat(chunks).toString("utf8");
  } finally {
    clearTimeout(timer);
  }
}

async function startToolCallbackServer(timeoutMilliseconds) {
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
      const body = JSON.parse(await readHttpBody(request, timeoutMilliseconds));
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
      if (!response.destroyed) {
        response.writeHead(400, { "content-type": "application/json" });
        response.end(
          JSON.stringify({
            code: "invalid_argument",
            message: error instanceof Error ? error.message : String(error),
          }),
        );
      }
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

function capPendingDiagnostic(value) {
  if (value.length <= MAX_PENDING_DIAGNOSTIC_CHARS) {
    return { text: value, truncated: false };
  }
  return {
    text: value.slice(-MAX_PENDING_DIAGNOSTIC_CHARS),
    truncated: true,
  };
}

function validateLoopbackBridgeUrl(value) {
  let parsed;
  try {
    parsed = new URL(value);
  } catch (error) {
    throw new QualificationError(`Cursor SDK Bridge returned an invalid URL: ${error}`);
  }
  const hostname = parsed.hostname.replace(/^\[|\]$/gu, "");
  const ipVersion = isIP(hostname);
  const loopback =
    (ipVersion === 4 && hostname.split(".")[0] === "127") ||
    (ipVersion === 6 && hostname === "::1");
  if (
    parsed.protocol !== "http:" ||
    !loopback ||
    parsed.username !== "" ||
    parsed.password !== "" ||
    parsed.search !== "" ||
    parsed.hash !== ""
  ) {
    throw new QualificationError(
      "Cursor SDK Bridge must advertise an uncredentialed literal loopback HTTP URL",
    );
  }
}

async function startBridge({
  binary,
  workspace,
  stateRoot,
  apiKey,
  callback,
  timeoutMilliseconds,
  beforeSpawn,
  onSpawn,
}) {
  const diagnostics = [];
  const secrets = [apiKey, callback.bearer];
  const runtimeRoot = join(stateRoot, "runtime");
  await mkdir(runtimeRoot, { recursive: true, mode: 0o700 });
  beforeSpawn?.();
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
      TMPDIR: runtimeRoot,
      TEMP: runtimeRoot,
      TMP: runtimeRoot,
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  onSpawn?.(child);
  child.stderr.setEncoding("utf8");

  let buffered = "";
  let bufferedWasTruncated = false;
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
    if (line.trim()) diagnostics.push(redact(line, secrets).slice(-16_384));
    if (diagnostics.length > 40) diagnostics.shift();
  };
  child.stderr.on("data", (chunk) => {
    buffered += chunk;
    let newline;
    while ((newline = buffered.indexOf("\n")) >= 0) {
      const line = buffered.slice(0, newline).replace(/\r$/u, "");
      inspectLine(`${bufferedWasTruncated ? "[truncated prefix] " : ""}${line}`);
      buffered = buffered.slice(newline + 1);
      bufferedWasTruncated = false;
    }
    const pending = capPendingDiagnostic(buffered);
    buffered = pending.text;
    bufferedWasTruncated ||= pending.truncated;
  });
  child.once("error", failReady);
  child.once("exit", (code, signal) => {
    if (buffered) {
      inspectLine(`${bufferedWasTruncated ? "[truncated prefix] " : ""}${buffered}`);
    }
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
  try {
    if (
      ready.schemaVersion !== 1 ||
      ready.transport !== "tcp" ||
      ready.protocol !== "connect" ||
      typeof ready.url !== "string"
    ) {
      throw new QualificationError("Cursor SDK Bridge returned an unsupported discovery payload");
    }
    validateLoopbackBridgeUrl(ready.url);
    const token =
      typeof ready.authToken === "string"
        ? ready.authToken.trim()
        : typeof ready.authTokenFile === "string"
          ? (await readFile(ready.authTokenFile, "utf8")).trim()
          : "";
    if (!token) {
      throw new QualificationError("Cursor SDK Bridge returned an empty bearer token");
    }
    secrets.push(token);
    for (let index = 0; index < diagnostics.length; index += 1) {
      diagnostics[index] = redact(diagnostics[index], secrets);
    }
    return {
      child,
      diagnostics,
      secrets,
      token,
      url: ready.url,
    };
  } catch (error) {
    await terminateProcessTree(child);
    throw error;
  }
}

const PROCESS_TREE_TERMINATIONS = new WeakMap();

function terminateProcessTree(child) {
  if (child === null || (typeof child !== "object" && typeof child !== "function")) {
    return Promise.resolve();
  }
  const existing = PROCESS_TREE_TERMINATIONS.get(child);
  if (existing !== undefined) return existing;
  const termination = terminateProcessTreeOnce(child);
  PROCESS_TREE_TERMINATIONS.set(child, termination);
  return termination;
}

async function terminateProcessTreeOnce(child) {
  if (!Number.isInteger(child?.pid)) return;
  if (process.platform === "win32") {
    const exited = () => child.exitCode !== null || child.signalCode !== null;
    const waitForExit = async (milliseconds) => {
      const deadline = Date.now() + milliseconds;
      while (!exited() && Date.now() < deadline) {
        await new Promise((accept) => setTimeout(accept, 50));
      }
      return exited();
    };
    if (!exited()) {
      const taskkill = spawn(
        "taskkill",
        ["/PID", String(child.pid), "/T", "/F"],
        { windowsHide: true, stdio: "ignore" },
      );
      if (!(await waitForChildSettlement(taskkill, 5_000))) {
        taskkill.kill("SIGKILL");
        if (!(await waitForChildSettlement(taskkill, 1_000))) {
          throw new QualificationError("taskkill process did not terminate");
        }
      }
      if (!(await waitForExit(5_000))) {
        child.kill("SIGKILL");
        if (!(await waitForExit(5_000))) {
          throw new QualificationError("Cursor SDK Bridge process did not terminate");
        }
      }
    }
    return;
  }

  const groupAlive = () => {
    try {
      process.kill(-child.pid, 0);
      return true;
    } catch (error) {
      if (error?.code === "ESRCH") return false;
      throw error;
    }
  };
  const signalGroup = (signal) => {
    try {
      process.kill(-child.pid, signal);
    } catch (error) {
      if (error?.code !== "ESRCH") throw error;
    }
  };
  const waitForGroupExit = async (milliseconds) => {
    const deadline = Date.now() + milliseconds;
    while (groupAlive() && Date.now() < deadline) {
      await new Promise((accept) => setTimeout(accept, 50));
    }
    return !groupAlive();
  };

  // The process-group id remains valid while descendants survive even if the
  // bridge leader has already exited. Always signal and verify the group.
  signalGroup("SIGTERM");
  if (await waitForGroupExit(5_000)) return;
  signalGroup("SIGKILL");
  if (!(await waitForGroupExit(5_000))) {
    throw new QualificationError("Cursor SDK Bridge process group did not terminate");
  }
}

async function waitForChildSettlement(child, milliseconds) {
  if (child.exitCode !== null || child.signalCode !== null) return true;
  return new Promise((accept) => {
    let timer;
    const finish = (settled) => {
      clearTimeout(timer);
      child.off("error", onError);
      child.off("exit", onExit);
      accept(settled);
    };
    const onError = () => finish(true);
    const onExit = () => finish(true);
    child.once("error", onError);
    child.once("exit", onExit);
    timer = setTimeout(() => finish(false), milliseconds);
  });
}

async function terminateTimedOutChild(child, label, settlementMilliseconds = 5_000) {
  child.kill("SIGKILL");
  if (!(await waitForChildSettlement(child, settlementMilliseconds))) {
    throw new QualificationError(`${label} timed out and did not terminate`);
  }
  throw new QualificationError(`${label} timed out`);
}

function requestTimeout(timeoutMilliseconds) {
  const controller = new AbortController();
  const timer = setTimeout(
    () => controller.abort(new QualificationError("Cursor SDK Bridge RPC timed out")),
    timeoutMilliseconds,
  );
  return { controller, timer };
}

async function readResponseText(response, limit, label) {
  if (response.body === null) return "";
  const chunks = [];
  let length = 0;
  for await (const raw of Readable.fromWeb(response.body)) {
    const chunk = Buffer.from(raw);
    length += chunk.length;
    if (length > limit) {
      throw new QualificationError(`${label} response exceeded ${limit} bytes`);
    }
    chunks.push(chunk);
  }
  return Buffer.concat(chunks, length).toString("utf8");
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
    const text = await readResponseText(response, MAX_HTTP_BODY_BYTES, method);
    let value;
    try {
      value = text ? JSON.parse(text) : {};
    } catch (error) {
      throw new QualificationError(`${method} returned invalid JSON: ${error}`);
    }
    if (!response.ok || (value.code !== undefined && value.message !== undefined)) {
      throw new ConnectRpcError(method, response.status, value);
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

class ConnectFrameDecoder {
  constructor() {
    this.header = Buffer.alloc(5);
    this.headerLength = 0;
    this.flags = 0;
    this.payloadLength = 0;
    this.payloadRemaining = 0;
    this.payloadParts = [];
    this.totalPayloadBytes = 0;
    this.frameCount = 0;
  }

  push(raw) {
    const chunk = Buffer.from(raw);
    const frames = [];
    let offset = 0;
    while (offset < chunk.length) {
      if (this.headerLength < 5) {
        const copied = Math.min(5 - this.headerLength, chunk.length - offset);
        chunk.copy(this.header, this.headerLength, offset, offset + copied);
        this.headerLength += copied;
        offset += copied;
        if (this.headerLength < 5) break;
        this.flags = this.header.readUInt8(0);
        this.payloadLength = this.header.readUInt32BE(1);
        if (this.payloadLength > MAX_CONNECT_FRAME_BYTES) {
          throw new QualificationError(
            `Connect frame exceeded ${MAX_CONNECT_FRAME_BYTES} bytes`,
          );
        }
        this.totalPayloadBytes += this.payloadLength;
        if (this.totalPayloadBytes > MAX_CONNECT_STREAM_BYTES) {
          throw new QualificationError(
            `Connect stream exceeded ${MAX_CONNECT_STREAM_BYTES} retained bytes`,
          );
        }
        this.payloadRemaining = this.payloadLength;
      }

      const copied = Math.min(this.payloadRemaining, chunk.length - offset);
      if (copied > 0) {
        this.payloadParts.push(chunk.subarray(offset, offset + copied));
        this.payloadRemaining -= copied;
        offset += copied;
      }
      if (this.payloadRemaining !== 0) break;

      this.frameCount += 1;
      if (this.frameCount > MAX_CONNECT_STREAM_FRAMES) {
        throw new QualificationError(
          `Connect stream exceeded ${MAX_CONNECT_STREAM_FRAMES} frames`,
        );
      }
      frames.push({
        flags: this.flags,
        payload: Buffer.concat(this.payloadParts, this.payloadLength),
      });
      this.headerLength = 0;
      this.payloadLength = 0;
      this.payloadRemaining = 0;
      this.payloadParts = [];
    }
    return frames;
  }

  finish() {
    if (this.headerLength !== 0 || this.payloadRemaining !== 0) {
      throw new QualificationError("Send ended with a partial Connect frame");
    }
  }
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
      const detail = await readResponseText(response, MAX_HTTP_BODY_BYTES, method);
      throw new QualificationError(
        `${method} failed (${response.status}): ${detail.slice(0, 4_096)}`,
      );
    }
    const messages = [];
    const decoder = new ConnectFrameDecoder();
    let sawEnd = false;
    for await (const raw of Readable.fromWeb(response.body)) {
      for (const { flags, payload } of decoder.push(raw)) {
        if (sawEnd) {
          throw new QualificationError(
            `${method} emitted a frame after the Connect end-stream frame`,
          );
        }
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
    decoder.finish();
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
  return (
    status === 3 ||
    status === "3" ||
    status === "RUN_LIFECYCLE_STATUS_FINISHED"
  );
}

function assertUniqueToolLifecycle(toolMessages, label) {
  const seen = new Set();
  for (const message of toolMessages) {
    if (typeof message.call_id !== "string" || message.call_id.length === 0) {
      throw new QualificationError(`${label}: tool lifecycle event omitted its call id`);
    }
    const key = JSON.stringify([message.call_id, message.status ?? null]);
    if (seen.has(key)) {
      throw new QualificationError(
        `${label}: duplicate ${message.status ?? "unspecified"} lifecycle event for ` +
          message.call_id,
      );
    }
    seen.add(key);
  }
}

function exactTerminalResult(frames, label) {
  const results = frames.filter((frame) => frame.result !== undefined);
  const done = frames.filter((frame) => frame.done !== undefined);
  if (results.length !== 1 || done.length !== 1) {
    throw new QualificationError(
      `${label}: expected exactly one result and done frame ` +
        `(results=${results.length}, done=${done.length})`,
    );
  }
  if (
    done[0].done === null ||
    typeof done[0].done !== "object" ||
    Array.isArray(done[0].done)
  ) {
    throw new QualificationError(`${label}: done frame had an invalid envelope`);
  }
  return results[0].result;
}

export function toolIsForbidden(name) {
  const normalized = String(name).toLowerCase().replace(/[^a-z0-9]+/gu, "_");
  if (normalized === "mcp" || normalized === TOOL_NAME) return false;
  const segments = normalized.split("_").filter(Boolean);
  return (
    FORBIDDEN_NATIVE_TOOL_NAMES.has(canonicalToolName(name)) ||
    segments.some((segment) => FORBIDDEN_BUILT_INS.has(segment))
  );
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
          `{"token":"${TOOL_ARGUMENT}"}. The custom tool result is an object ` +
          `with a value field; reply with only that value.`,
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
  if (
    callbackCall.toolName !== TOOL_NAME ||
    callbackCall.agentId !== agentId ||
    callbackCall.args?.token !== TOOL_ARGUMENT
  ) {
    throw new QualificationError(
      `turn ${ordinal}: callback identity or arguments differed from the exact request`,
    );
  }

  const messages = sdkMessages(frames);
  const system = messages.find((message) => message.type === "system");
  // SDKSystemMessage.tools is optional and some Bridge/runtime combinations
  // omit it. This is corroborating telemetry only; the deterministic policy
  // check rejects an unknown built-in through CreateAgent before paid turns.
  const reportedTools = Array.isArray(system?.tools) ? system.tools : null;
  const forbidden = reportedTools?.filter(toolIsForbidden) ?? [];
  if (forbidden.length > 0) {
    throw new QualificationError(
      `turn ${ordinal}: built-in tools escaped confinement: ${forbidden.join(", ")}`,
    );
  }
  const toolMessages = messages.filter((message) => message.type === "tool_call");
  assertUniqueToolLifecycle(toolMessages, `turn ${ordinal}`);
  const callIds = new Set(toolMessages.map((message) => message.call_id));
  const toolNames = [...new Set(toolMessages.map((message) => message.name).filter(Boolean))];
  const unexpectedToolNames = toolNames.filter(
    (name) => name !== "mcp" && name !== TOOL_NAME,
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
  if (typeof callbackCallId !== "string" || callbackCallId.length === 0) {
    throw new QualificationError(`turn ${ordinal}: callback omitted its tool-call id`);
  }
  if (callbackCallId !== streamedCallId) {
    throw new QualificationError(
      `turn ${ordinal}: MCP stream call ${streamedCallId} did not correlate with ` +
        `callback call ${callbackCallId}`,
    );
  }
  if (
    toolMessages.some((message) => message.status === "error") ||
    toolMessages.filter((message) => message.status === "completed").length !== 1
  ) {
    throw new QualificationError(`turn ${ordinal}: custom MCP call did not complete cleanly`);
  }
  const resultFrame = exactTerminalResult(frames, `turn ${ordinal}`);
  if (!terminalStatusIsFinished(resultFrame.status)) {
    throw new QualificationError(
      `turn ${ordinal}: run did not finish successfully (${resultFrame?.status})`,
    );
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
    stream_callback_id_correlated: true,
    assistant_used_result: true,
    built_in_tools_present: false,
  };
}

function agentOptions(apiKey, model, workspace) {
  return {
    model: { id: model },
    apiKey,
    name: "Trouve Cursor SDK Bridge qualification",
    tools: { names: ["mcp"] },
    disallowedTools: [...CURSOR_NATIVE_TOOL_DENYLIST],
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

async function verifyToolAllowlist(client, options, timeoutMilliseconds) {
  if (
    JSON.stringify(options.disallowedTools) !==
    JSON.stringify(CURSOR_NATIVE_TOOL_DENYLIST)
  ) {
    throw new QualificationError(
      "tool-allowlist validation did not receive the pinned native-tool denylist",
    );
  }

  // Prove the probe name is a real tool in this exact Bridge release rather
  // than relying on a guessed identifier. The agent is never run, and is
  // closed before testing the shipping MCP-only policy below.
  const knownOptions = structuredClone(options);
  knownOptions.tools = { names: [KNOWN_NATIVE_TOOL_PROBE] };
  knownOptions.disallowedTools = knownOptions.disallowedTools.filter(
    (tool) => tool !== KNOWN_NATIVE_TOOL_PROBE,
  );
  knownOptions.local.customTools = {};
  const known = await unary(
    client,
    "SdkAgentService",
    "CreateAgent",
    { options: knownOptions },
    timeoutMilliseconds,
  );
  if (typeof known?.agentId !== "string" || known.agentId.length === 0) {
    throw new QualificationError(
      `known native tool probe ${KNOWN_NATIVE_TOOL_PROBE} returned no agent id`,
    );
  }
  await unary(
    client,
    "SdkAgentService",
    "CloseAgent",
    { agentId: known.agentId },
    Math.min(timeoutMilliseconds, 10_000),
  );

  const invalidOptions = structuredClone(options);
  invalidOptions.tools = { names: ["mcp", INVALID_TOOL_NAME] };
  let created;
  try {
    created = await unary(
      client,
      "SdkAgentService",
      "CreateAgent",
      { options: invalidOptions },
      timeoutMilliseconds,
    );
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    if (!isUnknownToolValidationError(error)) {
      throw new QualificationError(
        `tool-allowlist validation failed for an unrelated reason: ${detail}`,
      );
    }
    return {
      contract: "@cursor/sdk 1.0.28 AgentOptions.tools allowlist",
      known_native_tool_recognized: KNOWN_NATIVE_TOOL_PROBE,
      known_native_probe_agent_run: false,
      explicit_native_denylist: [...CURSOR_NATIVE_TOOL_DENYLIST],
      unknown_tool_rejected_with_invalid_argument: true,
      model_behavior_used_as_evidence: false,
    };
  }
  if (typeof created?.agentId === "string" && created.agentId.length > 0) {
    await safeUnary(
      client,
      "SdkAgentService",
      "CloseAgent",
      { agentId: created.agentId },
      Math.min(timeoutMilliseconds, 10_000),
    );
  }
  throw new QualificationError(
    `CreateAgent accepted the unknown built-in tool ${INVALID_TOOL_NAME}`,
  );
}

function isUnknownToolValidationError(error) {
  return error instanceof ConnectRpcError &&
    ["invalid_argument", 3, "3"].includes(error.code) &&
    error.message.includes(INVALID_TOOL_NAME);
}

async function safeUnary(client, service, method, body, timeoutMilliseconds) {
  if (client === undefined) return;
  try {
    await unary(client, service, method, body, timeoutMilliseconds);
  } catch {
    // Best-effort cleanup must not replace the qualification result.
  }
}

async function runCleanupSteps(steps) {
  const errors = [];
  for (const [label, cleanup] of steps) {
    try {
      await cleanup();
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      errors.push(new Error(`${label}: ${detail}`, { cause: error }));
    }
  }
  if (errors.length > 0) {
    throw new AggregateError(
      errors,
      `Cursor SDK Bridge cleanup failed: ${errors.map((error) => error.message).join("; ")}`,
    );
  }
}

function createProcessCleanupBoundary() {
  let processCleanupFailure;
  return {
    async terminate(cleanup) {
      try {
        await cleanup();
      } catch (error) {
        processCleanupFailure ??= error;
        throw error;
      }
    },
    async remove(label, cleanup) {
      if (processCleanupFailure !== undefined) {
        throw new QualificationError(
          `${label} retained because SDK Bridge process-tree cleanup failed`,
        );
      }
      await cleanup();
    },
  };
}

function combineQualificationAndCleanupErrors(qualificationError, cleanupError) {
  if (qualificationError === undefined) return cleanupError;
  const qualificationDetail = qualificationError instanceof Error
    ? qualificationError.message
    : String(qualificationError);
  const cleanupDetail = cleanupError instanceof Error
    ? cleanupError.message
    : String(cleanupError);
  return new AggregateError(
    [qualificationError, cleanupError],
    `qualification failed: ${qualificationDetail}; cleanup also failed: ${cleanupDetail}`,
  );
}

function installSignalCleanup(
  cleanup,
  {
    target = process,
    exit = (code) => process.exit(code),
    report = (error) => process.stderr.write(`signal cleanup failed: ${error}\n`),
  } = {},
) {
  let completion;
  const handlers = new Map();
  for (const [signal, code] of [["SIGINT", 130], ["SIGTERM", 143]]) {
    const handler = () => {
      if (completion !== undefined) return;
      let result;
      try {
        result = cleanup(signal);
      } catch (error) {
        result = Promise.reject(error);
      }
      completion = Promise.resolve(result)
        .catch(report)
        .finally(() => exit(code));
    };
    handlers.set(signal, handler);
    target.on(signal, handler);
  }
  return {
    dispose() {
      for (const [signal, handler] of handlers) target.off(signal, handler);
    },
    completion: () => completion ?? Promise.resolve(),
    throwIfSignalled() {
      if (completion !== undefined) {
        throw new QualificationError("qualification interrupted by signal");
      }
    },
  };
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
  const bridgeChildren = new Set();
  let cleanupPromise;
  const cleanup = (signal) => {
    if (cleanupPromise !== undefined) return cleanupPromise;
    cleanupPromise = (async () => {
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
      if (!args.keepDownload) {
        cleanupSteps.push([
          "remove SDK Bridge qualification files",
          () => processCleanup.remove(
            "SDK Bridge qualification files",
            () => rm(temporaryRoot, { recursive: true, force: true }),
          ),
        ]);
      } else {
        cleanupSteps.push([
          "remove SDK Bridge state",
          () => processCleanup.remove(
            "SDK Bridge state",
            () => rm(stateRoot, { recursive: true, force: true }),
          ),
        ]);
        process.stderr.write(`Kept verified download files at ${temporaryRoot}\n`);
      }
      await runCleanupSteps(cleanupSteps);
    })();
    return cleanupPromise;
  };
  const signalCleanup = installSignalCleanup(cleanup, {
    report: (error) => process.stderr.write(
      `Cursor SDK Bridge signal cleanup failed: ${redact(error, [apiKey])}\n`,
    ),
  });
  let qualificationError;
  try {
    const resolvedBridge = await resolveBridge(
      args.bridge,
      temporaryRoot,
      timeoutMilliseconds,
    );
    callback = await startToolCallbackServer(timeoutMilliseconds);
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
    const toolPolicy = await verifyToolAllowlist(
      bridge,
      options,
      timeoutMilliseconds,
    );
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
          confinement: "sdk-tool-allowlist-contract",
          tool_policy_validation: toolPolicy,
          cursor_native_sandbox: false,
          exactly_once: true,
          resume: true,
          isolated_state_removed: true,
          verified_download_files_retained: args.keepDownload,
          turns,
        },
        null,
        2,
      )}\n`,
    );
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
}

export {
  BRIDGE_VERSION,
  CURSOR_NATIVE_TOOL_DENYLIST,
  ConnectRpcError,
  QualificationError,
  assetName,
  assistantText,
  assertUniqueToolLifecycle,
  capPendingDiagnostic,
  combineQualificationAndCleanupErrors,
  connectFrame,
  createProcessCleanupBoundary,
  download,
  exactTerminalResult,
  expectedBridgeChecksum,
  installSignalCleanup,
  isUnknownToolValidationError,
  isUnsupportedRpcMethodError,
  parseTimeoutSeconds,
  redact,
  readBoundedJsonResponse,
  resolveBridge,
  runCleanupSteps,
  safeUnary,
  sdkMessages,
  send,
  serverStream,
  startBridge,
  terminalStatusIsFinished,
  terminateTimedOutChild,
  terminateProcessTree,
  unary,
  verifyToolAllowlist,
  validateLoopbackBridgeUrl,
  waitForChildSettlement,
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
