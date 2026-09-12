import { css, html, LitElement, type PropertyValues } from "lit";
import { unsafeHTML } from "lit/directives/unsafe-html.js";

/** Rendered SVG keyed by source. Streaming responses re-render the enclosing
 * Markdown on every delta, so a finished diagram must not be laid out again
 * each time its surrounding text grows. Colors are CSS variables, so a theme
 * switch restyles cached output without re-rendering. */
const renderedDiagrams = new Map<string, string>();
const pendingDiagrams = new Map<string, Promise<string | undefined>>();

/** The diagram engine is only loaded once a diagram is actually on screen;
 * its layout engine is larger than the rest of the Markdown pipeline and
 * most responses never use it. */
const loadRenderer = async () => (await import("beautiful-mermaid")).renderMermaidSVGAsync;

export const renderMermaid = async (source: string): Promise<string | undefined> => {
  const cached = renderedDiagrams.get(source);
  if (cached !== undefined) return cached;
  const pending = pendingDiagrams.get(source);
  if (pending !== undefined) return pending;
  const request = (async () => {
    try {
      const render = await loadRenderer();
      const svg = await render(source, {
        transparent: true,
        bg: "var(--trouve-code-bg)",
        fg: "var(--trouve-text)",
        line: "var(--trouve-text-dim)",
        accent: "var(--trouve-accent)",
        muted: "var(--trouve-text-dim)",
        surface: "var(--trouve-raised-bg)",
        border: "var(--trouve-border-strong)",
        font: "var(--trouve-font-ui, system-ui, sans-serif)",
        padding: 12,
      });
      renderedDiagrams.set(source, svg);
      return svg;
    } catch {
      return undefined;
    } finally {
      pendingDiagrams.delete(source);
    }
  })();
  pendingDiagrams.set(source, request);
  return request;
};

/** A fenced `mermaid` block rendered as an inline diagram. Invalid or
 * unsupported diagram text falls back to the plain code block so nothing the
 * model wrote is lost. */
export class TrouveMermaidDiagram extends LitElement {
  static override properties = {
    source: { type: String },
  };

  static override styles = css`
    :host { display: block; max-width: 100%; margin: 0 0 .75em; }
    :host(:last-child) { margin-bottom: 0; }
    .diagram { max-width: 100%; overflow: auto; padding: 10px 12px; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius); background: var(--trouve-code-bg); }
    .diagram svg { display: block; max-width: 100%; height: auto; margin: 0 auto; }
    pre { max-width: 100%; overflow: auto; margin: 0; padding: 10px 12px; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius); background: var(--trouve-code-bg); color: var(--trouve-code-fg); font-family: var(--trouve-font-mono); }
    code { font-family: inherit; }
  `;

  source = "";
  #svg: string | undefined;
  #failed = false;
  #generation = 0;

  protected override willUpdate(changed: PropertyValues<this>): void {
    if (changed.has("source")) void this.#render(this.source);
  }

  override connectedCallback(): void {
    super.connectedCallback();
    // A pending render is deliberately ignored after disconnect. Reattach to
    // that shared promise (or its cache entry) when the same element returns;
    // Lit will not call willUpdate again when `source` itself did not change.
    if (this.hasUpdated && this.#svg === undefined && !this.#failed) {
      void this.#render(this.source);
    }
  }

  override disconnectedCallback(): void {
    this.#generation += 1;
    super.disconnectedCallback();
  }

  async #render(source: string): Promise<void> {
    const generation = ++this.#generation;
    this.#svg = renderedDiagrams.get(source);
    this.#failed = false;
    if (this.#svg !== undefined) return;
    const svg = await renderMermaid(source);
    if (generation !== this.#generation || !this.isConnected) return;
    this.#svg = svg;
    this.#failed = svg === undefined;
    this.requestUpdate();
  }

  override render() {
    if (this.#svg !== undefined) {
      return html`<div class="diagram" role="img" aria-label="Diagram">${unsafeHTML(this.#svg)}</div>`;
    }
    return html`<pre aria-busy=${!this.#failed}><code>${this.source}</code></pre>`;
  }
}

customElements.define("trouve-mermaid-diagram", TrouveMermaidDiagram);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-mermaid-diagram": TrouveMermaidDiagram;
  }
}
