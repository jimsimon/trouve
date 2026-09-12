import { describe, expect, it } from "vitest";

import {
  cliIsInstalled,
  cliProgressLabel,
  cliVersionLabel,
  formatBytes,
  type CliInfo,
} from "./cli.js";

describe("CLI runtime presentation", () => {
  it("CLI source remains authoritative when version detection fails", () => {
    const systemCli: CliInfo = {
      id: "codex",
      display_name: "Codex CLI",
      kinds: ["codex-app-server"],
      source: "path",
      update_available: false,
    };
    expect(cliIsInstalled(systemCli)).toBe(true);
    expect(cliVersionLabel(systemCli)).toBe("Installed · system PATH");
    expect(cliIsInstalled({ ...systemCli, source: "none" })).toBe(false);
  });

  it("CLI version label distinguishes managed installs and updates", () => {
    expect(
      cliVersionLabel({
        id: "codex",
        display_name: "Codex CLI",
        kinds: ["codex-app-server"],
        installed_version: "0.150.0",
        source: "managed",
        latest_version: "0.151.0",
        update_available: true,
      }),
    ).toBe("0.150.0 · managed by trouve · 0.151.0 available");
  });

  it("CLI download progress handles known and unknown totals", () => {
    expect(formatBytes(1_572_864)).toBe("1.5 MB");
    expect(
      cliProgressLabel({
        status: "pending",
        version: "0.151.0",
        received_bytes: 1_048_576,
        total_bytes: 4_194_304,
      }),
    ).toBe("Downloading 0.151.0 · 1.0 MB of 4.0 MB · 25%");
    expect(
      cliProgressLabel({
        status: "pending",
        received_bytes: 2048,
        total_bytes: 0,
      }),
    ).toBe("Downloading · 2.0 KB");
  });
});
