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
  "crates/trouve-slint-code-view",
  "crates/trouve-slint-diff-view",
  "crates/trouve-slint-markdown",
  "crates/trouve-slint-terminal",
] as const;

/** Every retained native-frontend source reviewed in the source audit. Keep
 * this explicit: a newly added Rust/Slint frontend file must receive a web
 * disposition instead of silently falling outside the comparison. */
const auditedSources = [
  "crates/trouve-app/build.rs",
  "crates/trouve-app/src/controller.rs",
  "crates/trouve-app/src/main.rs",
  "crates/trouve-app/src/notify.rs",
  "crates/trouve-app/src/opener.rs",
  "crates/trouve-app/src/render.rs",
  "crates/trouve-app/src/servo_preview.rs",
  "crates/trouve-app/src/sleep.rs",
  "crates/trouve-app/src/theme.rs",
  "crates/trouve-app/src/ui.rs",
  "crates/trouve-app/src/web_preview.rs",
  "crates/trouve-app/src/web_preview_support.rs",
  "crates/trouve-app/src/winstate.rs",
  "crates/trouve-app/ui/app.slint",
  "crates/trouve-app/ui/automations-screen.slint",
  "crates/trouve-app/ui/connectivity-banner.slint",
  "crates/trouve-app/ui/pull-requests-screen.slint",
  "crates/trouve-app/ui/scroll-keys.slint",
  "crates/trouve-app/ui/settings-window.slint",
  "crates/trouve-app/ui/theme.slint",
  "crates/trouve-client-core/src/client.rs",
  "crates/trouve-client-core/src/lib.rs",
  "crates/trouve-client-core/src/viewmodel.rs",
  "crates/trouve-desktop-host/src/gateway.rs",
  "crates/trouve-desktop-host/src/lib.rs",
  "crates/trouve-desktop-host/tests/openapi_snapshot.rs",
  "crates/trouve-servo-embed-preview/src/main.rs",
  "crates/trouve-servo-embed-preview/src/system_opener.rs",
  "crates/trouve-servo-embed-preview/src/web_preview_support.rs",
  "crates/trouve-slint-code-view/build.rs",
  "crates/trouve-slint-code-view/examples/code_view_demo.rs",
  "crates/trouve-slint-code-view/src/lib.rs",
  "crates/trouve-slint-code-view/ui/code-view-window.slint",
  "crates/trouve-slint-code-view/ui/code-view.slint",
  "crates/trouve-slint-diff-view/build.rs",
  "crates/trouve-slint-diff-view/examples/diff_view_demo.rs",
  "crates/trouve-slint-diff-view/src/lib.rs",
  "crates/trouve-slint-diff-view/ui/diff-view-window.slint",
  "crates/trouve-slint-diff-view/ui/diff-view.slint",
  "crates/trouve-slint-markdown/build.rs",
  "crates/trouve-slint-markdown/examples/markdown_demo.rs",
  "crates/trouve-slint-markdown/src/lib.rs",
  "crates/trouve-slint-markdown/ui/markdown-view.slint",
  "crates/trouve-slint-markdown/ui/markdown-window.slint",
  "crates/trouve-slint-terminal/build.rs",
  "crates/trouve-slint-terminal/examples/terminal_demo.rs",
  "crates/trouve-slint-terminal/src/lib.rs",
  "crates/trouve-slint-terminal/ui/terminal-grid.slint",
  "crates/trouve-slint-terminal/ui/terminal-view.slint",
  "crates/trouve-slint-terminal/ui/terminal-window.slint",
] as const;

const collectFrontendSources = (path: string): readonly string[] => {
  const entries = readdirSync(`${repositoryRoot}/${path}`, { withFileTypes: true });
  return entries.flatMap((entry) => {
    const child = `${path}/${entry.name}`;
    if (entry.isDirectory()) {
      if (entry.name === "target") return [];
      return collectFrontendSources(child);
    }
    return entry.isFile() && (entry.name.endsWith(".rs") || entry.name.endsWith(".slint"))
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
  CommandsUpdated: "thread.commands_updated",
  CompactionCompleted: "thread.compaction_completed",
  CompactionStarted: "thread.compaction_started",
  QuestionRequested: "question.requested",
  QuestionResolved: "question.resolved",
  QueueUpdated: "thread.queue_updated",
  TodosUpdated: "thread.todos_updated",
  ToolCompleted: "tool.completed",
  ToolOutput: "tool.output",
  ToolRequested: "tool.requested",
  ToolStarted: "tool.started",
  TurnCancelled: "turn.cancelled",
  TurnCompleted: "turn.completed",
  TurnFailed: "turn.failed",
  TurnStarted: "turn.started",
  TurnUsageUpdated: "turn.usage_updated",
  UserMessage: "user.message",
} as const;

describe("retained Rust/Slint frontend source parity", () => {
  it("keeps every frontend source in the explicit audit inventory", () => {
    const discovered = sourceRoots.flatMap(collectFrontendSources).sort();
    expect(discovered).toEqual([...auditedSources].sort());
  });

  it("records a disposition for every source in the saved audit", () => {
    const audit = readRepositoryFile("docs/design/web-frontend-source-parity-audit.md");
    for (const source of auditedSources) expect(audit).toContain(`\`${source}\``);
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
      "web/app-ui/src/app/rust-source-parity.test.ts",
    );
  });
});
