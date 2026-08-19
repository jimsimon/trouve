import { describe, expect, it, vi } from "vitest";

import type {
  ProtocolAgentPersona,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
} from "../services/protocol-client.js";
import {
  beginNewSessionOptionLoad,
  canSubmitNewSession,
  createNewSessionThreadRequest,
  createNewThreadOptionEdits,
  NEW_SESSION_TITLE_FALLBACK,
  NEW_SESSION_TITLE_MAX_LENGTH,
  NEW_THREAD_TITLE_FALLBACK,
  newThreadInheritanceForWorkspace,
  mergeNewSessionModelCatalogs,
  reconcileNewThreadDefaults,
  resolveNewSessionBaseRef,
  resolveNewSessionModel,
  resolveNewThreadDefaults,
  sessionTitleFallback,
  snapshotNewSessionSubmission,
  thinkingOption,
  threadTitleFallback,
  withNewSessionOptionsTimeout,
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
        thinking_level: {
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
        thinking_level: {
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

  it("preserves options for reconnect loads and resets them for ordinary loads", () => {
    const current = {
      optionsWorkspaceId: "workspace-1",
      blocksSubmission: true,
      edits: { mode: true, model: true, thinking: true, permission: true },
      inheritedThinking: "high",
      inheritedPermissionMode: "yolo" as const,
    };

    const preserved = beginNewSessionOptionLoad(current, true);
    expect(preserved).toEqual({ ...current, blocksSubmission: false });
    expect(preserved).not.toBe(current);
    expect(preserved.edits).not.toBe(current.edits);
    expect(beginNewSessionOptionLoad(current, false)).toEqual({
      optionsWorkspaceId: "",
      blocksSubmission: true,
      edits: createNewThreadOptionEdits(),
      inheritedThinking: undefined,
      inheritedPermissionMode: undefined,
    });
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

  it("bounds required option loads that never settle", async () => {
    vi.useFakeTimers();
    try {
      const pending = withNewSessionOptionsTimeout(new Promise<never>(() => undefined), 100);
      const rejected = expect(pending).rejects.toThrow("New session options timed out.");
      await vi.advanceTimersByTimeAsync(100);
      await rejected;
    } finally {
      vi.useRealTimers();
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
      modes: [{ ...mode(), id: "review" }],
      providers: providers("provider/default"),
      selectableModels: [selectedModel],
      inheritedThinking: "high",
      inheritedPermissionMode: "yolo",
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

    expect(createNewSessionThreadRequest({
      sessionId: "session-1",
      mode: submission.modeId,
      model: submission.modelId,
      thinking: submission.thinking,
      permissionMode: submission.permissionMode,
      ...(submission.inheritedThinking === undefined
        ? {}
        : { inheritedThinking: submission.inheritedThinking }),
      ...(submission.inheritedPermissionMode === undefined
        ? {}
        : { inheritedPermissionMode: submission.inheritedPermissionMode }),
      ...(submission.modelInfo === undefined ? {} : { modelInfo: submission.modelInfo }),
    })).toEqual({
      session_id: "session-1",
      mode: "review",
      model: "provider/selected",
      permission_mode: "ask",
      model_options: { thinking_level: "low" },
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
  });

  it("chooses an explicit base, repository branch, detached HEAD, then conventional trunks", () => {
    expect(resolveNewSessionBaseRef(["feature", "master", "main"], "", "feature")).toBe("feature");
    expect(resolveNewSessionBaseRef(["feature", "master", "main"], "master", "feature")).toBe("master");
    expect(resolveNewSessionBaseRef(["feature", "master", "main"], "", "deadbeef")).toBe("HEAD");
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
