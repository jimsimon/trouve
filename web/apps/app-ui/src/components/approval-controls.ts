export type ApprovalDecision = "approve" | "always_approve" | "deny";

export interface ApprovalShortcutInput {
  readonly key: string;
  readonly altKey?: boolean;
  readonly ctrlKey?: boolean;
  readonly metaKey?: boolean;
  readonly repeat?: boolean;
  readonly isComposing?: boolean;
  readonly editable?: boolean;
}

/** Resolve keyboard shortcuts only inside the focused approval card. The
 * caller owns that focus scope; this helper rejects modified, repeated, IME,
 * and editable-target keystrokes so ordinary text entry is never captured. */
export const approvalDecisionForShortcut = (
  input: ApprovalShortcutInput,
): ApprovalDecision | undefined => {
  if (
    input.altKey === true ||
    input.ctrlKey === true ||
    input.metaKey === true ||
    input.repeat === true ||
    input.isComposing === true ||
    input.editable === true
  ) return undefined;

  switch (input.key.toLowerCase()) {
    case "y":
      return "approve";
    case "a":
      return "always_approve";
    case "n":
      return "deny";
    default:
      return undefined;
  }
};

/** Per-call single-flight guard. Independent approval cards may resolve in
 * parallel, but a repeated key/click cannot submit the same call twice. */
export class ApprovalSubmissionTracker {
  readonly #pending = new Set<string>();

  begin(callId: string): boolean {
    if (callId === "" || this.#pending.has(callId)) return false;
    this.#pending.add(callId);
    return true;
  }

  finish(callId: string): void {
    this.#pending.delete(callId);
  }

  has(callId: string): boolean {
    return this.#pending.has(callId);
  }
}
