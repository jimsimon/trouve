import { describe, expect, it } from "vitest";

import type {
  ProtocolCliInfo,
  ProtocolCliInstallStatus,
} from "../services/protocol-client.js";
import {
  cliIsInstalled,
  cliPrimaryActionLabel,
  cliProgressLabel,
  cliProgressPercent,
  cliSourceLabel,
  cliVersionLabel,
  formatCliBytes,
  pendingCliIds,
  shouldPollCliInstalls,
} from "./cli-settings-model.js";

const cli = (overrides: Partial<ProtocolCliInfo> = {}): ProtocolCliInfo => ({
  id: "codex",
  display_name: "Codex CLI",
  kinds: ["codex-app-server"],
  source: "none",
  update_available: false,
  ...overrides,
});

const status = (
  state: string,
  received = 0,
  total = 0,
): ProtocolCliInstallStatus => ({
  status: state,
  received_bytes: received,
  total_bytes: total,
});

describe("CLI settings model", () => {
  it("treats managed and PATH sources as installed even without a version", () => {
    expect(cliIsInstalled(cli({ source: "managed" }))).toBe(true);
    expect(cliIsInstalled(cli({ source: "path" }))).toBe(true);
    expect(cliIsInstalled(cli({ source: "none" }))).toBe(false);
    expect(cliSourceLabel(cli({ source: "future" }))).toBe("Unknown source");
  });

  it("labels managed versions, updates, and primary actions", () => {
    const installed = cli({
      source: "managed",
      installed_version: "0.150.0",
      latest_version: "0.151.0",
      update_available: true,
    });
    expect(cliVersionLabel(installed)).toBe(
      "0.150.0 · managed by trouve · 0.151.0 available",
    );
    expect(cliPrimaryActionLabel(installed)).toBe("Update");
    expect(cliPrimaryActionLabel(cli({ source: "path" }))).toBe("Reinstall");
    expect(cliPrimaryActionLabel(cli())).toBe("Install");
  });

  it("formats known and indeterminate download progress", () => {
    expect(formatCliBytes(1_572_864)).toBe("1.5 MiB");
    expect(cliProgressPercent(status("pending", 75, 100))).toBe(75);
    expect(cliProgressPercent(status("pending", 200, 100))).toBe(100);
    expect(cliProgressPercent(status("pending", 50, 0))).toBeUndefined();
    expect(cliProgressLabel(status("pending", 1_048_576, 2_097_152))).toBe(
      "Downloading · 1.0 MiB of 2.0 MiB · 50%",
    );
    expect(cliProgressLabel(status("pending"))).toBe("Preparing download…");
    expect(cliProgressLabel(status("pending", 1_048_576, 2_097_152), 524_288)).toBe(
      "Downloading · 1.0 MiB of 2.0 MiB · 50% · 524 kB/s",
    );
  });

  it("selects only pending CLI ids for progress polling", () => {
    const statuses = new Map<string, ProtocolCliInstallStatus>([
      ["codex", status("pending")],
      ["claude", status("success")],
      ["cursor-agent", status("failed")],
    ]);
    expect(pendingCliIds(statuses)).toEqual(["codex"]);
    expect(shouldPollCliInstalls(statuses)).toBe(true);
  });

  it("keeps polling until every install reaches a terminal state", () => {
    const statuses = new Map([["codex", status("pending")]]);
    expect(shouldPollCliInstalls(statuses)).toBe(true);
    expect(shouldPollCliInstalls(new Map())).toBe(false);
  });
});
