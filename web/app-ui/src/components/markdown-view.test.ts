import { describe, expect, it } from "vitest";

import { CONTENT_WORKER_MAX_SOURCE_UNITS } from "../workers/content-worker-protocol.js";
import { renderMarkdown, renderMarkdownSafely } from "./markdown-view.js";
import { stableMarkdownPrefixLength } from "./streaming-markdown.js";

describe("renderMarkdown", () => {
  it("renders GFM while stripping raw HTML and unsafe links", async () => {
    const rendered = await renderMarkdown(
      "**safe** | ~~done~~ | [bad](javascript:alert(1)) <img src=x onerror=alert(1)>",
    );

    expect(rendered).toContain("<strong>safe</strong>");
    expect(rendered).toContain("<del>done</del>");
    expect(rendered).not.toContain("javascript:");
    expect(rendered).not.toContain("onerror");
    expect(rendered).not.toContain("<img");
  });

  it("marks HTTPS links for isolated, host-routed external navigation", async () => {
    const rendered = await renderMarkdown("[docs](https://example.com/docs)");
    expect(rendered).not.toContain('target="_blank"');
    expect(rendered).toContain('rel="noopener noreferrer"');
  });

  it("retains canonical root-relative application links", async () => {
    const rendered = await renderMarkdown(
      "[session](/workspaces/ws_1/sessions/se_1)",
    );
    expect(rendered).toContain('href="/workspaces/ws_1/sessions/se_1"');
  });

  it("removes network-path, slash-backslash, and credential-bearing links", async () => {
    const rendered = await renderMarkdown(
      "[network](//attacker.example/path) [backslash](/\\\\attacker.example/path) [credentials](https://user:secret@example.com/path)",
    );

    expect(rendered).not.toContain("attacker.example");
    expect(rendered).not.toContain("user:secret");
    expect(rendered).not.toContain("href=");
  });

  it("shares the native stable-tail boundary for streamed rendering", () => {
    const source = "settled\n\n```ts\nconst partial = true;\n\n";
    expect(source.slice(0, stableMarkdownPrefixLength(source))).toBe("settled\n\n");
  });

  it("syntax-highlights fenced source while preserving selectable text", async () => {
    const rendered = await renderMarkdown("```rust\npub fn answer() -> u32 { 42 }\n```");
    expect(rendered).toContain('<code class="language-rust">');
    expect(rendered).toContain('<span class="tok-keyword">pub</span>');
    expect(rendered).toContain('<span class="tok-typeName">u32</span>');
    expect(rendered.replace(/<[^>]+>/gu, "")).toContain("pub fn answer() -> u32 { 42 }");
  });

  it("leaves unknown fenced languages safe and unmodified", async () => {
    const rendered = await renderMarkdown("```made-up\n<tag>& value\n```");
    expect(rendered).not.toContain("tok-");
    expect(rendered).not.toContain("<tag>");
    expect(rendered).toContain("&#x3C;tag>&#x26; value");
  });

  it("recovers unambiguous same-length fences inside Markdown examples", async () => {
    const source = [
      "before",
      "",
      "```markdown",
      "### Finding",
      "",
      "```text",
      "Agent-ready prompt...",
      "```",
      "",
      "</details>",
      "```",
      "",
      "after",
    ].join("\n");
    const rendered = await renderMarkdown(source);
    const visibleText = rendered.replace(/<[^>]+>/gu, "");

    expect(rendered.match(/<pre>/gu)).toHaveLength(1);
    expect(rendered).toContain('<code class="language-markdown">');
    expect(visibleText).toContain("```text");
    expect(visibleText).toContain("Agent-ready prompt...");
    expect(rendered).toContain("&#x3C;/details>");
    expect(rendered).toContain("<p>after</p>");
  });

  it("keeps CommonMark meaning when no later outer closer exists", async () => {
    const rendered = await renderMarkdown("```markdown\n```text\n```\nafter");

    expect(rendered.match(/<pre>/gu)).toHaveLength(1);
    expect(rendered.replace(/<[^>]+>/gu, "")).toContain("```text");
    expect(rendered).toContain("<p>after</p>");
  });
});

describe("renderMarkdownSafely", () => {
  it("resolves an oversized render to a bounded failure state", async () => {
    await expect(
      renderMarkdownSafely("x".repeat(CONTENT_WORKER_MAX_SOURCE_UNITS + 1)),
    ).resolves.toBeUndefined();
  });
});
