import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { EventEmitter } from "node:events";
import { constants as fsConstants } from "node:fs";
import {
  access,
  chmod,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { inflateSync } from "node:zlib";

import {
  CURSOR_NATIVE_TOOL_DENYLIST,
  ConnectRpcError,
  assetName,
  assertUniqueToolLifecycle,
  capPendingDiagnostic,
  combineQualificationAndCleanupErrors,
  download,
  exactTerminalResult,
  expectedBridgeChecksum,
  installSignalCleanup,
  isUnknownToolValidationError,
  isUnsupportedRpcMethodError,
  parseTimeoutSeconds,
  readBoundedJsonResponse,
  runCleanupSteps,
  serverStream,
  startBridge,
  terminalStatusIsFinished,
  terminateTimedOutChild,
  terminateProcessTree,
  validateLoopbackBridgeUrl,
  verifyToolAllowlist,
  waitForChildSettlement,
  toolIsForbidden,
} from "./qualify_cursor_sdk_bridge.mjs";
import {
  RED_PIXEL_PNG,
  createCallbackAdmission,
  inspectToolCalls,
  isNonEmptyTimestamp,
  parseConversationEvidence,
  qualificationExitCode,
  startCallbackServer,
} from "./qualify_cursor_sdk_bridge_full.mjs";

async function listen(server) {
  await new Promise((accept, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", accept);
  });
  const address = server.address();
  assert.notEqual(address, null);
  assert.equal(typeof address, "object");
  return address;
}

function connectTestFrame(flags, value) {
  const payload = Buffer.from(JSON.stringify(value));
  const frame = Buffer.alloc(5 + payload.length);
  frame[0] = flags;
  frame.writeUInt32BE(payload.length, 1);
  payload.copy(frame, 5);
  return frame;
}

test("timeout parsing rejects values outside Node's timer range", () => {
  assert.equal(parseTimeoutSeconds("300"), 300);
  assert.throws(() => parseTimeoutSeconds("2147484"), /no greater than/u);
});

test("cleanup attempts every registered resource before aggregating failures", async () => {
  const attempted = [];
  await assert.rejects(
    runCleanupSteps([
      ["first child", async () => {
        attempted.push("first child");
        throw new Error("first failed");
      }],
      ["second child", async () => {
        attempted.push("second child");
      }],
      ["callback server", async () => {
        attempted.push("callback server");
        throw new Error("callback failed");
      }],
      ["temporary state", async () => {
        attempted.push("temporary state");
      }],
    ]),
    (error) => {
      assert.equal(error instanceof AggregateError, true);
      assert.match(error.message, /first child: first failed/u);
      assert.match(error.message, /callback server: callback failed/u);
      return true;
    },
  );
  assert.deepEqual(attempted, [
    "first child",
    "second child",
    "callback server",
    "temporary state",
  ]);
});

test("cleanup failures preserve an earlier qualification failure", () => {
  const qualificationError = new Error("qualification failed first");
  const cleanupError = new Error("cleanup failed second");
  const combined = combineQualificationAndCleanupErrors(
    qualificationError,
    cleanupError,
  );
  assert.equal(combined instanceof AggregateError, true);
  assert.deepEqual(combined.errors, [qualificationError, cleanupError]);
  assert.match(combined.message, /qualification failed first/u);
  assert.match(combined.message, /cleanup failed second/u);
});

test("a blocked full qualification returns a failing process status", () => {
  assert.equal(qualificationExitCode({ decision: "proceed-with-sdk-bridge-adapter" }), 0);
  assert.equal(qualificationExitCode({ decision: "hold-sdk-bridge-promotion" }), 1);
  assert.equal(qualificationExitCode({ result: "passed" }), 0);
});

test("tool confinement pins every Cursor native tool except mcp", () => {
  assert.equal(CURSOR_NATIVE_TOOL_DENYLIST.includes("mcp"), false);
  for (const tool of CURSOR_NATIVE_TOOL_DENYLIST) {
    assert.equal(CURSOR_NATIVE_TOOL_DENYLIST.includes(tool), true, tool);
    assert.equal(toolIsForbidden(tool), true, tool);
  }
  assert.equal(toolIsForbidden("mcp"), false);
  assert.equal(toolIsForbidden("trouve_qualification_echo"), false);
});

test("red-pixel fixture is a one-pixel RGBA PNG with an opaque red scanline", () => {
  const png = Buffer.from(RED_PIXEL_PNG, "base64");
  assert.equal(png.readUInt32BE(16), 1);
  assert.equal(png.readUInt32BE(20), 1);
  assert.equal(png[25], 6);
  const idat = [];
  for (let offset = 8; offset < png.length;) {
    const length = png.readUInt32BE(offset);
    const type = png.toString("ascii", offset + 4, offset + 8);
    if (type === "IDAT") idat.push(png.subarray(offset + 8, offset + 8 + length));
    offset += 12 + length;
  }
  assert.deepEqual([...inflateSync(Buffer.concat(idat))], [0, 255, 0, 0, 255]);
});

test("subscription-health billing cycles require a real timestamp", () => {
  assert.equal(isNonEmptyTimestamp("2026-08-27T12:00:00Z"), true);
  for (const value of [undefined, null, "", "not-a-date", 123]) {
    assert.equal(isNonEmptyTimestamp(value), false);
  }
});

test("cold-resume conversations require populated run-specific evidence", () => {
  assert.deepEqual(
    parseConversationEvidence(
      JSON.stringify({ messages: [{ text: "TROUVE_CURSOR_PRE_RESTART_OK" }] }),
      "historical conversation",
      "TROUVE_CURSOR_PRE_RESTART_OK",
    ),
    { messages: [{ text: "TROUVE_CURSOR_PRE_RESTART_OK" }] },
  );
  assert.throws(
    () => parseConversationEvidence("[]", "historical conversation", "expected"),
    /omitted expected durable evidence/u,
  );
  assert.throws(
    () => parseConversationEvidence('{"messages":[]}', "historical conversation", "expected"),
    /omitted expected durable evidence/u,
  );
});

test("allowlist qualification accepts only a correlated invalid-argument error", () => {
  const expected = new ConnectRpcError("CreateAgent", 400, {
    code: "invalid_argument",
    message: "unknown tool trouve_qualification_invalid_builtin",
  });
  assert.equal(isUnknownToolValidationError(expected), true);
  assert.equal(
    isUnknownToolValidationError(new ConnectRpcError("CreateAgent", 500, {
      code: "internal",
      message: "unknown tool trouve_qualification_invalid_builtin",
    })),
    false,
  );
  assert.equal(
    isUnknownToolValidationError(new ConnectRpcError("CreateAgent", 400, {
      code: "invalid_argument",
      message: "unrelated validation failure",
    })),
    false,
  );
  assert.equal(isUnknownToolValidationError(new Error(expected.message)), false);
});

test("allowlist qualification anchors confinement to a recognized native tool", async () => {
  const requests = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    requests.push({ path: request.url, body });
    response.setHeader("content-type", "application/json");
    if (request.url?.endsWith("/CreateAgent") && requests.filter(
      (entry) => entry.path?.endsWith("/CreateAgent"),
    ).length === 1) {
      response.end(JSON.stringify({ agentId: "known-native-probe" }));
    } else if (request.url?.endsWith("/CreateAgent")) {
      response.statusCode = 400;
      response.end(JSON.stringify({
        code: "invalid_argument",
        message: "unknown tool trouve_qualification_invalid_builtin",
      }));
    } else {
      response.end("{}");
    }
  });
  const address = await listen(server);
  try {
    const policy = await verifyToolAllowlist(
      { url: `http://127.0.0.1:${address.port}`, token: "test-token" },
      {
        tools: { names: ["mcp"] },
        disallowedTools: [...CURSOR_NATIVE_TOOL_DENYLIST],
        local: { customTools: { trouve_test: {} } },
      },
      1_000,
    );
    assert.equal(policy.known_native_tool_recognized, "shell");
    assert.equal(policy.unknown_tool_rejected_with_invalid_argument, true);
    assert.deepEqual(requests[0].body.options.tools.names, ["shell"]);
    assert.equal(requests[0].body.options.disallowedTools.includes("shell"), false);
    assert.deepEqual(requests[1], {
      path: "/sdk.v1.SdkAgentService/CloseAgent",
      body: { agentId: "known-native-probe" },
    });
    assert.deepEqual(requests[2].body.options.tools.names, [
      "mcp",
      "trouve_qualification_invalid_builtin",
    ]);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("Bridge discovery requires an uncredentialed literal loopback HTTP URL", () => {
  for (const url of ["http://127.0.0.1:43123", "http://127.1.2.3:43123/sdk", "http://[::1]:43123"]) {
    assert.doesNotThrow(() => validateLoopbackBridgeUrl(url), url);
  }
  for (const url of [
    "http://localhost:43123",
    "http://192.168.1.2:43123",
    "https://127.0.0.1:43123",
    "http://user@127.0.0.1:43123",
    "http://127.0.0.1:43123?target=elsewhere",
  ]) {
    assert.throws(() => validateLoopbackBridgeUrl(url), /literal loopback HTTP URL/u, url);
  }
});

test("full qualification bounds total and concurrent callbacks", () => {
  const admission = createCallbackAdmission(2, 1);
  const releaseFirst = admission.tryAcquire();
  assert.equal(typeof releaseFirst, "function");
  assert.equal(admission.tryAcquire(), undefined);
  assert.deepEqual(admission.snapshot(), { total: 1, active: 1 });

  releaseFirst();
  releaseFirst();
  const releaseSecond = admission.tryAcquire();
  assert.equal(typeof releaseSecond, "function");
  assert.equal(admission.tryAcquire(), undefined);
  releaseSecond();
  assert.deepEqual(admission.snapshot(), { total: 2, active: 0 });
});

test("cancelled qualification callbacks settle and release admission", async () => {
  let resolveStarted;
  const started = new Promise((resolve) => {
    resolveStarted = resolve;
  });
  const handlers = new Map([
    [
      "trouve_test_block",
      async (_input, record) => {
        resolveStarted(record);
        await record.cancelled.promise;
        return { value: "cancelled" };
      },
    ],
  ]);
  const callback = await startCallbackServer(handlers, 2_000);
  try {
    const controller = new AbortController();
    const request = fetch(
      `${callback.url}/sdk.v1.SdkCustomToolCallbackService/CallCustomTool`,
      {
        method: "POST",
        headers: {
          authorization: `Bearer ${callback.bearer}`,
          "content-type": "application/json",
        },
        body: JSON.stringify({
          toolName: "trouve_test_block",
          toolCallId: "call-1",
          agentId: "agent-1",
          args: {},
        }),
        signal: controller.signal,
      },
    ).catch((error) => error);
    const record = await started;
    controller.abort();
    await record.cancelled.promise;
    await record.settled.promise;
    assert.deepEqual(callback.admission.snapshot(), { total: 1, active: 0 });
    assert.ok((await request) instanceof Error);
  } finally {
    callback.server.closeAllConnections?.();
    await new Promise((resolve) => callback.server.close(resolve));
  }
});

test("qualification status matching accepts only explicit finished values", () => {
  for (const value of [3, "3", "RUN_LIFECYCLE_STATUS_FINISHED"]) {
    assert.equal(terminalStatusIsFinished(value), true);
  }
  for (const value of ["NOT_FINISHED", "NOT_COMPLETED", "COMPLETED"]) {
    assert.equal(terminalStatusIsFinished(value), false);
  }
});

test("qualification rejects duplicate tool and terminal lifecycle events", () => {
  assert.doesNotThrow(() =>
    assertUniqueToolLifecycle(
      [
        { call_id: "call-1", status: "started" },
        { call_id: "call-1", status: "completed" },
      ],
      "fixture",
    ),
  );
  assert.throws(
    () =>
      assertUniqueToolLifecycle(
        [
          { call_id: "call-1", status: "completed" },
          { call_id: "call-1", status: "completed" },
        ],
        "fixture",
      ),
    /duplicate completed lifecycle event/u,
  );
  assert.throws(
    () =>
      exactTerminalResult(
        [{ result: { status: 3 } }, { result: { status: 3 } }, { done: {} }],
        "fixture",
      ),
    /exactly one result and done/u,
  );
  assert.throws(
    () => exactTerminalResult([{ result: { status: 3 } }, { done: false }], "fixture"),
    /done frame had an invalid envelope/u,
  );
});

test("unterminated Bridge diagnostics retain only a marked bounded suffix", () => {
  const pending = capPendingDiagnostic("x".repeat(32_768));
  assert.equal(pending.truncated, true);
  assert.equal(pending.text.length, 16_384);
  assert.equal(pending.text, "x".repeat(16_384));
});

test("Windows ARM64 is rejected before an asset URL is constructed", () => {
  assert.throws(() => assetName("win32", "arm64"), /unsupported/u);
  assert.equal(
    assetName("win32", "x64"),
    "cursor-sdk-bridge-standalone-win32-x64.tar.gz",
  );
});

test("qualification release assets have independently reviewed checksums", () => {
  assert.equal(
    expectedBridgeChecksum(assetName("linux", "x64")),
    "5357a42d3faa668a3ef25c6669fe576544b032dd17fabbbfa515355cd8d33c19",
  );
  assert.throws(
    () => expectedBridgeChecksum("cursor-sdk-bridge-standalone-plan9-x64.tar.gz"),
    /no reviewed checksum/u,
  );
});

test("only an explicit unimplemented RPC is classified as unsupported", () => {
  assert.equal(
    isUnsupportedRpcMethodError(
      new ConnectRpcError("GetUsage", 501, { code: "unimplemented", message: "missing" }),
      "GetUsage",
    ),
    true,
  );
  for (const error of [
    new ConnectRpcError("GetUsage", 401, { code: "unauthenticated", message: "bad key" }),
    new ConnectRpcError("GetUsage", 500, { code: "internal", message: "outage" }),
    new Error("Cursor SDK Bridge RPC timed out"),
  ]) {
    assert.equal(isUnsupportedRpcMethodError(error, "GetUsage"), false);
  }
});

test("bounded JSON reading rejects an oversized streamed response", async () => {
  const response = new Response(JSON.stringify({ value: "too large" }));
  await assert.rejects(
    readBoundedJsonResponse(response, "fixture", 4),
    /response exceeded 4 bytes/u,
  );
});

test("stream qualification rejects frames after Connect end-stream", async () => {
  const server = createServer((request, response) => {
    request.resume();
    response.writeHead(200, { "content-type": "application/connect+json" });
    response.end(
      Buffer.concat([
        connectTestFrame(0, { message: "accepted" }),
        connectTestFrame(0x02, {}),
        connectTestFrame(0, { message: "late" }),
      ]),
    );
  });
  try {
    const address = await listen(server);
    await assert.rejects(
      serverStream(
        { url: `http://127.0.0.1:${address.port}`, token: "fixture-token" },
        "FixtureService",
        "FixtureStream",
        {},
        2_000,
      ),
      /frame after the Connect end-stream frame/u,
    );
  } finally {
    await new Promise((accept) => server.close(accept));
  }
});

test("child settlement waits have a bounded deadline", async () => {
  const child = new EventEmitter();
  child.exitCode = null;
  child.signalCode = null;

  assert.equal(await waitForChildSettlement(child, 10), false);
  assert.equal(child.listenerCount("error"), 0);
  assert.equal(child.listenerCount("exit"), 0);

  child.exitCode = 0;
  assert.equal(await waitForChildSettlement(child, 10), true);
});

test("timed-out child cleanup settles before surfacing the timeout", async () => {
  const child = new EventEmitter();
  child.exitCode = null;
  child.signalCode = null;
  let killed = false;
  let settled = false;
  child.kill = (signal) => {
    assert.equal(signal, "SIGKILL");
    killed = true;
    setTimeout(() => {
      settled = true;
      child.signalCode = signal;
      child.emit("exit", null, signal);
    }, 10);
    return true;
  };

  await assert.rejects(
    terminateTimedOutChild(child, "fixture child", 100),
    /fixture child timed out$/u,
  );
  assert.equal(killed, true);
  assert.equal(settled, true);
});

test("full qualification requires callback and stream ids to be one-to-one", () => {
  const frame = (callId, status) => ({
    sdkMessage: {
      message: { type: "tool_call", name: "mcp", call_id: callId, status },
    },
  });
  const frames = [
    frame("call-a", "started"),
    frame("call-a", "completed"),
    frame("call-b", "started"),
    frame("call-b", "completed"),
  ];
  const tools = ["tool-a", "tool-b"];
  assert.throws(
    () => inspectToolCalls(
      frames,
      [
        { toolName: "tool-a", toolCallId: "call-a" },
        { toolName: "tool-b", toolCallId: "call-a" },
      ],
      tools,
      false,
      "parallel turn",
    ),
    /not one-to-one/u,
  );
  assert.doesNotThrow(() => inspectToolCalls(
    frames,
    [
      { toolName: "tool-a", toolCallId: "call-a" },
      { toolName: "tool-b", toolCallId: "call-b" },
    ],
    tools,
    false,
    "parallel turn",
  ));
});

test("denied-tool qualification requires the stream to propagate an error terminal", () => {
  const frame = (status) => ({
    sdkMessage: {
      message: { type: "tool_call", name: "mcp", call_id: "call-denied", status },
    },
  });
  const callbacks = [{
    toolName: "trouve_qualification_permission_denied",
    toolCallId: "call-denied",
  }];
  assert.throws(
    () => inspectToolCalls(
      [frame("started"), frame("completed")],
      callbacks,
      ["trouve_qualification_permission_denied"],
      true,
      "denied turn",
    ),
    /did not finish with error stream events/u,
  );
  assert.doesNotThrow(() => inspectToolCalls(
    [frame("started"), frame("error")],
    callbacks,
    ["trouve_qualification_permission_denied"],
    true,
    "denied turn",
  ));
});

test("bounded downloads remove their partial destination", async () => {
  const root = await mkdtemp(join(tmpdir(), "trouve-cursor-download-test-"));
  const destination = join(root, "artifact");
  const server = createServer((_request, response) => {
    response.writeHead(200, {
      "content-type": "application/octet-stream",
      "transfer-encoding": "chunked",
    });
    response.write("01234");
    response.end("56789");
  });
  try {
    const address = await listen(server);
    await assert.rejects(
      download(
        `http://127.0.0.1:${address.port}/artifact`,
        destination,
        new AbortController().signal,
        4,
      ),
      /download exceeded 4 bytes/u,
    );
    await assert.rejects(access(destination, fsConstants.F_OK));
  } finally {
    await new Promise((accept) => server.close(accept));
    await rm(root, { recursive: true, force: true });
  }
});

test(
  "token-file failures terminate the spawned Bridge process",
  { skip: process.platform === "win32" },
  async () => {
    const root = await mkdtemp(join(tmpdir(), "trouve-cursor-start-test-"));
    const stateRoot = join(root, "state");
    const fixture = join(root, "bridge-fixture");
    await mkdir(stateRoot);
    await writeFile(
      fixture,
      `#!/usr/bin/env node
const { writeFileSync } = require("node:fs");
const { join } = require("node:path");
const root = process.env.CURSOR_SDK_BRIDGE_STATE_ROOT;
writeFileSync(join(root, "fixture.pid"), String(process.pid));
process.stderr.write("cursor-sdk-bridge ready " + JSON.stringify({
  schemaVersion: 1,
  transport: "tcp",
  protocol: "connect",
  url: "http://127.0.0.1:9",
  authTokenFile: join(root, "missing-token"),
}) + "\\n");
setInterval(() => {}, 1000);
`,
    );
    await chmod(fixture, 0o755);
    try {
      await assert.rejects(
        startBridge({
          binary: fixture,
          workspace: root,
          stateRoot,
          apiKey: "fixture-api-key",
          callback: {
            bearer: "fixture-callback-token",
            url: "http://127.0.0.1:9",
          },
          timeoutMilliseconds: 2_000,
        }),
        /missing-token|ENOENT/u,
      );
      const pid = Number(await readFile(join(stateRoot, "fixture.pid"), "utf8"));
      assert.throws(
        () => process.kill(pid, 0),
        (error) => error?.code === "ESRCH",
      );
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  },
);

test("spawn failures preserve the original Bridge error", async () => {
  const root = await mkdtemp(join(tmpdir(), "trouve-cursor-spawn-test-"));
  const stateRoot = join(root, "state");
  await mkdir(stateRoot);
  try {
    await assert.rejects(
      startBridge({
        binary: join(root, "missing-bridge"),
        workspace: root,
        stateRoot,
        apiKey: "fixture-api-key",
        callback: {
          bearer: "fixture-callback-token",
          url: "http://127.0.0.1:9",
        },
        timeoutMilliseconds: 2_000,
      }),
      /ENOENT/u,
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test(
  "signal cleanup terminates a detached process group before exit",
  { skip: process.platform === "win32" },
  async () => {
    const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], {
      detached: true,
      stdio: "ignore",
    });
    await new Promise((accept, reject) => {
      child.once("spawn", accept);
      child.once("error", reject);
    });
    const target = new EventEmitter();
    const exits = [];
    const errors = [];
    const signals = installSignalCleanup(() => terminateProcessTree(child), {
      target,
      exit: (code) => exits.push(code),
      report: (error) => errors.push(error),
    });

    target.emit("SIGTERM");
    await signals.completion();
    signals.dispose();

    assert.deepEqual(errors, []);
    assert.deepEqual(exits, [143]);
    assert.throws(
      () => process.kill(child.pid, 0),
      (error) => error?.code === "ESRCH",
    );
  },
);

test(
  "process-tree cleanup is single-flight and never re-signals a settled group",
  { skip: process.platform === "win32" },
  async () => {
    const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], {
      detached: true,
      stdio: "ignore",
    });
    await new Promise((accept, reject) => {
      child.once("spawn", accept);
      child.once("error", reject);
    });

    const termination = terminateProcessTree(child);
    assert.equal(terminateProcessTree(child), termination);
    await termination;
    assert.equal(terminateProcessTree(child), termination);
  },
);

test("repeated signals wait for the same asynchronous cleanup", async () => {
  const target = new EventEmitter();
  let releaseCleanup;
  const cleanupGate = new Promise((accept) => {
    releaseCleanup = accept;
  });
  let cleanupCalls = 0;
  const exits = [];
  const signals = installSignalCleanup(
    async () => {
      cleanupCalls += 1;
      await cleanupGate;
    },
    {
      target,
      exit: (code) => exits.push(code),
      report: assert.fail,
    },
  );

  target.emit("SIGTERM");
  target.emit("SIGTERM");
  await Promise.resolve();
  assert.equal(cleanupCalls, 1);
  assert.deepEqual(exits, []);
  assert.throws(() => signals.throwIfSignalled(), /interrupted by signal/u);

  releaseCleanup();
  await signals.completion();
  assert.deepEqual(exits, [143]);
  signals.dispose();
});

test("a signal received during startup prevents a later Bridge spawn", async () => {
  const root = await mkdtemp(join(tmpdir(), "trouve-cursor-signal-startup-"));
  const stateRoot = join(root, "state");
  await mkdir(stateRoot);
  const target = new EventEmitter();
  const exits = [];
  const signals = installSignalCleanup(async () => {}, {
    target,
    exit: (code) => exits.push(code),
    report: assert.fail,
  });
  target.emit("SIGINT");
  let spawned = false;
  try {
    await assert.rejects(
      startBridge({
        binary: process.execPath,
        workspace: root,
        stateRoot,
        apiKey: "fixture-api-key",
        callback: {
          bearer: "fixture-callback-token",
          url: "http://127.0.0.1:9",
        },
        timeoutMilliseconds: 2_000,
        beforeSpawn: signals.throwIfSignalled,
        onSpawn: () => {
          spawned = true;
        },
      }),
      /interrupted by signal/u,
    );
    await signals.completion();
    assert.equal(spawned, false);
    assert.deepEqual(exits, [130]);
  } finally {
    signals.dispose();
    await rm(root, { recursive: true, force: true });
  }
});
