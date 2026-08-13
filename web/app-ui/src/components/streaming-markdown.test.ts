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

  it("keeps mismatched markers open and accepts longer matching closers", () => {
    const mismatched = "intro\n\n````text\ninside\n\n~~~~\n\nstill code";
    expect(mismatched.slice(0, stableMarkdownPrefixLength(mismatched))).toBe(
      "intro\n\n",
    );
    const closed = "intro\n\n~~~text\ninside\n~~~~~\n\ntail";
    expect(closed.slice(0, stableMarkdownPrefixLength(closed))).toBe(
      "intro\n\n~~~text\ninside\n~~~~~\n\n",
    );
  });

  it("does not freeze an ambiguous nested Markdown fence before its outer closer", () => {
    const partial = [
      "before",
      "",
      "```markdown",
      "```text",
      "prompt",
      "```",
      "",
      "still potentially inside the example",
      "",
    ].join("\n");
    expect(partial.slice(0, stableMarkdownPrefixLength(partial))).toBe("before\n\n");

    const outerCloser = "```\n\n";
    const complete = partial + outerCloser + "after";
    expect(complete.slice(0, stableMarkdownPrefixLength(complete))).toBe(
      partial + outerCloser,
    );
  });
});
