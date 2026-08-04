import { describe, expect, it } from "vitest";

import type {
  ProtocolAgentMode,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
} from "../services/protocol-client.js";
import {
  createNewSessionThreadRequest,
  NEW_SESSION_TITLE_FALLBACK,
  NEW_SESSION_TITLE_MAX_LENGTH,
  resolveNewSessionModel,
  sessionTitleFallback,
  thinkingOption,
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

const mode = (defaultModel?: string | null): ProtocolAgentMode => ({
  id: "code",
  display_name: "Code",
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

  it("supports Codex-style reasoning option names in Slint precedence order", () => {
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

  it("composes a complete request with an advertised thinking override", () => {
    const modelInfo = model({
      properties: {
        effort: { type: "string", enum: ["low", "high"], default: "low" },
      },
    });
    expect(createNewSessionThreadRequest({
      sessionId: " session-1 ",
      mode: " plan ",
      model: "provider/model",
      permissionMode: "allow_list",
      thinking: " high ",
      modelInfo,
    })).toEqual({
      session_id: "session-1",
      mode: "plan",
      model: "provider/model",
      permission_mode: "allow_list",
      model_options: { effort: "high" },
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
