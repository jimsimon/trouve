import { describe, expect, it } from "vitest";

import { stableMarkdownPrefixLength } from "./streaming-markdown.js";

describe("streaming Markdown stable prefix", () => {
  it("stops at the last complete blank line", () => {
    const source = "first paragraph\n\nsecond paragraph";
    expect(source.slice(0, stableMarkdownPrefixLength(source))).toBe(
      "first paragraph\n\n",
    );
  });

  it("does not split inside open or closed fenced code", () => {
    const open = "before\n\n```rust\nfn main() {}\n\nstill code";
    expect(open.slice(0, stableMarkdownPrefixLength(open))).toBe("before\n\n");
    const closed = [open, "```", "", "after"].join("\n");
    expect(closed.slice(0, stableMarkdownPrefixLength(closed))).toBe(
      [open, "```", "", ""].join("\n"),
    );
  });

  it("recognizes tilde fences and ignores blank lines until they close", () => {
    const source = "intro\n\n~~~text\ninside\n\n~~~\n\ntail";
    expect(source.slice(0, stableMarkdownPrefixLength(source))).toBe(
      "intro\n\n~~~text\ninside\n\n~~~\n\n",
    );
  });
});
