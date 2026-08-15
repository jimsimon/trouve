import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("new thread setup component contract", () => {
  const component = readFileSync(
    new URL("./new-thread-setup.ts", import.meta.url),
    "utf8",
  );

  it("is a standalone context consumer with no protocol mutations", () => {
    expect(component).toContain("new ContextConsumer");
    expect(component).toContain("context: appServicesContext");
    expect(component).toContain("services.protocol.modes(workspaceId)");
    expect(component).toContain('services.modelCatalog.refresh("if-stale")');
    expect(component).toContain("services.protocol.providers()");
    expect(component).not.toContain("services.protocol.createThread(");
    expect(component).not.toContain("services.protocol.sendMessage(");
  });

  it("exposes a typed cancellable provisional lifecycle", () => {
    expect(component).toContain('NEW_THREAD_SETUP_SUBMIT_EVENT = "trouve-new-thread-submit"');
    expect(component).toContain('NEW_THREAD_SETUP_CANCEL_EVENT = "trouve-new-thread-cancel"');
    expect(component).toContain("CustomEvent<NewThreadSetupSubmitDetail>");
    expect(component).toContain("CustomEvent<NewThreadSetupCancelDetail>");
    expect(component).toContain("bubbles: true");
    expect(component).toContain("composed: true");
    expect(component).toContain("cancelable: true");
    expect(component).toContain('aria-label="New thread setup (provisional)"');
    expect(component).not.toContain(">Provisional</span>");
    expect(component).toContain(">Cancel</button>");
  });

  it("renders every setup control, the first-message gate, and bounded uploads", () => {
    expect(component).toContain('name="mode"');
    expect(component).toContain("<trouve-model-picker");
    expect(component).toContain(".value=${this.#draft.modelId}");
    expect(component).toContain("@trouve-model-picked=${this.#modelPicked}");
    expect(component).toContain('name="thinking"');
    expect(component).toContain('name="permission_mode"');
    expect(component).not.toContain("Default mode");
    expect(component).not.toContain("Mode or server default");
    expect(component).not.toContain("Model default");
    expect(component).toContain("<span>First message</span>");
    expect(component).not.toContain('name="prompt"\n            required');
    expect(component).toContain('this.#draft.prompt.trim() === "" && this.#draft.attachments.length === 0');
    expect(component).toContain("encodeAttachment(");
    expect(component).toContain("appendNewThreadAttachment(");
    expect(component).toContain("pendingAttachmentPreviewUrl(attachment)");
    expect(component).toContain('aria-label="Initial message attachments"');
    expect(component).toContain('class="attachment-icon"');
    expect(component).toContain("attachment.upload.mime");
    expect(component).toContain("Unattended execution (YOLO) is dangerous");
  });

  it("uses bounded native attachments while retaining browser fallbacks", () => {
    expect(component).toContain("context: hostCapabilitiesContext");
    expect(component).toContain('type="file"');
    expect(component).toContain("nativeHost.pickFiles()");
    expect(component).toContain("nativeHost.readClipboardImage()");
    expect(component).toContain('types.includes("text/plain")');
  });

  it("matches the chat composer's autogrow and Enter-to-submit behavior", () => {
    expect(component).toContain("composerTextareaLayout(");
    expect(component).toContain("isComposerCompositionKey({");
    expect(component).toContain("event.key !== \"Enter\" || event.shiftKey");
    expect(component).toContain(".form?.requestSubmit()");
    expect(component).toContain("@compositionstart=${this.#promptCompositionStarted}");
    expect(component).toContain(
      'this.#optionsTouched = false;\n      this.#promptComposing = false;',
    );
    expect(component).toContain(
      'this.#attachmentGeneration += 1;\n    this.#promptComposing = false;\n    this.#loadedWorkspaceId = "";',
    );
  });

  it("keeps static controls immediate while surfacing busy, warning, and error states", () => {
    expect(component).toContain("newThreadSetupControls({");
    expect(component).toContain('aria-busy=${this.busy || this.#attachmentLoading}');
    expect(component).not.toContain("Loading modes and models…");
    expect(component).toContain('role="status"');
    expect(component).toContain('role="alert"');
    expect(component).toContain("disabledMessage");
    expect(component).toContain("errorMessage");
    expect(component).toContain("var(--trouve-win-bg)");
    expect(component).toContain("var(--trouve-accent)");
    expect(component).toContain("var(--trouve-err)");
  });
});
