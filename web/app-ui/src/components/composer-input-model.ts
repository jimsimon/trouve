export const COMPOSER_MIN_HEIGHT = 34;
export const COMPOSER_MAX_HEIGHT = 162;

export interface ComposerTextareaLayout {
  readonly height: number;
  readonly overflowY: "auto" | "hidden";
}

/** Keep the Lit textarea's autogrow limits identical to the desktop
 * composer's 34–162 px contract. */
export const composerTextareaLayout = (
  scrollHeight: number,
  hasContent = true,
): ComposerTextareaLayout => {
  // Chromium includes a wrapped placeholder in scrollHeight. An empty
  // narrow composer must stay at its minimum height so entering a short
  // message cannot shrink the composer and visibly shift a pinned transcript.
  const contentHeight = !hasContent
    ? COMPOSER_MIN_HEIGHT
    : Number.isFinite(scrollHeight)
    ? Math.max(0, scrollHeight)
    : COMPOSER_MIN_HEIGHT;
  return {
    height: Math.min(COMPOSER_MAX_HEIGHT, Math.max(COMPOSER_MIN_HEIGHT, contentHeight)),
    overflowY: contentHeight > COMPOSER_MAX_HEIGHT ? "auto" : "hidden",
  };
};

export interface ComposerKeyState {
  readonly key: string;
  readonly keyCode?: number;
  readonly isComposing?: boolean;
  readonly compositionActive?: boolean;
}

/** Browsers do not agree on whether the key event that commits an IME still
 * has isComposing set. Process and legacy keyCode 229 cover that commit edge. */
export const isComposerCompositionKey = (state: ComposerKeyState): boolean =>
  state.compositionActive === true
  || state.isComposing === true
  || state.key === "Process"
  || state.keyCode === 229;
