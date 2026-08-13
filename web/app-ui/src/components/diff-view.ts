import { css, html, LitElement, type PropertyValues } from "lit";

import { loadCodeMirrorExtensions } from "./code-view.js";
import {
  diffLineRangeDescription,
  type DiffLineNumber,
} from "./diff-line-numbers.js";
import {
  constrainDiffMode,
  diffModesForViewport,
  NARROW_DIFF_MEDIA_QUERY,
  type DiffMode,
} from "./diff-mode.js";

export type { DiffMode } from "./diff-mode.js";

export const LARGE_DIFF_VIEW_THRESHOLD = 3_000_000;

export class TrouveDiffView extends LitElement {
  static override properties = {
    original: { type: String },
    modified: { type: String },
    language: { type: String },
    label: { type: String },
    mode: { type: String, reflect: true },
    originalLineNumbers: { attribute: false },
    modifiedLineNumbers: { attribute: false },
  };

  static override styles = [
    css`
      :host { display: grid; grid-template-rows: auto minmax(0, 1fr); min-width: 0; min-height: 0; height: 100%; border: 1px solid var(--trouve-border); background: var(--trouve-code-bg); }
      .toolbar { display: flex; align-items: center; gap: 5px; min-height: 31px; padding: 4px 7px; border-bottom: 1px solid var(--trouve-border); background: var(--trouve-panel-bg); }
      .toolbar strong { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--trouve-text-hi); font-size: 11px; }
      .toolbar span { flex: 1; }
      button { border: 1px solid transparent; border-radius: var(--trouve-radius-sm); padding: 3px 7px; color: var(--trouve-text-dim); background: transparent; font: inherit; }
      button:hover { background: var(--trouve-hover-bg); }
      button[aria-pressed="true"] { border-color: var(--trouve-border-strong); color: var(--trouve-text-hi); background: var(--trouve-control-bg); }
      button:focus-visible { outline: 2px solid var(--trouve-accent); outline-offset: 1px; }
      #diff { min-height: 10rem; overflow: hidden; }
      .cm-editor { height: 100%; }
      .cm-scroller, .cm-mergeView { overflow: auto; }
      .cm-mergeView { height: 100%; }
      .cm-mergeViewEditors { height: 100%; }
      .cm-merge-a .cm-changedLine,
      .cm-deletedChunk,
      .cm-deletedLine { background: var(--trouve-diff-del-bg) !important; }
      .cm-merge-b .cm-changedLine,
      .cm-inlineChangedLine { background: var(--trouve-diff-add-bg) !important; }
      .cm-merge-a .cm-changedText,
      .cm-deletedText { background: var(--trouve-diff-del-text-bg) !important; background-image: none !important; }
      .cm-merge-b .cm-changedText,
      .cm-inlineChangedLine .cm-changedText { background: var(--trouve-diff-add-text-bg) !important; background-image: none !important; }
      .cm-changedText { background-image: none !important; text-decoration: none !important; }
      .cm-deletedText,
      .cm-deletedChunk del,
      .cm-deletedLine del { text-decoration: none !important; }
      .fallback { display: grid; grid-template-columns: 1fr 1fr; min-height: 0; overflow: hidden; }
      .fallback section { display: grid; grid-template-rows: auto minmax(0, 1fr); min-width: 0; min-height: 0; overflow: hidden; }
      .fallback section + section { border-inline-start: 1px solid var(--trouve-rule); }
      .fallback h3 { position: sticky; top: 0; margin: 0; padding: 5px 9px; background: var(--trouve-panel-bg); color: var(--trouve-text-dim); font-size: 11px; }
      .fallback .large-code { min-width: 0; min-height: 0; overflow: hidden; }
      .fallback .cm-editor { height: 100%; }
      .visually-hidden { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); clip-path: inset(50%); white-space: nowrap; }
      @media (max-width: 760px) { .fallback { grid-template-columns: 1fr; } .fallback section + section { border-inline-start: 0; border-top: 1px solid var(--trouve-rule); } }
    `,
  ];

  original = "";
  modified = "";
  language = "text";
  label = "Code changes";
  mode: DiffMode = "unified";
  originalLineNumbers: readonly DiffLineNumber[] = [];
  modifiedLineNumbers: readonly DiffLineNumber[] = [];

  #merge: import("@codemirror/merge").MergeView | undefined;
  #originalView: import("@codemirror/view").EditorView | undefined;
  #view: import("@codemirror/view").EditorView | undefined;
  #generation = 0;
  #narrow = false;
  #viewportQuery: MediaQueryList | undefined;
  #viewportListening = false;
  #correctedMode: DiffMode | undefined;

  readonly #viewportChanged = (event: MediaQueryListEvent): void => {
    this.#applyViewport(event.matches, true);
  };

  override connectedCallback(): void {
    super.connectedCallback();
    this.#viewportQuery ??= globalThis.matchMedia?.(NARROW_DIFF_MEDIA_QUERY);
    const query = this.#viewportQuery;
    if (query === undefined) return;
    this.#applyViewport(query.matches, false);
    if (this.#viewportListening) return;
    if (typeof query.addEventListener === "function") {
      query.addEventListener("change", this.#viewportChanged);
    } else {
      query.addListener(this.#viewportChanged);
    }
    this.#viewportListening = true;
  }

  protected override firstUpdated(): void {
    void this.#mount();
  }

  protected override willUpdate(): void {
    const effectiveMode = constrainDiffMode(this.mode, this.#narrow);
    this.#correctedMode = effectiveMode === this.mode ? undefined : effectiveMode;
    if (this.#correctedMode !== undefined) this.mode = this.#correctedMode;
  }

  protected override updated(changed: PropertyValues<this>): void {
    const correctedMode = this.#correctedMode;
    this.#correctedMode = undefined;
    if (correctedMode !== undefined) this.#dispatchModeChange(correctedMode);
    if (
      changed.has("original") ||
      changed.has("modified") ||
      changed.has("language") ||
      changed.has("label") ||
      changed.has("mode") ||
      changed.has("originalLineNumbers") ||
      changed.has("modifiedLineNumbers") ||
      correctedMode !== undefined
    ) {
      void this.#mount();
    }
  }

  override disconnectedCallback(): void {
    this.#generation += 1;
    const query = this.#viewportQuery;
    if (query !== undefined && this.#viewportListening) {
      if (typeof query.removeEventListener === "function") {
        query.removeEventListener("change", this.#viewportChanged);
      } else {
        query.removeListener(this.#viewportChanged);
      }
      this.#viewportListening = false;
    }
    this.#dispose();
    super.disconnectedCallback();
  }

  #selectMode(mode: DiffMode): void {
    const effectiveMode = constrainDiffMode(mode, this.#narrow);
    if (effectiveMode === this.mode) return;
    this.mode = effectiveMode;
    this.#dispatchModeChange(effectiveMode);
  }

  #dispatchModeChange(mode: DiffMode): void {
    this.dispatchEvent(
      new CustomEvent<{ mode: DiffMode }>("trouve-diff-mode-change", {
        detail: { mode },
        bubbles: true,
        composed: true,
      }),
    );
  }

  #applyViewport(narrow: boolean, announceChange: boolean): void {
    const viewportChanged = narrow !== this.#narrow;
    this.#narrow = narrow;
    const effectiveMode = constrainDiffMode(this.mode, narrow);
    if (effectiveMode !== this.mode) {
      this.mode = effectiveMode;
      if (announceChange) this.#dispatchModeChange(effectiveMode);
    } else if (viewportChanged) {
      this.requestUpdate();
    }
  }

  #dispose(): void {
    this.#merge?.destroy();
    this.#originalView?.destroy();
    this.#view?.destroy();
    this.#merge = undefined;
    this.#originalView = undefined;
    this.#view = undefined;
  }

  async #mount(): Promise<void> {
    if (!this.hasUpdated || !this.isConnected) return;
    const generation = ++this.#generation;
    this.#dispose();
    const tooLarge = this.original.length + this.modified.length > LARGE_DIFF_VIEW_THRESHOLD;
    if (tooLarge) {
      const originalParent = this.renderRoot.querySelector<HTMLElement>("#large-original");
      const modifiedParent = this.renderRoot.querySelector<HTMLElement>("#large-modified");
      if (originalParent === null || modifiedParent === null) return;
      const [{ EditorState }, { EditorView }, originalExtensions, modifiedExtensions] =
        await Promise.all([
          import("@codemirror/state"),
          import("@codemirror/view"),
          loadCodeMirrorExtensions({
            language: this.language,
            lineWrapping: false,
            label: `${this.label}, original large document`,
            lineNumbers: this.originalLineNumbers,
            parseLanguage: false,
          }),
          loadCodeMirrorExtensions({
            language: this.language,
            lineWrapping: false,
            label: `${this.label}, modified large document`,
            lineNumbers: this.modifiedLineNumbers,
            parseLanguage: false,
          }),
        ]);
      if (generation !== this.#generation || !this.isConnected) return;
      const root = this.renderRoot instanceof ShadowRoot ? this.renderRoot : undefined;
      this.#originalView = new EditorView({
        state: EditorState.create({ doc: this.original, extensions: originalExtensions }),
        parent: originalParent,
        ...(root === undefined ? {} : { root }),
      });
      this.#view = new EditorView({
        state: EditorState.create({ doc: this.modified, extensions: modifiedExtensions }),
        parent: modifiedParent,
        ...(root === undefined ? {} : { root }),
      });
      return;
    }
    const parent = this.renderRoot.querySelector<HTMLElement>("#diff");
    if (parent === null) return;
    const [{ EditorState }, { EditorView }, merge] = await Promise.all([
      import("@codemirror/state"),
      import("@codemirror/view"),
      import("@codemirror/merge"),
    ]);
    const [originalExtensions, modifiedExtensions] = await Promise.all([
      loadCodeMirrorExtensions({
        language: this.language,
        lineWrapping: false,
        label: `${this.label}, original`,
        lineNumbers: this.originalLineNumbers,
      }),
      loadCodeMirrorExtensions({
        language: this.language,
        lineWrapping: false,
        label: `${this.label}, modified`,
        lineNumbers: this.modifiedLineNumbers,
      }),
    ]);
    if (generation !== this.#generation || !this.isConnected) return;
    parent.replaceChildren();
    const root = this.renderRoot instanceof ShadowRoot ? this.renderRoot : undefined;
    if (constrainDiffMode(this.mode, this.#narrow) === "split") {
      this.#merge = new merge.MergeView({
        a: { doc: this.original, extensions: originalExtensions },
        b: { doc: this.modified, extensions: modifiedExtensions },
        parent,
        ...(root === undefined ? {} : { root }),
        highlightChanges: true,
        gutter: true,
        collapseUnchanged: { margin: 3, minSize: 8 },
        diffConfig: { scanLimit: 5_000, timeout: 1_000 },
      });
      return;
    }
    this.#view = new EditorView({
      state: EditorState.create({
        doc: this.modified,
        extensions: [
          ...modifiedExtensions,
          merge.unifiedMergeView({
            original: this.original,
            mergeControls: false,
            allowInlineDiffs: false,
            collapseUnchanged: { margin: 3, minSize: 8 },
            diffConfig: { scanLimit: 5_000, timeout: 1_000 },
          }),
        ],
      }),
      parent,
      ...(root === undefined ? {} : { root }),
    });
  }

  override render() {
    const tooLarge = this.original.length + this.modified.length > LARGE_DIFF_VIEW_THRESHOLD;
    const mode = constrainDiffMode(this.mode, this.#narrow);
    const lineRangeDescription = diffLineRangeDescription(
      this.originalLineNumbers,
      this.modifiedLineNumbers,
    );
    return html`
      <header class="toolbar">
        <strong>${this.label}</strong><span></span>
        ${diffModesForViewport(this.#narrow).map((candidate) => html`
          <button
            type="button"
            aria-pressed=${mode === candidate}
            @click=${() => this.#selectMode(candidate)}
          >${candidate === "unified" ? "Unified" : "Split"}</button>
        `)}
      </header>
      <p id="diff-line-ranges" class="visually-hidden">${lineRangeDescription}</p>
      ${tooLarge
        ? html`<div class="fallback" role="region" aria-label=${this.label} aria-describedby="diff-line-ranges">
            <p class="visually-hidden">This large change is shown as virtualized before and after documents; inline change computation is disabled.</p>
            <section><h3>Before</h3><div id="large-original" class="large-code"></div></section>
            <section><h3>After</h3><div id="large-modified" class="large-code"></div></section>
          </div>`
        : html`<div id="diff" role="region" aria-label=${this.label} aria-describedby="diff-line-ranges"></div>`}
    `;
  }
}

customElements.define("trouve-diff-view", TrouveDiffView);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-diff-view": TrouveDiffView;
  }
}
