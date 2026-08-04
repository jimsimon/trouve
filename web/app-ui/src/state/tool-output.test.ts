import { describe, expect, it } from "vitest";

import {
  appendBoundedToolOutput,
  emptyToolOutput,
} from "./tool-output.js";

describe("bounded tool output", () => {
  it("appends chunks in order without marking complete output omitted", () => {
    const first = appendBoundedToolOutput(emptyToolOutput(), "one ", 64);
    const second = appendBoundedToolOutput(first, "two", 64);

    expect(second).toEqual({ text: "one two", bytes: 7, omitted: false });
  });

  it("retains a bounded tail and keeps the omission flag sticky", () => {
    const first = appendBoundedToolOutput(emptyToolOutput(), "0123456789", 8);
    expect(first).toEqual({ text: "23456789", bytes: 8, omitted: true });

    const second = appendBoundedToolOutput(first, "ab", 8);
    expect(second).toEqual({ text: "456789ab", bytes: 8, omitted: true });
  });

  it("bounds UTF-8 bytes without retaining a partial scalar value", () => {
    const output = appendBoundedToolOutput(emptyToolOutput(), "a🙂b🙂c", 7);

    expect(output).toEqual({ text: "b🙂c", bytes: 6, omitted: true });
    expect(new TextEncoder().encode(output.text)).toHaveLength(output.bytes);
  });

  it("treats empty chunks as an identity operation", () => {
    const current = appendBoundedToolOutput(emptyToolOutput(), "output", 16);
    expect(appendBoundedToolOutput(current, "", 16)).toBe(current);
  });
});
