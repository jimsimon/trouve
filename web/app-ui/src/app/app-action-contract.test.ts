import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

interface Evidence {
  readonly path: string;
  readonly markers: readonly string[];
}

interface CallbackSurface {
  readonly description: string;
  readonly callbacks: readonly string[];
  readonly evidence: readonly Evidence[];
}

const read = (path: string): string =>
  readFileSync(new URL(path, import.meta.url), "utf8");

describe("team creation lifecycle regressions", () => {
  const source = read("./trouve-app.ts");

  it("seeds team session metadata before the session listing refresh", () => {
    const create = source.indexOf("createdSessionId = team.session_id");
    const provisional = source.indexOf("session = {", create);
    const upsert = source.indexOf("this.#store.upsertSessionMetadata(session)", provisional);
    const refresh = source.indexOf("await this.#protocolClient.sessions()", provisional);
    expect(provisional).toBeGreaterThan(-1);
    expect(upsert).toBeGreaterThan(provisional);
    expect(refresh).toBeGreaterThan(upsert);
    expect(source.slice(provisional, refresh)).toContain('kind: "team"');
  });

  it("invalidates deferred attachment reads when team mode clears attachments", () => {
    expect(source).toMatch(
      /if \(this\.#newSessionKind === "team"\) \{\s*this\.#newSessionAttachmentGeneration \+= 1;\s*this\.#newSessionAttachments = \[\];/,
    );
  });
});

/**
 * Executable inventory of the established application action boundary. Some
 * legacy callbacks collapse into one browser primitive (HTML drag-and-drop,
 * native form state, xterm input/selection), so parity is intentionally
 * recorded by user-facing surface instead of requiring a one-for-one method
 * name in Lit.
 */
const SURFACES: readonly CallbackSurface[] = [
  {
    description: "workspace and session navigation",
    callbacks: [
      "nav-row-clicked",
      "new-session",
      "open-workspace",
      "open-link",
      "workspace-new-session",
      "workspace-drag-data",
      "workspace-drag-acceptable",
      "workspace-dropped",
      "workspace-moved",
      "workspace-closed",
      "open-settings",
      "session-renamed",
      "session-archived",
      "session-deleted",
      "archived-filter-toggled",
    ],
    evidence: [
      {
        path: "./trouve-app.ts",
        markers: [
          "async #openWorkspace()",
          "this.#workspaceOrder.drop",
          "this.#showNewSession",
          "@trouve-open-external",
        ],
      },
      {
        path: "../components/session-list.ts",
        markers: [
          "#toggleArchived(",
          "async #rename(",
          "async #setArchived(",
          "async #delete(",
        ],
      },
      {
        path: "../components/workspace-settings.ts",
        markers: ["pickAndRegisterWorkspace", "closeWorkspace"],
      },
    ],
  },
  {
    description: "account pull-request dashboard",
    callbacks: [
      "open-pull-requests",
      "close-pull-requests",
      "pr-dash-filter-picked",
      "pr-group-toggled",
      "pr-group-drag-data",
      "pr-group-drag-acceptable",
      "pr-group-dropped",
      "pr-group-moved",
      "pr-chat-clicked",
      "pr-fix-clicked",
    ],
    evidence: [
      {
        path: "../components/pull-requests-dashboard.ts",
        markers: [
          "#selectRepository",
          "#toggleGroup",
          "#dragStart",
          "#drop(",
          "#chat(",
          "#fix(",
        ],
      },
      {
        path: "./trouve-app.ts",
        markers: ["#openPullRequestChat", "#fixPullRequestReview"],
      },
    ],
  },
  {
    description: "automations",
    callbacks: [
      "open-automations",
      "close-automations",
      "automation-saved",
      "automation-toggled",
      "automation-run",
      "automation-deleted",
    ],
    evidence: [
      {
        path: "../components/automations-screen.ts",
        markers: [
          "#saveAutomation",
          "#toggleEnabled",
          "#runNow",
          "#deleteAutomation",
          "#refreshAutomations",
        ],
      },
    ],
  },
  {
    description: "title model and GitHub host management",
    callbacks: [
      "title-model-load-picked",
      "title-model-resource-picked",
      "title-model-install",
      "title-model-cancel",
      "github-host-added",
      "github-host-removed",
    ],
    evidence: [
      {
        path: "../components/management-settings-panels.ts",
        markers: [
          "derive_branch_name_from_session_title",
          "title_model_load_behavior",
          "title_model_resource_policy",
          "async #install(cancel:",
          "async #addHost(",
          "async #removeHost(",
        ],
      },
    ],
  },
  {
    description: "MCP settings and the effective session overview",
    callbacks: [
      "mcp-saved",
      "mcp-deleted",
      "mcp-logs-requested",
      "mcp-logs-closed",
    ],
    evidence: [
      {
        path: "../components/management-settings-panels.ts",
        markers: [
          "upsertMcpServer",
          "deleteMcpServer",
          "mcpServerLogs",
          "this.#logsName = \"\"",
        ],
      },
      {
        path: "../components/session-info-panel.ts",
        markers: ["Session overview", "#refreshAll()", "sessionMcpServers"],
      },
    ],
  },
  {
    description: "provider configuration and authentication",
    callbacks: [
      "provider-saved",
      "provider-field-changed",
      "provider-fields-reset",
      "provider-fields-valid",
      "provider-deleted",
      "provider-login",
      "provider-login-response",
    ],
    evidence: [
      {
        path: "../components/provider-settings.ts",
        markers: [
          "#renderPresetFields",
          "#renderCustomFields",
          "upsertProvider",
          "deleteProvider",
          "startProviderLogin",
          "completeProviderLogin",
        ],
      },
    ],
  },
  {
    description: "global defaults, agent personas, and settings routing",
    callbacks: [
      "default-model-picked",
      "default-permission-picked",
      "mode-saved",
      "mode-deleted",
      "mode-model-picked",
      "mode-thinking-picked",
      "close-settings",
    ],
    evidence: [
      {
        path: "../components/persona-settings-panel.ts",
        markers: [
          "setGlobalDefaults",
          "upsertPersona",
          "deletePersona",
        ],
      },
      {
        path: "../components/settings-screen.ts",
        markers: ["SETTINGS_SECTIONS", "services.router.navigate"],
      },
    ],
  },
  {
    description: "managed vendor CLI lifecycle",
    callbacks: ["cli-install", "cli-cancel", "cli-uninstall"],
    evidence: [
      {
        path: "../components/cli-settings.ts",
        markers: ["startCliInstall", "cancelCliInstall", "uninstallCli"],
      },
    ],
  },
  {
    description: "sleep preference",
    callbacks: ["prevent-sleep-while-running-toggled"],
    evidence: [
      {
        path: "../components/settings-screen.ts",
        markers: ["preventSleepWhileRunning", "setGeneralPreferences"],
      },
      {
        path: "../services/desktop-host-coordinator.ts",
        markers: ["setSleepInhibition", "#synchronizeSleepInhibition"],
      },
    ],
  },
  {
    description: "chat presentation preference",
    callbacks: [
      "collapse-sequential-tool-calls-toggled",
      "collapse-thinking-with-tools-toggled",
      "collapse-compaction-with-tools-toggled",
      "collapse-todo-updates-with-tools-toggled",
    ],
    evidence: [
      {
        path: "../components/settings-screen.ts",
        markers: [
          "collapseSequentialToolCalls",
          "collapseThinkingWithTools",
          "collapseCompactionWithTools",
          "collapseTodoUpdatesWithTools",
          "setChatPreferences",
        ],
      },
      {
        path: "../components/thread-screen.ts",
        markers: [
          "#renderVisibleThinking",
          "collapseSequentialToolCalls",
          "collapseThinkingWithTools",
          "collapseCompactionWithTools",
          "collapseTodoUpdatesWithTools",
        ],
      },
    ],
  },
  {
    description: "local model and llama.cpp lifecycle",
    callbacks: [
      "local-search",
      "local-search-filters-changed",
      "local-enabled-toggled",
      "local-runtime-install",
      "local-runtime-cancel",
      "local-runtime-uninstall",
      "local-download",
      "local-cancel",
      "local-delete",
      "local-stop-server",
      "local-restart-server",
      "local-added",
    ],
    evidence: [
      {
        path: "../components/local-model-settings.ts",
        markers: [
          "filterLocalSearchResults",
          "#setSearchFit",
          "#installRuntime",
          "#cancelRuntimeInstall",
          "#uninstallRuntime",
          "#startDownload",
          "#cancelDownload",
          "#deleteModel",
          "#stopServer",
          "#restartServer",
          "#addSearchResult",
        ],
      },
    ],
  },
  {
    description: "appearance",
    callbacks: [
      "appearance-theme-picked",
      "appearance-font-size-picked",
      "appearance-font-picked",
      "appearance-reduce-motion-toggled",
    ],
    evidence: [
      {
        path: "../components/settings-screen.ts",
        markers: [
          "setThemePreference",
          "fontSize:",
          "fontFamily:",
          "reduceMotion:",
        ],
      },
    ],
  },
  {
    description: "notification preferences and test delivery",
    callbacks: ["notify-pref-toggled", "notify-test"],
    evidence: [
      {
        path: "../components/settings-screen.ts",
        markers: [
          "#testNativeNotification",
          "#testWebNotification",
          "setNotificationPreferences",
        ],
      },
    ],
  },
  {
    description: "thread navigation and creation",
    callbacks: ["thread-selected", "new-thread"],
    evidence: [
      {
        path: "../components/thread-screen.ts",
        markers: ["#selectThread(", "#submitNewThread", "#cancelNewThread"],
      },
    ],
  },
  {
    description: "chat cards, approvals, questions, and inspection routing",
    callbacks: [
      "approval-resolved",
      "question-option-toggled",
      "question-other-edited",
      "question-back",
      "question-next",
      "question-skip",
      "tool-toggled",
      "raw-toggled",
      "card-toggled",
      "chat-file-opened",
      "right-tab-changed",
    ],
    evidence: [
      {
        path: "../components/thread-screen.ts",
        markers: [
          "#resolveApproval",
          "#toggleQuestionOption",
          "#submitQuestion",
          "#toggleMessageDisclosure",
          "#toggleRawTool",
          "trouve-open-file",
        ],
      },
      {
        path: "./trouve-app.ts",
        markers: ["#openFile", "#selectInspection"],
      },
    ],
  },
  {
    description: "composer, attachments, completion, and model controls",
    callbacks: [
      "send-message",
      "cancel-turn",
      "attach-file",
      "attachment-removed",
      "paste-image-attempted",
      "slash-filter-changed",
      "at-filter-changed",
      "at-picked",
      "model-filter-changed",
      "composer-mode-changed",
      "composer-model-changed",
      "composer-thinking-changed",
      "composer-permission-changed",
      "composer-context-changed",
      "composer-fast-toggled",
    ],
    evidence: [
      {
        path: "../components/thread-screen.ts",
        markers: [
          "#sendMessage",
          "#cancelTurn",
          "#composerPaste",
          "nativeHost.pickFiles",
          "nativeHost.readClipboardImage",
          "#applyComposerCompletion",
          "#updateThreadSetting",
          "#updateThreadModelOption",
        ],
      },
      {
        path: "../components/composer-completion.ts",
        markers: ["composerCompletionToken", "rankComposerCompletions"],
      },
      {
        path: "../components/model-picker.ts",
        markers: ["trouve-model-picked", "#query"],
      },
    ],
  },
  {
    description: "queued prompts",
    callbacks: [
      "queue-edited",
      "queue-deleted",
      "queue-moved",
      "queue-reordered",
      "queue-send-now",
      "queue-send-now-at",
    ],
    evidence: [
      {
        path: "../components/thread-screen.ts",
        markers: [
          "updateQueuedPrompt",
          "deleteQueuedPrompt",
          "#queueRowKeyDown",
          "#commitQueueKeyboardReorder",
          "#dispatchQueue",
          "#sendQueuedNow",
        ],
      },
    ],
  },
  {
    description: "new-session flow",
    callbacks: [
      "nc-model-changed",
      "nc-workspace-changed",
      "start-new-chat",
      "cancel-new-chat",
    ],
    evidence: [
      {
        path: "./trouve-app.ts",
        markers: [
          "#selectNewSessionWorkspace",
          "createSession({",
          "#closeNewSession",
          "trouve-model-picker",
        ],
      },
    ],
  },
  {
    description: "diff and checkpoint restore",
    callbacks: ["diff-file-selected", "undo-turn", "redo-turn"],
    evidence: [
      {
        path: "../components/inspection-workspace.ts",
        markers: [
          "#renderDiff",
          "#activateDiffFileTreeRow",
        ],
      },
      {
        path: "../components/thread-screen.ts",
        markers: ["#restoreTurnCheckpoint", "restoreCheckpoint(boundary.checkpointId)"],
      },
    ],
  },
  {
    description: "session pull requests",
    callbacks: [
      "create-pr",
      "pr-picked",
      "open-pr-url",
      "open-integrations-settings",
    ],
    evidence: [
      {
        path: "../components/session-pr-panel.ts",
        markers: ["#create(", "#renderPr(", "#openExternal", "#openIntegrationsSettings"],
      },
    ],
  },
  {
    description: "integrated terminal",
    callbacks: [
      "term-key",
      "term-paste",
      "term-copy",
      "term-clipboard-decision",
      "term-search",
      "term-unit-selection",
      "term-open-link",
      "term-mouse",
      "term-wheel",
      "term-resized",
      "term-new-tab",
      "term-tab-picked",
      "term-close-tab",
      "term-restart",
    ],
    evidence: [
      {
        path: "../components/terminal-view.ts",
        markers: [
          "WebLinksAddon",
          "terminal.onData",
          "ResizeObserver",
          "parseOsc52ClipboardRequest",
          "SearchAddon",
        ],
      },
      {
        path: "../components/terminal-panel.ts",
        markers: [
          "#copySelection",
          "#pasteClipboard",
          "#restartActive",
          "#closeTerminal",
          "#resolveClipboardRequest",
          "#terminalResize",
        ],
      },
    ],
  },
  {
    description: "files and chat scroll persistence",
    callbacks: [
      "file-activated",
      "file-opened-externally",
      "chat-position-changed",
    ],
    evidence: [
      {
        path: "../components/inspection-workspace.ts",
        markers: ["#activateFileTreeRow", "#loadFile", "actOnSessionFile"],
      },
      {
        path: "../components/thread-screen.ts",
        markers: ["trouve-chat-position", "restoreBookmark"],
      },
      {
        path: "./trouve-app.ts",
        markers: ["#chatPositionChanged", "setThreadScroll"],
      },
    ],
  },
  {
    description: "desktop close lifecycle",
    callbacks: ["quit-now", "quit-when-idle", "cancel-quit-when-idle"],
    evidence: [
      {
        path: "../services/desktop-host-coordinator.ts",
        markers: ["quitNow", "quitWhenIdle", "cancel", "#quitWhenIdleIfReady"],
      },
      {
        path: "./trouve-app.ts",
        markers: ["#desktopCloseRequested", "#resolveDesktopClose"],
      },
    ],
  },
];

describe("Lit application action contract", () => {
  it("keeps every established action mapped exactly once", () => {
    const callbacks = SURFACES.flatMap((surface) => surface.callbacks);
    expect(new Set(callbacks).size).toBe(callbacks.length);
    expect(callbacks).toHaveLength(146);
  });

  for (const surface of SURFACES) {
    it(`keeps implementation evidence for ${surface.description}`, () => {
      expect(surface.callbacks.length).toBeGreaterThan(0);
      for (const evidence of surface.evidence) {
        const source = read(evidence.path);
        for (const marker of evidence.markers) expect(source).toContain(marker);
      }
    });
  }
});
