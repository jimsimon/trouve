import { describe, expect, it } from "vitest";

import { utf8Length, utf8Prefix } from "./utf8-text.js";

describe("UTF-8 text bounds", () => {
  it("never returns a partial scalar at a byte boundary", () => {
    expect(utf8Prefix("a🙂b", 1)).toBe("a");
    expect(utf8Prefix("a🙂b", 4)).toBe("a");
    expect(utf8Prefix("a🙂b", 5)).toBe("a🙂");
    expect(utf8Prefix("a🙂b", 6)).toBe("a🙂b");
    expect(utf8Length("a🙂b")).toBe(6);
  });
});
