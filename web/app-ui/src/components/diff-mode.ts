export type DiffMode = "unified" | "split";

export const NARROW_DIFF_MEDIA_QUERY = "(max-width: 760px)";

const DESKTOP_DIFF_MODES = ["unified", "split"] as const;
const NARROW_DIFF_MODES = ["unified"] as const;

/** Narrow/mobile diff review is unified-only; desktop retains both modes. */
export const diffModesForViewport = (
  narrow: boolean,
): readonly DiffMode[] => narrow ? NARROW_DIFF_MODES : DESKTOP_DIFF_MODES;

/** Coerce externally supplied or previously selected state to the viewport contract. */
export const constrainDiffMode = (
  mode: DiffMode,
  narrow: boolean,
): DiffMode => narrow ? "unified" : mode;
