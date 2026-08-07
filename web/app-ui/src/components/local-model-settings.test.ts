import { describe, expect, it } from "vitest";

import type {
  ProtocolLocalModelInfo,
  ProtocolLocalSearchResult,
  ProtocolLocalStatus,
} from "../services/protocol-client.js";
import {
  filterLocalSearchResults,
  formatBytes,
  localModelSections,
  localModelHostCopy,
  localModelRequest,
  shouldRefreshLocalStatus,
} from "./local-model-settings.js";

const searchResult = (
  repo: string,
  fits: readonly string[],
): ProtocolLocalSearchResult => ({
  repo,
  downloads: 1,
  likes: 1,
  recommended: 0,
  files: fits.map((fit, index) => ({
    file: `model-${index}.gguf`,
    size_bytes: 1_024,
    quant: "Q4_K_M",
    fit,
    added: false,
  })),
});

const model = (downloadStatus: string): ProtocolLocalModelInfo => ({
  id: "qwen-coder",
  display_name: "Qwen Coder",
  repo: "Qwen/Qwen-Coder-GGUF",
  file: "qwen-coder.Q4_K_M.gguf",
  size_bytes: 4_294_967_296,
  params: "7B",
  context_window: 32_768,
  fit: "gpu",
  notes: "Coding model",
  downloaded: downloadStatus === "none-downloaded",
  download_status: downloadStatus === "none-downloaded" ? "none" : downloadStatus,
  download_bytes: downloadStatus === "pending" ? 1_024 : 0,
  download_error: "",
  custom: false,
});

const status = (
  serverStatus: string,
  models: ProtocolLocalModelInfo[],
): ProtocolLocalStatus => ({
  enabled: true,
  ram_bytes: 34_359_738_368,
  gpus: [{ name: "Test GPU", vram_bytes: 12_884_901_888 }],
  runtime_installed: true,
  runtime_version: "b7000",
  runtime_managed: true,
  runtime_latest_version: "b7000",
  runtime_update_available: false,
  running_model: null,
  server_status: serverStatus,
  models,
});

describe("local model settings helpers", () => {
  it("makes PWA execution location explicit", () => {
    const copy = localModelHostCopy("pwa");
    expect(copy).toContain("remote server host");
    expect(copy).toContain("not on this phone");
    expect(copy).toContain("inference happen on that server host");
    expect(localModelHostCopy("desktop")).toContain("this desktop's trouve server host");
  });

  it("builds trimmed add requests without an empty optional display name", () => {
    expect(localModelRequest(" Qwen/Qwen-GGUF ", " model.Q4.gguf ")).toEqual({
      repo: "Qwen/Qwen-GGUF",
      file: "model.Q4.gguf",
    });
    expect(localModelRequest("org/repo", "model.gguf", "  My Model  ")).toEqual({
      repo: "org/repo",
      file: "model.gguf",
      display_name: "My Model",
    });
  });

  it("formats server hardware and download sizes compactly", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1_024)).toBe("1.0 KiB");
    expect(formatBytes(5 * 1024 ** 3)).toBe("5.0 GiB");
    expect(formatBytes(Number.NaN)).toBe("0 B");
  });

  it("refreshes only while a model or server transition is in flight", () => {
    expect(shouldRefreshLocalStatus(status("starting", [model("none")]))).toBe(true);
    expect(shouldRefreshLocalStatus(status("stopped", [model("pending")]))).toBe(true);
    expect(shouldRefreshLocalStatus(status("running", [model("none-downloaded")]))).toBe(false);
    expect(shouldRefreshLocalStatus(status("stopped", [model("none")]), {
      status: "pending",
      received_bytes: 10,
      total_bytes: 100,
    })).toBe(true);
  });

  it("matches the repo-level local-search fit filters", () => {
    const results = [
      searchResult("gpu-only", ["gpu"]),
      searchResult("cpu-only", ["cpu"]),
      searchResult("large-only", ["too-large"]),
      searchResult("mixed", ["gpu", "too-large"]),
    ];
    expect(filterLocalSearchResults(results, {
      gpu: true,
      cpu: true,
      tooLarge: false,
    }).map(({ repo }) => repo)).toEqual(["gpu-only", "cpu-only", "mixed"]);
    expect(filterLocalSearchResults(results, {
      gpu: false,
      cpu: false,
      tooLarge: true,
    }).map(({ repo }) => repo)).toEqual(["large-only", "mixed"]);
    expect(filterLocalSearchResults(results, {
      gpu: false,
      cpu: false,
      tooLarge: false,
    })).toEqual([]);
    expect(filterLocalSearchResults(results, {
      gpu: true,
      cpu: false,
      tooLarge: false,
    })[1]?.files).toHaveLength(2);
  });

  it("keeps owned, active, and failed models ahead of untouched recommendations", () => {
    const downloaded = model("none-downloaded");
    const pending = { ...model("pending"), id: "pending" };
    const failed = { ...model("failed"), id: "failed", download_error: "network" };
    const recommendation = { ...model("none"), id: "recommended" };
    expect(localModelSections([
      recommendation,
      downloaded,
      pending,
      failed,
    ])).toEqual({
      yours: [downloaded, pending, failed],
      recommended: [recommendation],
    });
  });
});
