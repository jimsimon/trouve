import { describe, expect, it } from "vitest";

import { renderMarkdown } from "./markdown-view.js";
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
});
