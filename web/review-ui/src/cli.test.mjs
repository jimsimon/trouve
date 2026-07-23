import assert from "node:assert/strict";
import test from "node:test";

import {
  cliIsInstalled,
  cliProgressLabel,
  cliVersionLabel,
  formatBytes,
} from "./cli.ts";

test("CLI source remains authoritative when version detection fails", () => {
  const systemCli = {
    id: "codex",
    display_name: "Codex CLI",
    kinds: ["codex-app-server"],
    source: "path",
    update_available: false,
  };
  assert.equal(cliIsInstalled(systemCli), true);
  assert.equal(cliVersionLabel(systemCli), "Installed · system PATH");
  assert.equal(cliIsInstalled({ ...systemCli, source: "none" }), false);
});

test("CLI version label distinguishes managed installs and updates", () => {
  assert.equal(cliVersionLabel({
    id: "codex",
    display_name: "Codex CLI",
    kinds: ["codex-app-server"],
    installed_version: "0.150.0",
    source: "managed",
    latest_version: "0.151.0",
    update_available: true,
  }), "0.150.0 · managed by trouve · 0.151.0 available");
});

test("CLI download progress handles known and unknown totals", () => {
  assert.equal(formatBytes(1_572_864), "1.5 MB");
  assert.equal(cliProgressLabel({
    status: "pending",
    version: "0.151.0",
    received_bytes: 1_048_576,
    total_bytes: 4_194_304,
  }), "Downloading 0.151.0 · 1.0 MB of 4.0 MB · 25%");
  assert.equal(cliProgressLabel({
    status: "pending",
    received_bytes: 2048,
    total_bytes: 0,
  }), "Downloading · 2.0 KB");
});
