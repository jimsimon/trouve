export type NativeTextHistoryCommand = "undo" | "redo";

export interface NativeTextHistoryShortcut {
  readonly key: string;
  readonly altKey?: boolean;
  readonly ctrlKey?: boolean;
  readonly metaKey?: boolean;
  readonly shiftKey?: boolean;
  readonly isComposing?: boolean;
}

const TEXT_INPUT_TYPES = new Set([
  "email",
  "number",
  "password",
  "search",
  "tel",
  "text",
  "url",
]);

/** Resolve the platform-standard text history shortcuts without assuming a
 * particular desktop host. Ctrl/Cmd+Shift+Z and Ctrl/Cmd+Y are both accepted
 * because the product runs on Linux, macOS, and Windows. */
export const nativeTextHistoryCommand = (
  shortcut: NativeTextHistoryShortcut,
): NativeTextHistoryCommand | undefined => {
  if (
    shortcut.altKey === true
    || shortcut.isComposing === true
    || (shortcut.ctrlKey !== true && shortcut.metaKey !== true)
  ) return undefined;

  const key = shortcut.key.toLowerCase();
  if (key === "z") return shortcut.shiftKey === true ? "redo" : "undo";
  if (key === "y" && shortcut.shiftKey !== true) return "redo";
  return undefined;
};

const nativeTextControl = (path: readonly EventTarget[]): boolean => {
  // Terminal emulators use a hidden textarea to receive keystrokes. Its
  // Ctrl+Z is terminal input, not browser text history.
  if (
    path.some((target) =>
      target instanceof HTMLElement && target.getAttribute("role") === "application"
    )
  ) return false;

  return path.some((target) => {
    if (target instanceof HTMLTextAreaElement) {
      return !target.disabled && !target.readOnly;
    }
    if (!(target instanceof HTMLInputElement)) return false;
    return !target.disabled && !target.readOnly && TEXT_INPUT_TYPES.has(target.type);
  });
};

const installedDocuments = new WeakSet<Document>();

/**
 * Wry's Linux host uses WebKitGTK's GTK3 ABI. That port translates GTK text
 * bindings through a hidden GtkTextView and explicitly maps SelectAll, Cut,
 * Copy, and Paste, but GTK3 supplies no Undo/Redo binding for the translator
 * to forward. The WebKit editing commands and their native history still
 * work, so route the missing accelerators back through those commands rather
 * than maintaining a competing application-level undo stack.
 *
 * Upstream boundary: Source/WebKit/UIProcess/gtk/KeyBindingTranslator.cpp.
 */
export const installNativeTextHistoryShortcuts = (
  documentLike: Document = globalThis.document,
): void => {
  if (installedDocuments.has(documentLike)) return;
  installedDocuments.add(documentLike);
  documentLike.addEventListener("keydown", (event) => {
    if (event.defaultPrevented || !nativeTextControl(event.composedPath())) return;
    const command = nativeTextHistoryCommand(event);
    if (command === undefined) return;
    try {
      if (documentLike.execCommand(command)) event.preventDefault();
    } catch {
      // Let the engine's ordinary key binding run when the compatibility
      // editing command is unavailable.
    }
  }, true);
};
