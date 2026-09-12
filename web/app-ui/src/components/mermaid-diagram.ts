import { css, html, LitElement, type PropertyValues } from "lit";
import { unsafeHTML } from "lit/directives/unsafe-html.js";

/** Successful and failed render outcomes keyed by source. Streaming responses
 * create distinct diagram prefixes, so retain only the most recently used
 * outcomes instead of growing for the lifetime of the page. */
const MAX_CACHED_DIAGRAMS = 64;
type CachedDiagram = string | null;
const cachedDiagrams = new Map<string, CachedDiagram>();
const pendingDiagrams = new Map<string, Promise<string | undefined>>();

const readDiagram = (source: string): CachedDiagram | undefined => {
  const cached = cachedDiagrams.get(source);
  if (cached === undefined) return undefined;
  cachedDiagrams.delete(source);
  cachedDiagrams.set(source, cached);
  return cached;
};

const cacheDiagram = (source: string, svg: string | undefined): void => {
  cachedDiagrams.delete(source);
  cachedDiagrams.set(source, svg ?? null);
  if (cachedDiagrams.size > MAX_CACHED_DIAGRAMS) {
    cachedDiagrams.delete(cachedDiagrams.keys().next().value!);
  }
};

/** The diagram engine is only loaded once a diagram is actually on screen;
 * its layout engine is larger than the rest of the Markdown pipeline and
 * most responses never use it. */
const loadRenderer = async () => (await import("beautiful-mermaid")).renderMermaidSVGAsync;

export const renderMermaid = async (source: string): Promise<string | undefined> => {
  const cached = readDiagram(source);
  if (cached !== undefined) return cached ?? undefined;
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
      cacheDiagram(source, svg);
      return svg;
    } catch {
      cacheDiagram(source, undefined);
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
    .visually-hidden { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); }
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
    const cached = readDiagram(source);
    this.#svg = cached ?? undefined;
    this.#failed = cached === null;
    if (cached !== undefined) {
      this.requestUpdate();
      return;
    }
    const svg = await renderMermaid(source);
    if (generation !== this.#generation || !this.isConnected) return;
    this.#svg = svg;
    this.#failed = svg === undefined;
    this.requestUpdate();
  }

  override render() {
    if (this.#svg !== undefined) {
      return html`
        <div
          class="diagram"
          role="img"
          aria-label="Diagram"
          aria-describedby="diagram-source"
        >${unsafeHTML(this.#svg)}</div>
        <span id="diagram-source" class="visually-hidden">${this.source}</span>
      `;
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
