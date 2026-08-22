import { describe, expect, it } from "vitest";
import type { ProtocolModelInfo } from "../services/protocol-client.js";

import {
  automationDraftFrom,
  automationDraftFromTemplate,
  automationEnabledRequest,
  automationRequestFromDraft,
  automationScheduleSummary,
  emptyAutomationDraft,
  validateAutomationDraft,
} from "./automations-model.js";

describe("automation form model", () => {
  const model: ProtocolModelInfo = {
    id: "provider/model",
    display_name: "Model",
    context_window: 128_000,
    supports_tools: true,
    options_schema: {
      type: "object",
      properties: {
        reasoning_effort: {
          type: "string",
          enum: ["low", "high"],
        },
      },
    },
  };
  it("normalizes optional schedule and permission fields from older responses", () => {
    const draft = automationDraftFrom({
      id: "auto_1",
      name: "Nightly checks",
      prompt: "Run the checks",
      workspace_id: "ws_1",
      schedule: { kind: "weekly", days: [4, 1, 4, 99], time: "bad" },
      enabled: true,
      created_at: "2026-08-02T12:00:00Z",
    });

    expect(draft.permissionMode).toBe("ask");
    expect(draft.time).toBe("09:00");
    expect(draft.days).toEqual([1, 4]);
  });

  it("validates required fields and each canonical schedule kind", () => {
    expect(validateAutomationDraft(emptyAutomationDraft())).toEqual({
      name: "Enter an automation name.",
      prompt: "Enter the prompt to run.",
      workspaceId: "Choose a workspace.",
    });

    const base = {
      ...emptyAutomationDraft("ws_1"),
      name: "Checks",
      prompt: "Run checks",
    };
    expect(validateAutomationDraft({ ...base, scheduleKind: "hourly", minute: "60" }))
      .toMatchObject({ schedule: expect.stringContaining("0 through 59") });
    for (const minute of ["", "  ", "1e1", "0x10", "1.5"]) {
      expect(validateAutomationDraft({ ...base, scheduleKind: "hourly", minute }))
        .toMatchObject({ schedule: expect.stringContaining("whole number") });
    }
    expect(validateAutomationDraft({ ...base, scheduleKind: "daily", time: "24:00" }))
      .toMatchObject({ schedule: expect.stringContaining("24-hour time") });
    expect(validateAutomationDraft({ ...base, scheduleKind: "weekly", days: [] }))
      .toMatchObject({ schedule: expect.stringContaining("at least one day") });
  });

  it("builds a trimmed weekly request with canonical day ordering", () => {
    const request = automationRequestFromDraft({
      ...emptyAutomationDraft("ws_1"),
      name: "  Weekly review  ",
      prompt: "  Review recent changes  ",
      mode: "review",
      model: "openai/gpt-5.6",
      modelOptions: { reasoning_effort: "max", fast: true },
      permissionMode: "allow_list",
      scheduleKind: "weekly",
      time: "07:15",
      days: [6, 0, 6, 3],
    }, {
      ...model,
      id: "openai/gpt-5.6",
      options_schema: {
        type: "object",
        properties: {
          reasoning_effort: { enum: ["low", "max"] },
          fast: { type: "boolean" },
        },
      },
    });

    expect(request).toMatchObject({
      name: "Weekly review",
      prompt: "Review recent changes",
      workspace_id: "ws_1",
      mode: "review",
      model: "openai/gpt-5.6",
      thinking_level: null,
      model_options: { reasoning_effort: "max", fast: true },
      permission_mode: "allow_list",
      schedule: { kind: "weekly", minute: 0, time: "07:15", days: [0, 3, 6] },
      enabled: true,
    });
  });

  it("drops stale automation options and translates the legacy thinking key", () => {
    const draft = {
      ...emptyAutomationDraft("ws_1"),
      name: "Check",
      prompt: "Run it",
    };
    expect(automationRequestFromDraft({
      ...draft,
      modelOptions: { effort: "missing", removed: true },
    }, model).model_options).toEqual({});
    expect(automationRequestFromDraft({
      ...draft,
      modelOptions: { thinking_level: "high" },
    }, model).model_options).toEqual({ reasoning_effort: "high" });
  });

  it("never perpetuates hidden options when catalog metadata is unavailable", () => {
    const request = automationRequestFromDraft({
      ...emptyAutomationDraft("ws_1"),
      name: "Check",
      prompt: "Run it",
      modelOptions: { removed: true, effort: "high" },
    }, undefined);
    expect(request.model_options).toEqual({});
  });

  it("preserves stored model fields for lifecycle-only enabled changes", () => {
    const automation = {
      id: "auto_retired",
      name: " Retired model ",
      prompt: " Run it ",
      workspace_id: "ws_1",
      mode: "code",
      model: "provider/retired",
      thinking_level: "high",
      model_options: { removed_option: true },
      permission_mode: "ask" as const,
      schedule: { kind: "weekly", time: "09:00", days: [4, 1, 4] },
      enabled: true,
      created_at: "2026-08-02T12:00:00Z",
    };
    expect(automationEnabledRequest(automation, false)).toMatchObject({
      name: " Retired model ",
      prompt: " Run it ",
      model: "provider/retired",
      thinking_level: "high",
      model_options: { removed_option: true },
      schedule: { kind: "weekly", time: "09:00", days: [4, 1, 4] },
      enabled: false,
    });
  });

  it("does not hydrate legacy thinking beside an explicit token budget", () => {
    const draft = automationDraftFrom({
      id: "auto_budget",
      name: "Budget",
      prompt: "Run it",
      workspace_id: "ws_1",
      thinking_level: "16384",
      model_options: { thinking_budget_tokens: 8192 },
      permission_mode: "ask",
      schedule: { kind: "daily", time: "09:00" },
      enabled: true,
      created_at: "2026-08-02T12:00:00Z",
    });
    expect(draft.modelOptions).toEqual({ thinking_budget_tokens: 8192 });
  });

  it("applies templates without choosing unsafe permissions for the user", () => {
    const draft = automationDraftFromTemplate(
      {
        id: "template_1",
        name: "Coverage gaps",
        description: "Find missing tests",
        prompt: "Inspect coverage",
        schedule: { kind: "hourly", minute: 5 },
      },
      "ws_2",
    );

    expect(draft).toMatchObject({
      name: "Coverage gaps",
      workspaceId: "ws_2",
      permissionMode: "ask",
      scheduleKind: "hourly",
      minute: "5",
    });
  });

  it("describes schedules with the server's local-time semantics", () => {
    expect(automationScheduleSummary({ kind: "hourly", minute: 7 })).toBe(
      "Hourly at :07",
    );
    expect(automationScheduleSummary({ kind: "daily", time: "09:30" })).toBe(
      "Daily at 09:30",
    );
    expect(
      automationScheduleSummary({ kind: "weekly", time: "17:00", days: [0, 2, 4] }),
    ).toBe("Mon, Wed, Fri at 17:00");
  });
});
