import { css, html, LitElement, type PropertyValues } from "lit";
import { unsafeHTML } from "lit/directives/unsafe-html.js";

import {
  cachedMarkdownOffThread,
  renderMarkdownOffThread,
} from "../services/content-worker-client.js";
import { safeMarkdownHref } from "../services/markdown-renderer.js";
import { CONTENT_WORKER_MAX_SOURCE_UNITS } from "../workers/content-worker-protocol.js";
import {
  parseChatFileTarget,
} from "./chat-file-link.js";
import { stableMarkdownPrefixLength } from "./streaming-markdown.js";

export const renderMarkdown = async (source: string): Promise<string> =>
  renderMarkdownOffThread(source);

const MARKDOWN_RENDER_FAILURE_NOTICE = "This content is too large to render safely.";

/** Resolve worker rejection into a render-state result so component updates
 * never leak an unhandled promise rejection. */
export const renderMarkdownSafely = async (source: string): Promise<string | undefined> => {
  if (source.length > CONTENT_WORKER_MAX_SOURCE_UNITS) return undefined;
  try {
    return await renderMarkdown(source);
  } catch {
    return undefined;
  }
};

export class TrouveMarkdownView extends LitElement {
  static override properties = {
    content: { type: String },
    streaming: { type: Boolean, reflect: true },
  };

  static override styles = [
    css`
      :host { min-width: 0; max-width: 100%; display: block; color: var(--trouve-text); overflow-wrap: anywhere; }
      :host > div { min-width: 0; max-width: 100%; }
      .pending-content { visibility: hidden; white-space: pre-wrap; }
      .render-notice { margin: 0; color: var(--trouve-text-dim); }
      :host([streaming])::after { content: ""; display: inline-block; width: .55em; height: 1em; margin-left: .18em; vertical-align: -.14em; background: var(--trouve-accent); animation: var(--trouve-chat-streaming-animation, pulse 1s steps(2, end) infinite); }
      :where(p, ul, ol, blockquote, pre, table) { margin: 0 0 .75em; }
      :where(p, ul, ol, blockquote, pre, table):last-child { margin-bottom: 0; }
      ul, ol { padding-inline-start: 1.7em; }
      blockquote { margin-inline: 0; padding-inline-start: .9em; border-inline-start: 3px solid var(--trouve-rule); color: var(--trouve-text-dim); }
      code { padding: .08em .28em; border-radius: var(--trouve-radius-sm); background: var(--trouve-code-bg); color: var(--trouve-code-fg); font-family: var(--trouve-font-mono); }
      pre { max-width: 100%; overflow: auto; padding: 10px 12px; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius); background: var(--trouve-code-bg); color: var(--trouve-code-fg); }
      pre code { padding: 0; background: transparent; }
      .tok-keyword, .tok-operatorKeyword { color: var(--trouve-syn-keyword, var(--trouve-accent)); }
      .tok-string, .tok-regexp { color: var(--trouve-syn-string, var(--trouve-ok)); }
      .tok-number, .tok-bool, .tok-atom { color: var(--trouve-syn-number, var(--trouve-warn)); }
      .tok-comment { color: var(--trouve-syn-comment, var(--trouve-text-dim)); }
      .tok-typeName, .tok-className, .tok-namespace { color: var(--trouve-syn-type, var(--trouve-term-cyan)); }
      .tok-variableName, .tok-propertyName, .tok-labelName { color: var(--trouve-code-fg); }
      .tok-invalid { color: var(--trouve-err); text-decoration: underline wavy; }
      a { color: var(--trouve-link); text-underline-offset: 2px; }
      img, video { max-width: 100%; height: auto; }
      table { display: block; max-width: 100%; overflow: auto; border-collapse: collapse; }
      th, td { padding: .35em .55em; border: 1px solid var(--trouve-rule); text-align: start; }
      hr { border: 0; border-top: 1px solid var(--trouve-rule); }
      @keyframes pulse { 50% { opacity: .25; } }
      @media (prefers-reduced-motion: reduce) { :host([streaming])::after { animation: none; } }
    `,
  ];

  content = "";
  streaming = false;
  #rendered = "";
  #generation = 0;
  #processedContent = "";
  #stableSourceLength = 0;
  #stableRendered = "";
  #renderFailure = false;

  protected override willUpdate(changed: PropertyValues<this>): void {
    if (changed.has("content") || changed.has("streaming")) {
      void this.#process(this.content, this.streaming);
    }
  }

  override disconnectedCallback(): void {
    this.#generation += 1;
    super.disconnectedCallback();
  }

  async #process(content: string, streaming: boolean): Promise<void> {
    const generation = ++this.#generation;
    if (content.length > CONTENT_WORKER_MAX_SOURCE_UNITS) {
      this.#showRenderFailure(generation);
      return;
    }
    const cached = streaming ? undefined : cachedMarkdownOffThread(content);
    if (cached !== undefined) {
      this.#processedContent = content;
      this.#stableSourceLength = content.length;
      this.#stableRendered = cached;
      this.#rendered = cached;
      this.#renderFailure = false;
      return;
    }
    const stableLength = streaming ? stableMarkdownPrefixLength(content) : content.length;
    const appendOnly = streaming &&
      content.startsWith(this.#processedContent) &&
      stableLength >= this.#stableSourceLength;
    const committedStableLength = appendOnly ? this.#stableSourceLength : 0;
    const committedStableRendered = appendOnly ? this.#stableRendered : "";
    const newlyStable = content.slice(committedStableLength, stableLength);
    const tail = content.slice(stableLength);
    const [newlyStableRendered, tailRendered] = await Promise.all([
      newlyStable === "" ? Promise.resolve("") : renderMarkdownSafely(newlyStable),
      tail === "" ? Promise.resolve("") : renderMarkdownSafely(tail),
    ]);
    if (newlyStableRendered === undefined || tailRendered === undefined) {
      this.#showRenderFailure(generation);
      return;
    }
    if (generation !== this.#generation || !this.isConnected) return;
    this.#processedContent = content;
    this.#stableSourceLength = stableLength;
    this.#stableRendered = committedStableRendered + newlyStableRendered;
    this.#rendered = this.#stableRendered + tailRendered;
    this.#renderFailure = false;
    this.requestUpdate();
  }

  #showRenderFailure(generation: number): void {
    if (generation !== this.#generation || !this.isConnected) return;
    this.#processedContent = "";
    this.#stableSourceLength = 0;
    this.#stableRendered = "";
    this.#rendered = "";
    this.#renderFailure = true;
    this.requestUpdate();
  }

  readonly #activateLink = (event: MouseEvent): void => {
    const anchor = event
      .composedPath()
      .find((candidate): candidate is HTMLAnchorElement => candidate instanceof HTMLAnchorElement);
    if (anchor === undefined) return;
    const href = anchor.getAttribute("href");
    if (href === null || href === undefined) return;
    const fileTarget = anchor.dataset.trouveFileTarget;
    if (fileTarget !== undefined) {
      const file = parseChatFileTarget(fileTarget);
      if (file === undefined) return;
      event.preventDefault();
      this.dispatchEvent(
        new CustomEvent("trouve-open-file", {
          detail: file,
          bubbles: true,
          composed: true,
        }),
      );
      return;
    }
    // Sanitized workspace links deliberately use `href="#"`; the typed
    // target must win over ordinary same-page fragment handling.
    if (href.startsWith("#")) return;
    const kind = safeMarkdownHref(href);
    if (kind === undefined) return;
    event.preventDefault();
    this.dispatchEvent(
      new CustomEvent(
        kind === "external" ? "trouve-open-external" : "trouve-open-internal",
        {
          detail: { href },
          bubbles: true,
          composed: true,
        },
      ),
    );
  };

  override render() {
    return html`<div @click=${this.#activateLink}>${this.#renderFailure
      ? html`<p class="render-notice" role="status">${MARKDOWN_RENDER_FAILURE_NOTICE}</p>`
      : this.#rendered === "" && this.content !== ""
      ? html`<div class="pending-content" aria-hidden="true">${this.content}</div>`
      : unsafeHTML(this.#rendered)}</div>`;
  }
}

customElements.define("trouve-markdown-view", TrouveMarkdownView);

declare global {
  interface HTMLElementEventMap {
    "trouve-open-external": CustomEvent<{ readonly href: string }>;
    "trouve-open-internal": CustomEvent<{ readonly href: string }>;
    "trouve-open-file": CustomEvent<{
      readonly path: string;
      readonly from: number;
      readonly to: number;
    }>;
  }

  interface HTMLElementTagNameMap {
    "trouve-markdown-view": TrouveMarkdownView;
  }
}
