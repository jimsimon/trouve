import assert from "node:assert/strict";
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
  download,
  readBoundedJsonResponse,
  startBridge,
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
