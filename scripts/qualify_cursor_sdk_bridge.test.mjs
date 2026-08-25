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

import {
  ConnectRpcError,
  assetName,
  assertUniqueToolLifecycle,
  capPendingDiagnostic,
  download,
  exactTerminalResult,
  expectedBridgeChecksum,
  installSignalCleanup,
  isUnsupportedRpcMethodError,
  parseTimeoutSeconds,
  readBoundedJsonResponse,
  startBridge,
  terminalStatusIsFinished,
  terminateProcessTree,
} from "./qualify_cursor_sdk_bridge.mjs";

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

test("timeout parsing rejects values outside Node's timer range", () => {
  assert.equal(parseTimeoutSeconds("300"), 300);
  assert.throws(() => parseTimeoutSeconds("2147484"), /no greater than/u);
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
