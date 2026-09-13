import { describe, expect, it } from "vitest";

import {
  constrainDiffMode,
  diffModesForViewport,
  NARROW_DIFF_MEDIA_QUERY,
} from "./diff-mode.js";

describe("responsive diff modes", () => {
  it("keeps unified and split controls on desktop", () => {
    expect(diffModesForViewport(false)).toEqual(["unified", "split"]);
    expect(constrainDiffMode("unified", false)).toBe("unified");
    expect(constrainDiffMode("split", false)).toBe("split");
  });

  it("makes narrow viewports unified-only, including a prior split selection", () => {
    expect(NARROW_DIFF_MEDIA_QUERY).toBe("(max-width: 760px)");
    expect(diffModesForViewport(true)).toEqual(["unified"]);
    expect(constrainDiffMode("unified", true)).toBe("unified");
    expect(constrainDiffMode("split", true)).toBe("unified");
  });
});
