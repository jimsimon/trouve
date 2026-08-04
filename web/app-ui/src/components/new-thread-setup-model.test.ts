import { describe, expect, it } from "vitest";

import {
  MAX_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENTS,
  type PendingAttachment,
} from "../services/attachments.js";
import type {
  ProtocolAgentMode,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
} from "../services/protocol-client.js";
import {
  appendNewThreadAttachment,
  createInitialNewThreadDraft,
  createNewThreadSetupSubmission,
  effectiveNewThreadModel,
  newThreadAttachmentLimitMessage,
  newThreadSetupControls,
  newThreadThinkingOption,
  selectNewThreadMode,
  selectNewThreadModel,
  type NewThreadSetupCatalog,
  type NewThreadSetupDraft,
} from "./new-thread-setup-model.js";

const mode = (
  id: string,
  defaultModel?: string,
): ProtocolAgentMode => ({
  id,
  display_name: id[0]?.toUpperCase() + id.slice(1),
  system_prompt: `${id} instructions`,
  ...(defaultModel === undefined ? {} : { default_model: defaultModel }),
});

const model = (
  id: string,
  option = "thinking_level",
  values: readonly string[] = ["low", "medium", "high"],
  defaultValue = "medium",
): ProtocolModelInfo => ({
  id,
  display_name: id,
  context_window: 128_000,
  supports_tools: true,
  options_schema: {
    type: "object",
    properties: {
      [option]: {
        type: "string",
        enum: values,
        default: defaultValue,
      },
    },
  },
});

const providers: ProtocolProvidersResponse = {
  default_model: "provider/global",
  default_permission_mode: "ask",
  default_thinking_level: "medium",
  providers: [],
};

const catalog: NewThreadSetupCatalog = {
  modes: [mode("plan"), mode("code"), mode("review", "provider/review")],
  models: [
    model("provider/first"),
    model("provider/review", "effort", ["low", "high"], "high"),
    model("provider/global"),
  ],
  providers,
};

const attachment = (
  name: string,
  size: number,
): PendingAttachment => ({
  upload: { name, mime: "application/octet-stream", data: "AA==" },
  size,
});

describe("new thread setup model", () => {
  it("starts a provisional draft with Slint-compatible mode/model defaults", () => {
    const draft = createInitialNewThreadDraft(catalog);
    expect(draft).toMatchObject({
      modeId: "code",
      modelId: "provider/first",
      thinking: "medium",
      permissionMode: "",
      prompt: "",
      attachments: [],
    });
    expect(effectiveNewThreadModel(draft, catalog)?.id).toBe("provider/first");
    expect(newThreadThinkingOption(draft, catalog)).toMatchObject({
      key: "thinking_level",
      values: ["low", "medium", "high"],
    });
  });

  it("applies mode model defaults and resets thinking when the effective model changes", () => {
    const initial = createInitialNewThreadDraft(catalog);
    const reviewed = selectNewThreadMode(initial, "review", catalog);
    expect(reviewed).toMatchObject({
      modeId: "review",
      modelId: "provider/review",
      thinking: "high",
    });
    expect(newThreadThinkingOption(reviewed, catalog)?.key).toBe("effort");

    const inherited = selectNewThreadModel(reviewed, "", catalog);
    expect(inherited).toMatchObject({ modelId: "", thinking: "high" });
    expect(effectiveNewThreadModel(inherited, catalog)?.id).toBe("provider/review");

    const plan = selectNewThreadMode(inherited, "plan", catalog);
    expect(plan).toMatchObject({ modeId: "plan", modelId: "", thinking: "high" });
    expect(effectiveNewThreadModel(plan, catalog)?.id).toBe("provider/global");
  });

  it("builds only existing protocol requests and an optional initial message", () => {
    const upload = attachment("spec.txt", 4);
    const draft: NewThreadSetupDraft = {
      ...createInitialNewThreadDraft(catalog),
      modeId: "review",
      modelId: "provider/review",
      thinking: "low",
      permissionMode: "allow_list",
      prompt: "  Review this change.  ",
      attachments: [upload],
    };
    expect(createNewThreadSetupSubmission({
      workspaceId: " ws-main ",
      sessionId: " se-main ",
      draft,
      catalog,
    })).toEqual({
      workspaceId: "ws-main",
      sessionId: "se-main",
      request: {
        session_id: "se-main",
        mode: "review",
        model: "provider/review",
        permission_mode: "allow_list",
        model_options: { effort: "low" },
      },
      initialMessage: {
        content: "Review this change.",
        attachments: [upload.upload],
      },
    });

    const empty = createNewThreadSetupSubmission({
      workspaceId: "ws-main",
      sessionId: "se-main",
      draft: {
        ...draft,
        prompt: " \n ",
        attachments: [],
      },
      catalog,
    });
    expect(empty.initialMessage).toBeUndefined();

    const attachmentOnly = createNewThreadSetupSubmission({
      workspaceId: "ws-main",
      sessionId: "se-main",
      draft: { ...draft, prompt: "", attachments: [upload] },
      catalog,
    });
    expect(attachmentOnly.initialMessage).toEqual({
      content: "",
      attachments: [upload.upload],
    });
  });

  it("drops tampered mode/model/thinking selections instead of inventing request fields", () => {
    const detail = createNewThreadSetupSubmission({
      workspaceId: "ws-main",
      sessionId: "se-main",
      draft: {
        ...createInitialNewThreadDraft(catalog),
        modeId: "unknown-mode",
        modelId: "unknown-model",
        thinking: "unadvertised",
        permissionMode: "",
      },
      catalog,
    });
    expect(detail.request).toEqual({ session_id: "se-main" });
  });

  it("enforces per-item, count, and aggregate attachment budgets", () => {
    const itemTooLarge = appendNewThreadAttachment(
      [],
      attachment("large.bin", MAX_ATTACHMENT_BYTES + 1),
    );
    expect(itemTooLarge).toMatchObject({ accepted: false, limit: "item-too-large" });

    const full = Array.from(
      { length: MAX_PENDING_ATTACHMENTS },
      (_, index) => attachment(`${index}.bin`, 1),
    );
    expect(appendNewThreadAttachment(full, attachment("extra.bin", 1)))
      .toMatchObject({ accepted: false, limit: "too-many", attachments: full });

    const aggregate = appendNewThreadAttachment(
      [attachment("first.bin", MAX_ATTACHMENT_BYTES), attachment("second.bin", MAX_ATTACHMENT_BYTES)],
      attachment("third.bin", MAX_PENDING_ATTACHMENT_BYTES - (2 * MAX_ATTACHMENT_BYTES) + 1),
    );
    expect(aggregate).toMatchObject({ accepted: false, limit: "total-too-large" });
    expect(newThreadAttachmentLimitMessage("too-many")).toContain(
      String(MAX_PENDING_ATTACHMENTS),
    );
  });

  it("derives distinct ready, loading, disabled, and busy control states", () => {
    const ready = newThreadSetupControls({
      sessionId: "se-main",
      workspaceId: "ws-main",
      disabled: false,
      busy: false,
      optionsLoading: false,
      attachmentLoading: false,
    });
    expect(ready).toEqual({
      formDisabled: false,
      optionControlsDisabled: false,
      canSubmit: true,
      canCancel: true,
      submitLabel: "Start thread",
    });
    expect(newThreadSetupControls({
      sessionId: "se-main",
      workspaceId: "ws-main",
      disabled: false,
      busy: false,
      optionsLoading: true,
      attachmentLoading: false,
    })).toMatchObject({
      formDisabled: false,
      optionControlsDisabled: true,
      canSubmit: false,
      canCancel: true,
    });
    expect(newThreadSetupControls({
      sessionId: "",
      workspaceId: "ws-main",
      disabled: true,
      busy: false,
      optionsLoading: false,
      attachmentLoading: false,
    })).toMatchObject({ formDisabled: true, canSubmit: false, canCancel: true });
    expect(newThreadSetupControls({
      sessionId: "se-main",
      workspaceId: "ws-main",
      disabled: false,
      busy: true,
      optionsLoading: false,
      attachmentLoading: false,
    })).toMatchObject({
      formDisabled: true,
      canSubmit: false,
      canCancel: false,
      submitLabel: "Starting…",
    });
  });

  it("rejects invalid integration scope and out-of-bounds draft attachments", () => {
    const draft = createInitialNewThreadDraft(catalog);
    expect(() => createNewThreadSetupSubmission({
      workspaceId: " ",
      sessionId: "se-main",
      draft,
      catalog,
    })).toThrow(/workspace id/);
    expect(() => createNewThreadSetupSubmission({
      workspaceId: "ws-main",
      sessionId: "se-main",
      draft: {
        ...draft,
        attachments: [attachment("large.bin", MAX_ATTACHMENT_BYTES + 1)],
      },
      catalog,
    })).toThrow(/10 MB/);
  });
});
