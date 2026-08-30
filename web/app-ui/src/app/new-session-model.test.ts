import { describe, expect, it } from "vitest";

import type {
  ProtocolAgentPersona,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
} from "../services/protocol-client.js";
import { changeModelOption } from "../components/model-option-controls.js";
import {
  applyNewSessionModelOptionChange,
  beginNewSessionSubmission,
  beginNewSessionOptionLoad,
  canSubmitNewSession,
  canonicalThinkingSelection,
  closeNewSessionSetup,
  completeNewSessionSetup,
  createNewSessionSetupLifecycle,
  createNewSessionOptionsLifecycle,
  createNewSessionThreadRequest,
  createNewSessionThreadRequestFromSnapshot,
  createNewThreadOptionEdits,
  defaultThinkingSelection,
  interruptNewSessionOptionLoad,
  failNewSessionSetup,
  NEW_SESSION_TITLE_FALLBACK,
  NEW_SESSION_TITLE_MAX_LENGTH,
  NEW_THREAD_TITLE_FALLBACK,
  newSessionOptionsAreAuthoritative,
  newSessionOptionsBlockSubmission,
  navigateNewSessionSetup,
  newThreadInheritanceForWorkspace,
  mergeNewSessionModelCatalogs,
  reconcileNewThreadDefaults,
  resolveNewSessionBaseRef,
  resolveNewSessionModel,
  resolveNewThreadDefaults,
  openNewSessionSetup,
  openNewSessionSetupForWorkspace,
  sessionTitleFallback,
  settleNewSessionOptionLoad,
  shouldRestoreFailedNewSessionDraft,
  snapshotNewSessionSubmission,
  thinkingOption,
  thinkingSelectionIsValid,
  threadTitleFallback,
} from "./new-session-model.js";

const model = (
  optionsSchema: unknown,
  id = "provider/model",
): ProtocolModelInfo => ({
  id,
  display_name: "Model",
  context_window: 128_000,
  supports_tools: true,
  options_schema: optionsSchema,
});

const mode = (defaultModel?: string | null): ProtocolAgentPersona => ({
  id: "code",
  display_name: "Engineer",
  system_prompt: "Write code.",
  ...(defaultModel === undefined ? {} : { default_model: defaultModel }),
});

const providers = (defaultModel: string): ProtocolProvidersResponse => ({
  default_model: defaultModel,
  providers: [],
});

describe("new session model", () => {
  it("scopes setup visibility to its opening route and restores failed background drafts", () => {
    const settings = "/settings";
    const inbox = "/inbox";
    const initial = createNewSessionSetupLifecycle();
    const opened = openNewSessionSetup(initial, inbox);

    expect(navigateNewSessionSetup(opened, inbox, false)).toBe(opened);
    const background = navigateNewSessionSetup(opened, settings, true);
    expect(background).toMatchObject({
      status: "background-submitting",
      generation: opened.generation,
    });
    const failed = failNewSessionSetup(background);
    expect(failed.status).toBe("background-failed");
    expect(openNewSessionSetup(failed, settings)).toEqual({
      status: "open",
      routeKey: settings,
      generation: opened.generation,
      idempotencyKey: "",
      createRequest: undefined,
    });
    expect(completeNewSessionSetup(background)).toMatchObject({
      lifecycle: { status: "closed" },
      navigateToSession: false,
    });
    expect(completeNewSessionSetup(opened).navigateToSession).toBe(true);
  });

  it("discards idle drafts on navigation and advances their generation", () => {
    const opened = openNewSessionSetup(
      createNewSessionSetupLifecycle(),
      "inbox",
    );
    const navigated = navigateNewSessionSetup(opened, "settings", false);
    expect(navigated).toEqual({
      status: "closed",
      routeKey: "",
      generation: opened.generation + 1,
      idempotencyKey: "",
      createRequest: undefined,
    });
    expect(closeNewSessionSetup(navigated).generation).toBe(navigated.generation + 1);
  });

  it("reuses the same session-create idempotency key until setup completes", () => {
    const opened = openNewSessionSetup(createNewSessionSetupLifecycle(), "/inbox");
    const originalRequest = {
      workspaceId: "ws-a",
      title: "Original title",
      baseRef: "main",
      fetchLatest: true,
    };
    const submitted = beginNewSessionSubmission(
      opened,
      () => "create-once",
      originalRequest,
    );
    const retried = beginNewSessionSubmission(
      failNewSessionSetup(navigateNewSessionSetup(submitted, "/settings", true)),
      () => "must-not-replace",
      {
        workspaceId: "ws-a",
        title: "Edited title",
        baseRef: "release",
        fetchLatest: false,
      },
    );

    expect(retried.idempotencyKey).toBe("create-once");
    expect(retried.createRequest).toEqual(originalRequest);
    expect(closeNewSessionSetup(retried).idempotencyKey).toBe("");
    expect(closeNewSessionSetup(retried).createRequest).toBeUndefined();
  });

  it("restores a failed draft only when the workspace choice is compatible", () => {
    const submitted = beginNewSessionSubmission(
      openNewSessionSetup(createNewSessionSetupLifecycle(), "/inbox"),
      () => "create-once",
      {
        workspaceId: "ws-a",
        title: "Original title",
        baseRef: "main",
        fetchLatest: true,
      },
    );
    const failed = failNewSessionSetup(
      navigateNewSessionSetup(submitted, "/settings", true),
    );

    expect(shouldRestoreFailedNewSessionDraft(failed, "ws-a", undefined)).toBe(true);
    expect(shouldRestoreFailedNewSessionDraft(failed, "ws-a", "ws-a")).toBe(true);
    expect(shouldRestoreFailedNewSessionDraft(failed, "ws-a", "ws-b")).toBe(false);

    const restored = openNewSessionSetupForWorkspace(
      failed,
      "/settings",
      "ws-a",
      "ws-a",
    );
    expect(restored).toMatchObject({
      lifecycle: { status: "open", idempotencyKey: "create-once" },
      restoringDraft: true,
    });
    const replaced = openNewSessionSetupForWorkspace(
      failed,
      "/settings",
      "ws-a",
      "ws-b",
    );
    expect(replaced).toMatchObject({
      lifecycle: { status: "open", idempotencyKey: "", createRequest: undefined },
      restoringDraft: false,
    });
    expect(replaced.lifecycle.generation).toBeGreaterThan(failed.generation);
  });

  it("uses the bounded first prompt line and removes invisible controls from fallback titles", () => {
    expect(sessionTitleFallback("  Build\n\t the   dashboard\r\n now  ")).toBe(
      "Build",
    );
    expect(sessionTitleFallback("Review\u202ethe diff")).toBe("Review the diff");
    expect(sessionTitleFallback("\n\n  Build   the dashboard\nignore this line")).toBe(
      "Build the dashboard",
    );
  });

  it("returns a nonempty fallback and bounds titles by Unicode code points", () => {
    expect(sessionTitleFallback("\u0000\u202e\t")).toBe(NEW_SESSION_TITLE_FALLBACK);
    const title = sessionTitleFallback(`🙂${"é".repeat(80)}`);
    expect(Array.from(title)).toHaveLength(NEW_SESSION_TITLE_MAX_LENGTH);
    expect(title.startsWith("🙂é")).toBe(true);
  });

  it("derives bounded thread titles from the first prompt line", () => {
    expect(threadTitleFallback("  Review the parser edge cases\nIgnore this line"))
      .toBe("Review the parser edge cases");
    expect(threadTitleFallback("\u0000\u202e\t")).toBe(NEW_THREAD_TITLE_FALLBACK);
    expect(Array.from(threadTitleFallback(`🙂${"é".repeat(80)}`)))
      .toHaveLength(NEW_SESSION_TITLE_MAX_LENGTH);
  });

  it("prefers a valid thinking_level schema and reports its enum and default", () => {
    expect(thinkingOption(model({
      type: "object",
      properties: {
        thinking_level: {
          type: "string",
          enum: ["low", "medium", "high"],
          default: "medium",
        },
        effort: { type: "string", enum: ["minimal", "maximal"] },
      },
    }))).toEqual({
      key: "thinking_level",
      values: ["low", "medium", "high"],
      defaultValue: "medium",
    });
  });

  it("supports effort and skips malformed higher-priority options", () => {
    expect(thinkingOption(model({
      properties: {
        thinking_level: { type: "string", enum: ["low", 2] },
        effort: { type: "string", enum: ["low", "high"] },
      },
    }))).toEqual({ key: "effort", values: ["low", "high"] });
  });

  it("supports Codex-style reasoning option names in product precedence order", () => {
    expect(thinkingOption(model({
      properties: {
        reasoning_effort: {
          type: "string",
          enum: ["low", "medium", "high"],
          default: "high",
        },
        reasoning: { type: "string", enum: ["minimal", "maximal"] },
      },
    }))).toEqual({
      key: "reasoning_effort",
      values: ["low", "medium", "high"],
      defaultValue: "high",
    });
  });

  it("derives and validates fixed thinking budgets from model bounds", () => {
    const option = thinkingOption(model({
      properties: {
        thinking_budget_tokens: {
          type: "integer",
          minimum: 1024,
          maximum: 32768,
          default: 4096,
        },
      },
    }));
    expect(option).toEqual({
      key: "thinking_budget_tokens",
      values: [],
      defaultValue: "4096",
      budget: { minimum: 1024, maximum: 32768 },
    });
    expect(thinkingSelectionIsValid(option, "16384")).toBe(true);
    expect(thinkingSelectionIsValid(option, "1e4")).toBe(true);
    expect(canonicalThinkingSelection(option, "1e4")).toBe("10000");
    expect(defaultThinkingSelection(option, "1e4")).toBe("10000");
    expect(thinkingSelectionIsValid(option, "512")).toBe(false);
    expect(thinkingSelectionIsValid(option, "1.5")).toBe(false);
    expect(defaultThinkingSelection(option)).toBe("4096");
  });

  it("rejects malformed schemas, enums, and defaults", () => {
    expect(thinkingOption(model([]))).toBeUndefined();
    expect(thinkingOption(model({ properties: [] }))).toBeUndefined();
    expect(thinkingOption(model({
      properties: { effort: { type: "number", enum: ["low", "high"] } },
    }))).toBeUndefined();
    expect(thinkingOption(model({
      properties: { effort: { enum: ["low", "low"] } },
    }))).toBeUndefined();
    expect(thinkingOption(model({
      properties: { effort: { enum: ["low", "high"], default: "medium" } },
    }))).toBeUndefined();
  });

  it("resolves explicit, mode, and global models in precedence order", () => {
    expect(resolveNewSessionModel(" explicit/model ", mode("mode/model"), providers("global/model")))
      .toBe("explicit/model");
    expect(resolveNewSessionModel("  ", mode(" mode/model "), providers("global/model")))
      .toBe("mode/model");
    expect(resolveNewSessionModel(undefined, mode(null), providers(" global/model ")))
      .toBe("global/model");
    expect(resolveNewSessionModel(undefined, undefined, providers(" "))).toBeUndefined();
  });

  it("resolves concrete mode, model, thinking, and permission defaults", () => {
    const codeMode: ProtocolAgentPersona = {
      ...mode(),
      default_permission_mode: "allow_list",
      default_thinking_level: "high",
    };
    const globalProviders: ProtocolProvidersResponse = {
      ...providers("provider/global"),
      default_permission_mode: "ask",
      default_thinking_level: "medium",
    };
    const models = [model({
      properties: {
        reasoning_effort: {
          type: "string",
          enum: ["low", "medium", "high"],
          default: "low",
        },
      },
    }, "provider/global")];
    expect(resolveNewThreadDefaults([codeMode], models, globalProviders)).toEqual({
      modeId: "code",
      modelId: "provider/global",
      thinking: "high",
      permissionMode: "allow_list",
      inheritedThinking: "high",
      inheritedPermissionMode: "allow_list",
    });
  });

  it("uses global thinking and permission values when the persona inherits them", () => {
    const globalProviders: ProtocolProvidersResponse = {
      ...providers("provider/global"),
      default_permission_mode: "yolo",
      default_thinking_level: "high",
    };
    const models = [model({
      properties: {
        reasoning_effort: {
          type: "string",
          enum: ["low", "medium", "high"],
          default: "low",
        },
      },
    }, "provider/global")];

    expect(resolveNewThreadDefaults([mode()], models, globalProviders)).toMatchObject({
      thinking: "high",
      permissionMode: "yolo",
      inheritedThinking: "high",
      inheritedPermissionMode: "yolo",
    });
  });

  it("applies and serializes a global fixed thinking budget for new sessions", () => {
    const modelInfo = model({
      properties: {
        thinking_budget_tokens: {
          type: "integer",
          minimum: 1024,
          maximum: 32768,
          default: 4096,
        },
      },
    }, "provider/fixed");
    const globalProviders: ProtocolProvidersResponse = {
      ...providers(modelInfo.id),
      default_thinking_level: "16384",
    };
    const defaults = resolveNewThreadDefaults([mode()], [modelInfo], globalProviders);
    expect(defaults).toMatchObject({
      modelId: modelInfo.id,
      thinking: "16384",
      inheritedThinking: "16384",
    });
    expect(createNewSessionThreadRequest({
      sessionId: "session-1",
      mode: defaults.modeId,
      model: defaults.modelId,
      thinking: "8192",
      inheritedThinking: "16384",
      modelInfo,
    })).toMatchObject({
      model_options: { thinking_budget_tokens: 8192 },
    });
  });

  it("canonicalizes inherited exponent-form fixed budgets", () => {
    const modelInfo = model({
      properties: {
        thinking_budget_tokens: {
          type: "integer",
          minimum: 1024,
          maximum: 32768,
        },
      },
    }, "provider/fixed");
    const defaults = resolveNewThreadDefaults([mode()], [modelInfo], {
      ...providers(modelInfo.id),
      default_thinking_level: "1e4",
    });
    expect(defaults).toMatchObject({
      thinking: "10000",
      inheritedThinking: "10000",
    });
  });

  it("authorizes inherited defaults only for the workspace that supplied the catalog", () => {
    const defaults = resolveNewThreadDefaults(
      [mode()],
      [model({
        properties: {
          thinking_level: { type: "string", enum: ["low", "high"] },
        },
      }, "provider/global")],
      {
        ...providers("provider/global"),
        default_permission_mode: "yolo",
        default_thinking_level: "high",
      },
    );

    expect(newThreadInheritanceForWorkspace(defaults, "ws-old", "ws-new")).toEqual({
      inheritedThinking: undefined,
      inheritedPermissionMode: undefined,
    });
    expect(newThreadInheritanceForWorkspace(defaults, "", "ws-new")).toEqual({
      inheritedThinking: undefined,
      inheritedPermissionMode: undefined,
    });
    expect(newThreadInheritanceForWorkspace(defaults, "ws-new", "ws-new")).toEqual({
      inheritedThinking: "high",
      inheritedPermissionMode: "yolo",
    });
  });

  it("preserves pending edits while adopting refreshed defaults for untouched fields", () => {
    const models = [model({
      properties: {
        thinking_level: { type: "string", enum: ["low", "high"] },
      },
    }, "provider/global")];
    const refreshed = reconcileNewThreadDefaults(
      {
        modeId: "code",
        modelId: "provider/global",
        thinking: "low",
        permissionMode: "ask",
      },
      [mode()],
      models,
      {
        ...providers("provider/global"),
        default_permission_mode: "yolo",
        default_thinking_level: "high",
      },
      {
        ...createNewThreadOptionEdits(),
        permission: true,
      },
    );

    expect(refreshed).toMatchObject({
      thinking: "high",
      inheritedThinking: "high",
      permissionMode: "ask",
      inheritedPermissionMode: undefined,
    });
  });

  it("tracks required loads, authoritative refreshes, and timeout fallback per workspace", () => {
    const current = {
      lifecycle: createNewSessionOptionsLifecycle(),
      edits: { mode: true, model: true, thinking: true, permission: true },
      inheritedThinking: "high",
      inheritedPermissionMode: "yolo" as const,
    };

    const reconnectBeforeReady = beginNewSessionOptionLoad(
      current,
      "workspace-1",
      true,
    );
    expect(reconnectBeforeReady.lifecycle).toEqual({
      status: "loading",
      workspaceId: "workspace-1",
      catalogWorkspaceId: "",
    });
    expect(newSessionOptionsBlockSubmission(reconnectBeforeReady.lifecycle)).toBe(true);
    expect(reconnectBeforeReady.edits).toEqual(current.edits);
    expect(interruptNewSessionOptionLoad(reconnectBeforeReady.lifecycle).status)
      .toBe("failed");

    const ready = settleNewSessionOptionLoad(
      reconnectBeforeReady.lifecycle,
      "workspace-1",
      "ready",
    );
    const refresh = beginNewSessionOptionLoad(
      { ...current, lifecycle: ready },
      "workspace-1",
      true,
    );
    expect(refresh.lifecycle.status).toBe("refreshing");
    expect(newSessionOptionsAreAuthoritative(refresh.lifecycle, "workspace-1"))
      .toBe(true);
    expect(newSessionOptionsBlockSubmission(refresh.lifecycle)).toBe(false);
    const refreshTimedOut = settleNewSessionOptionLoad(
      refresh.lifecycle,
      "workspace-1",
      "timed-out",
    );
    expect(refreshTimedOut.status).toBe("timed-out");
    expect(newSessionOptionsAreAuthoritative(refreshTimedOut, "workspace-1"))
      .toBe(true);
    const lateRefreshFailure = settleNewSessionOptionLoad(
      refreshTimedOut,
      "workspace-1",
      "failed",
    );
    expect(lateRefreshFailure.status).toBe("failed");
    expect(newSessionOptionsAreAuthoritative(lateRefreshFailure, "workspace-1"))
      .toBe(true);

    const workspaceChange = beginNewSessionOptionLoad(
      { ...current, lifecycle: ready },
      "workspace-2",
      false,
    );
    expect(workspaceChange).toEqual({
      lifecycle: {
        status: "loading",
        workspaceId: "workspace-2",
        catalogWorkspaceId: "",
      },
      edits: createNewThreadOptionEdits(),
      inheritedThinking: undefined,
      inheritedPermissionMode: undefined,
    });
    const timedOut = settleNewSessionOptionLoad(
      workspaceChange.lifecycle,
      "workspace-2",
      "timed-out",
    );
    expect(timedOut.status).toBe("timed-out");
    expect(newSessionOptionsAreAuthoritative(timedOut, "workspace-2")).toBe(false);
    expect(newSessionOptionsBlockSubmission(timedOut)).toBe(false);
  });

  it("blocks submission while required options, attachments, or submission are pending", () => {
    const ready = {
      sessionPending: false,
      optionsBlocking: false,
      attachmentPending: false,
    };
    expect(canSubmitNewSession(ready)).toBe(true);
    for (const pending of [
      "sessionPending",
      "optionsBlocking",
      "attachmentPending",
    ] as const) {
      expect(canSubmitNewSession({ ...ready, [pending]: true })).toBe(false);
    }
  });

  it("keeps the pre-await submission options when live form state changes", async () => {
    const selectedModel = model({
      properties: {
        thinking_level: {
          type: "string",
          enum: ["low", "high"],
          default: "high",
        },
      },
    }, "provider/selected");
    const input = {
      selections: {
        modeId: "review",
        modelId: selectedModel.id,
        thinking: "low",
        permissionMode: "ask",
      },
      modelOptions: {},
      modes: [{ ...mode(), id: "review" }],
      providers: providers("provider/default"),
      selectableModels: [selectedModel],
      inheritedThinking: "high",
      inheritedPermissionMode: "yolo",
      optionsAuthoritative: true,
      edits: createNewThreadOptionEdits(),
    };
    const submission = snapshotNewSessionSubmission(input);

    await Promise.resolve();
    input.selections.modeId = "code";
    input.selections.modelId = "provider/changed";
    input.selections.thinking = "high";
    input.selections.permissionMode = "yolo";
    input.inheritedThinking = "low";
    input.inheritedPermissionMode = "ask";
    input.selectableModels = [];
    input.optionsAuthoritative = false;

    expect(createNewSessionThreadRequestFromSnapshot({
      sessionId: "session-1",
      title: "Generated title",
      snapshot: submission,
    })).toEqual({
      session_id: "session-1",
      title: "Generated title",
      mode: "review",
      model: "provider/selected",
      permission_mode: "ask",
      model_options: { thinking_level: "low" },
    });
  });

  it("omits synthesized persona and model overrides but keeps displayed permission after timeout", () => {
    const snapshot = snapshotNewSessionSubmission({
      selections: {
        modeId: "code",
        modelId: "provider/fallback",
        thinking: "high",
        permissionMode: "yolo",
      },
      modelOptions: {},
      modes: [mode("provider/fallback")],
      providers: providers("provider/fallback"),
      selectableModels: [model({}, "provider/fallback")],
      inheritedThinking: undefined,
      inheritedPermissionMode: undefined,
      optionsAuthoritative: false,
      edits: createNewThreadOptionEdits(),
    });
    expect(createNewSessionThreadRequestFromSnapshot({
      sessionId: "session-1",
      title: "Fallback title",
      snapshot,
    })).toEqual({
      session_id: "session-1",
      title: "Fallback title",
      permission_mode: "yolo",
    });
  });

  it("serializes explicit permission edits when catalog loading falls back", () => {
    const snapshot = snapshotNewSessionSubmission({
      selections: {
        modeId: "code",
        modelId: "provider/fallback",
        thinking: "high",
        permissionMode: "ask",
      },
      modelOptions: {},
      edits: { ...createNewThreadOptionEdits(), permission: true },
      modes: [mode("provider/fallback")],
      providers: providers("provider/fallback"),
      selectableModels: [model({}, "provider/fallback")],
      inheritedThinking: undefined,
      inheritedPermissionMode: undefined,
      optionsAuthoritative: false,
    });

    expect(createNewSessionThreadRequestFromSnapshot({
      sessionId: "session-1",
      title: "Fallback title",
      snapshot,
    })).toEqual({
      session_id: "session-1",
      title: "Fallback title",
      permission_mode: "ask",
    });
  });

  it("pins the model when degraded metadata retains model-specific options", () => {
    const selectedModel = model({
      properties: {
        temperature: { type: "number", minimum: 0, maximum: 1 },
      },
    }, "provider/selected");
    const snapshot = snapshotNewSessionSubmission({
      selections: {
        modeId: "code",
        modelId: selectedModel.id,
        thinking: "",
        permissionMode: "ask",
      },
      modelOptions: changeModelOption({}, { key: "temperature", value: 0.25 }),
      edits: createNewThreadOptionEdits(),
      modes: [mode(selectedModel.id)],
      providers: providers(selectedModel.id),
      selectableModels: [selectedModel],
      inheritedThinking: undefined,
      inheritedPermissionMode: undefined,
      optionsAuthoritative: false,
    });

    expect(createNewSessionThreadRequestFromSnapshot({
      sessionId: "session-1",
      title: "Thread",
      snapshot,
    })).toEqual({
      session_id: "session-1",
      title: "Thread",
      model: selectedModel.id,
      model_options: { temperature: 0.25 },
      permission_mode: "ask",
    });
  });

  it("uses live availability without replacing authoritative static metadata", () => {
    const staticModel = model({}, "provider/static");
    const unavailableStatic = model({}, "provider/unavailable");
    const discoveredDefault = model({
      properties: {
        thinking_level: { type: "string", enum: ["low", "high"], default: "low" },
      },
    }, "provider/discovered");
    const liveStatic = model({ properties: { effort: { enum: ["max"] } } }, "provider/static");

    expect(mergeNewSessionModelCatalogs(
      [staticModel, unavailableStatic],
      [discoveredDefault, liveStatic],
      true,
    )).toEqual([discoveredDefault, staticModel]);
    expect(mergeNewSessionModelCatalogs(
      [staticModel, unavailableStatic],
      [discoveredDefault, liveStatic],
      true,
      "provider/unavailable",
    )).toEqual([discoveredDefault, staticModel, unavailableStatic]);
    expect(mergeNewSessionModelCatalogs([staticModel], [], false)).toEqual([
      staticModel,
    ]);
    expect(mergeNewSessionModelCatalogs([staticModel], [], true)).toEqual([]);

    expect(reconcileNewThreadDefaults(
      {
        modeId: "code",
        modelId: "provider/discovered",
        thinking: "low",
        permissionMode: "ask",
      },
      [mode()],
      [staticModel],
      providers("provider/static"),
      { ...createNewThreadOptionEdits(), model: true },
      [staticModel, discoveredDefault],
    )).toMatchObject({
      modelId: "provider/discovered",
      thinking: "low",
    });
  });

  it("always serializes the displayed permission when defaults are degraded", () => {
    const snapshot = snapshotNewSessionSubmission({
      selections: {
        modeId: "code",
        modelId: "provider/model",
        thinking: "",
        permissionMode: "ask",
      },
      modelOptions: {},
      edits: createNewThreadOptionEdits(),
      modes: [mode()],
      providers: providers("provider/model"),
      selectableModels: [model({})],
      inheritedPermissionMode: undefined,
      inheritedThinking: undefined,
      optionsAuthoritative: false,
    });

    expect(createNewSessionThreadRequestFromSnapshot({
      sessionId: "session-1",
      title: "Thread",
      snapshot,
    })).toMatchObject({ permission_mode: "ask" });
  });

  it("replaces an untouched configured default that live discovery removed", () => {
    const unavailableDefault = model({}, "provider/unavailable");
    const available = model({}, "provider/available");

    expect(reconcileNewThreadDefaults(
      {
        modeId: "code",
        modelId: "provider/unavailable",
        thinking: "",
        permissionMode: "ask",
      },
      [mode("provider/unavailable")],
      [unavailableDefault, available],
      providers("provider/unavailable"),
      createNewThreadOptionEdits(),
      [available],
    )).toMatchObject({
      modelId: "provider/available",
    });
  });

  it("emits displayed schema and safety fallbacks when metadata is unavailable", () => {
    const modelInfo = model({
      properties: {
        thinking_level: { type: "string", enum: ["low", "high"], default: "low" },
      },
    });
    const defaults = resolveNewThreadDefaults([], [modelInfo], undefined);
    expect(defaults).toMatchObject({
      thinking: "low",
      permissionMode: "ask",
      inheritedThinking: undefined,
      inheritedPermissionMode: undefined,
    });
    expect(createNewSessionThreadRequest({
      sessionId: "session-1",
      mode: defaults.modeId,
      model: defaults.modelId,
      permissionMode: defaults.permissionMode,
      thinking: defaults.thinking,
      modelInfo,
    })).toEqual({
      session_id: "session-1",
      mode: "code",
      model: "provider/model",
      permission_mode: "ask",
      model_options: { thinking_level: "low" },
    });
  });

  it("falls back to an advertised model when inherited defaults are stale", () => {
    const available = model({}, "provider/available");
    expect(resolveNewThreadDefaults(
      [mode("provider/stale-mode")],
      [available],
      providers("provider/stale-global"),
    ).modelId).toBe("provider/available");
    expect(resolveNewThreadDefaults(
      [mode(null)],
      [available],
      providers("provider/stale-global"),
    ).modelId).toBe("provider/available");
    expect(resolveNewThreadDefaults([], [], providers("provider/stale-global")).modelId)
      .toBe("");

    const cursorDefault = model({}, "cursor/default");
    const cursorFable = model({}, "cursor/claude-fable-5");
    expect(resolveNewThreadDefaults(
      [mode(null)],
      [cursorFable, cursorDefault],
      providers("provider/stale-global"),
    ).modelId).toBe("cursor/default");
  });

  it("chooses an explicit base, repository default, then conventional trunks", () => {
    expect(resolveNewSessionBaseRef(["feature", "master", "main"], "", "main")).toBe("main");
    expect(resolveNewSessionBaseRef(["feature", "master", "main"], "master", "main")).toBe("master");
    expect(resolveNewSessionBaseRef(["feature", "master", "main"], "", "deadbeef")).toBe("main");
    expect(resolveNewSessionBaseRef(["feature", "master"], "", "deadbeef")).toBe("master");
    expect(resolveNewSessionBaseRef(["feature", "main"])).toBe("main");
    expect(resolveNewSessionBaseRef(["feature", "master"])).toBe("master");
    expect(resolveNewSessionBaseRef(["feature"])).toBe("HEAD");
    expect(resolveNewSessionBaseRef(["main"], "missing", "main")).toBe("main");
  });

  it("composes a complete request with an advertised thinking override", () => {
    const modelInfo = model({
      properties: {
        effort: { type: "string", enum: ["low", "high"], default: "low" },
      },
    });
    expect(createNewSessionThreadRequest({
      sessionId: " session-1 ",
      title: " Review parser edge cases ",
      mode: " plan ",
      model: "provider/model",
      permissionMode: "allow_list",
      thinking: " high ",
      modelInfo,
    })).toEqual({
      session_id: "session-1",
      title: "Review parser edge cases",
      mode: "plan",
      model: "provider/model",
      permission_mode: "allow_list",
      model_options: { effort: "high" },
    });
  });

  it("keeps matching thinking and permission selections server-inherited", () => {
    const modelInfo = model({
      properties: {
        thinking_level: { type: "string", enum: ["low", "high"], default: "low" },
      },
    });
    expect(createNewSessionThreadRequest({
      sessionId: "session-1",
      mode: "code",
      permissionMode: "yolo",
      inheritedPermissionMode: "yolo",
      thinking: "high",
      inheritedThinking: "high",
      modelInfo,
    })).toEqual({
      session_id: "session-1",
      mode: "code",
    });
  });

  it("restores inherited thinking provenance when an override returns to default", () => {
    const reset = applyNewSessionModelOptionChange({
      modelOptions: { effort: "low" },
      thinking: "low",
      inheritedThinking: undefined,
      change: { key: "effort", value: undefined },
      defaults: { thinking: "high", inheritedThinking: "high" },
    });
    expect(reset).toEqual({
      modelOptions: {},
      thinking: "high",
      inheritedThinking: "high",
      thinkingEdit: false,
    });

    const modelInfo = model({
      properties: {
        effort: { type: "string", enum: ["low", "high"], default: "low" },
      },
    });
    expect(createNewSessionThreadRequest({
      sessionId: "session-1",
      thinking: reset.thinking,
      ...(reset.inheritedThinking === undefined
        ? {}
        : { inheritedThinking: reset.inheritedThinking }),
      modelOptions: reset.modelOptions,
      modelInfo,
    })).toEqual({ session_id: "session-1" });
  });

  it("keeps unrelated model options without pinning reset thinking across refreshes", () => {
    const reset = applyNewSessionModelOptionChange({
      modelOptions: changeModelOption(
        { effort: "low" },
        { key: "temperature", value: 0.7 },
      ),
      thinking: "low",
      inheritedThinking: undefined,
      change: { key: "effort", value: undefined },
      defaults: { thinking: "high", inheritedThinking: "high" },
    });
    expect(reset).toEqual({
      modelOptions: { temperature: 0.7 },
      thinking: "high",
      inheritedThinking: "high",
      thinkingEdit: false,
    });

    const modelInfo = model({
      properties: {
        effort: { type: "string", enum: ["low", "high"], default: "low" },
        temperature: { type: "number", minimum: 0, maximum: 1 },
      },
    });
    const refreshed = reconcileNewThreadDefaults(
      {
        modeId: "code",
        modelId: modelInfo.id,
        thinking: reset.thinking,
        permissionMode: "ask",
      },
      [{ ...mode(modelInfo.id), default_thinking_level: "low" }],
      [modelInfo],
      providers(modelInfo.id),
      { ...createNewThreadOptionEdits(), thinking: reset.thinkingEdit },
    );
    expect(refreshed).toMatchObject({ thinking: "low", inheritedThinking: "low" });
    expect(createNewSessionThreadRequest({
      sessionId: "session-1",
      thinking: refreshed.thinking,
      ...(refreshed.inheritedThinking === undefined
        ? {}
        : { inheritedThinking: refreshed.inheritedThinking }),
      modelOptions: reset.modelOptions,
      modelInfo,
    })).toEqual({
      session_id: "session-1",
      model_options: { temperature: 0.7 },
    });
  });

  it("omits empty fields and unadvertised thinking values", () => {
    const modelInfo = model({
      properties: { thinking_level: { enum: ["low", "high"] } },
    });
    expect(createNewSessionThreadRequest({
      sessionId: "session-1",
      mode: " ",
      model: null,
      permissionMode: null,
      thinking: "medium",
      modelInfo,
    })).toEqual({ session_id: "session-1" });
  });

  it("does not apply thinking metadata from a different model", () => {
    expect(createNewSessionThreadRequest({
      sessionId: "session-1",
      model: "provider/selected",
      thinking: "high",
      modelInfo: model({
        properties: { effort: { enum: ["low", "high"] } },
      }, "provider/other"),
    })).toEqual({
      session_id: "session-1",
      model: "provider/selected",
    });
  });

  it("rejects an empty required session id", () => {
    expect(() => createNewSessionThreadRequest({ sessionId: " \n " }))
      .toThrow(/nonempty session id/);
  });
});
