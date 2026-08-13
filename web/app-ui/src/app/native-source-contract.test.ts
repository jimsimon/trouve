import { readdirSync, readFileSync } from "node:fs";
import { relative } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const repositoryRoot = fileURLToPath(new URL("../../../../", import.meta.url));

const sourceRoots = [
  "crates/trouve-app",
  "crates/trouve-client-core/src",
  "crates/trouve-desktop-host/src",
  "crates/trouve-desktop-host/tests",
  "crates/trouve-servo-embed-preview/src",
] as const;

/** Every native frontend-host source. Keep this explicit so native UI logic
 * cannot silently grow beside the shared Lit application. */
const auditedSources = [
  "crates/trouve-app/build.rs",
  "crates/trouve-app/src/native_notification.rs",
  "crates/trouve-app/src/opener.rs",
  "crates/trouve-app/src/servo_preview.rs",
  "crates/trouve-app/src/sleep.rs",
  "crates/trouve-app/src/web_preview.rs",
  "crates/trouve-app/src/web_preview_support.rs",
  "crates/trouve-app/src/wry_main.rs",
  "crates/trouve-client-core/src/client.rs",
  "crates/trouve-client-core/src/lib.rs",
  "crates/trouve-client-core/src/protocol_compatibility.rs",
  "crates/trouve-client-core/src/viewmodel.rs",
  "crates/trouve-desktop-host/src/gateway.rs",
  "crates/trouve-desktop-host/src/lib.rs",
  "crates/trouve-desktop-host/tests/openapi_snapshot.rs",
  "crates/trouve-servo-embed-preview/src/main.rs",
  "crates/trouve-servo-embed-preview/src/native_notification.rs",
  "crates/trouve-servo-embed-preview/src/system_opener.rs",
  "crates/trouve-servo-embed-preview/src/web_preview_support.rs",
] as const;

const collectFrontendSources = (path: string): readonly string[] => {
  const entries = readdirSync(`${repositoryRoot}/${path}`, { withFileTypes: true });
  return entries.flatMap((entry) => {
    const child = `${path}/${entry.name}`;
    if (entry.isDirectory()) {
      if (entry.name === "target") return [];
      return collectFrontendSources(child);
    }
    return entry.isFile() && entry.name.endsWith(".rs")
      ? [child]
      : [];
  });
};

const readRepositoryFile = (path: string): string =>
  readFileSync(`${repositoryRoot}/${path}`, "utf8");

const rustEventToWire = {
  ApprovalRequested: "approval.requested",
  ApprovalResolved: "approval.resolved",
  AssistantDelta: "assistant.delta",
  AssistantMessage: "assistant.message",
  AssistantThinking: "assistant.thinking",
  AssistantThinkingCompleted: "assistant.thinking_completed",
  CommandsUpdated: "thread.commands_updated",
  CompactionCompleted: "thread.compaction_completed",
  CompactionFailed: "thread.compaction_failed",
  CompactionStarted: "thread.compaction_started",
  QuestionRequested: "question.requested",
  QuestionResolved: "question.resolved",
  QueueUpdated: "thread.queue_updated",
  SubagentSpawned: "subagent.spawned",
  TodosUpdated: "thread.todos_updated",
  ToolCompleted: "tool.completed",
  ToolOutput: "tool.output",
  ToolRequested: "tool.requested",
  ToolStarted: "tool.started",
  TurnCancelled: "turn.cancelled",
  TurnCapacityAcquired: "turn.capacity_acquired",
  TurnCompleted: "turn.completed",
  TurnFailed: "turn.failed",
  TurnStarted: "turn.started",
  TurnSteered: "turn.steered",
  TurnUsageUpdated: "turn.usage_updated",
  UserMessage: "user.message",
} as const;

describe("native frontend-host source contract", () => {
  it("keeps every native host source in the explicit inventory", () => {
    const discovered = sourceRoots.flatMap(collectFrontendSources).sort();
    expect(discovered).toEqual([...auditedSources].sort());
  });

  it("does not retain a second native product UI", () => {
    expect(auditedSources.some((source) => source.endsWith(".slint"))).toBe(false);
    expect(sourceRoots.some((source) => source.includes("slint"))).toBe(false);
  });

  it("folds every native thread view-model event in the TypeScript projection", () => {
    const rust = readRepositoryFile("crates/trouve-client-core/src/viewmodel.rs");
    const typescript = readRepositoryFile("web/app-ui/src/state/thread-view-model.ts");
    const nativeEvents = new Set(
      [...rust.matchAll(/^\s+Event::([A-Za-z0-9_]+)/gmu)].map((match) => match[1]!),
    );
    const applyStart = typescript.indexOf("  apply(envelope:");
    const applyEnd = typescript.indexOf("\n  private appendItem", applyStart);
    expect(applyStart).toBeGreaterThanOrEqual(0);
    expect(applyEnd).toBeGreaterThan(applyStart);
    const applySource = typescript.slice(applyStart, applyEnd);
    const webEvents = new Set(
      [...applySource.matchAll(/^\s+case "([a-z0-9._-]+)"/gmu)].map((match) => match[1]!),
    );

    expect([...nativeEvents].sort()).toEqual(Object.keys(rustEventToWire).sort());
    expect([...webEvents].sort()).toEqual(Object.values(rustEventToWire).sort());
  });

  it("resolves the repository root used by the inventory", () => {
    expect(relative(repositoryRoot, fileURLToPath(import.meta.url))).toBe(
      "web/app-ui/src/app/native-source-contract.test.ts",
    );
  });
});
