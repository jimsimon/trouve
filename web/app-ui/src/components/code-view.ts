import type { Extension } from "@codemirror/state";
import { tags } from "@lezer/highlight";
import { css, html, LitElement, type PropertyValues } from "lit";

import {
  formatMappedLineNumber,
  type DiffLineNumber,
} from "./diff-line-numbers.js";

export const LARGE_CODE_VIEW_THRESHOLD = 500_000;

const languageExtension = async (language: string): Promise<Extension[]> => {
  const normalized = language.toLowerCase();
  switch (normalized) {
    case "javascript":
    case "js":
    case "jsx":
    case "typescript":
    case "ts":
    case "tsx":
    case "json": {
      const { javascript } = await import("@codemirror/lang-javascript");
      return [
        javascript({
          jsx: normalized.includes("x"),
          typescript: normalized.startsWith("t"),
        }),
      ];
    }
    default: {
      const [{ StateField }, view, highlighter] = await Promise.all([
        import("@codemirror/state"),
        import("@codemirror/view"),
        import("../workers/source-highlighter.js"),
      ]);
      if (!highlighter.supportsGenericHighlighting(normalized)) return [];
      const decorations = (source: string): import("@codemirror/view").DecorationSet =>
        view.Decoration.set(
          highlighter.highlightSourceGeneric(source, normalized).map((token) =>
            view.Decoration.mark({ class: token.classes }).range(token.from, token.to)),
          true,
        );
      const field = StateField.define<import("@codemirror/view").DecorationSet>({
        create: (state) => decorations(state.doc.toString()),
        update: (value, transaction) => transaction.docChanged
          ? decorations(transaction.state.doc.toString())
          : value.map(transaction.changes),
        provide: (fieldReference) => view.EditorView.decorations.from(fieldReference),
      });
      return [field];
    }
  }
};

export const loadCodeMirrorExtensions = async (options: {
  readonly language: string;
  readonly lineWrapping: boolean;
  readonly label: string;
  readonly lineNumbers?: readonly DiffLineNumber[];
  /** Large documents keep CodeMirror's viewport virtualization but skip the
   * full-document parser/highlighter, matching the native widget's bounded
   * line instantiation without paying an unbounded token-tree cost. */
  readonly parseLanguage?: boolean;
}): Promise<Extension[]> => {
  const [{ EditorState }, { HighlightStyle, syntaxHighlighting }, view, commands, search] =
    await Promise.all([
      import("@codemirror/state"),
      import("@codemirror/language"),
      import("@codemirror/view"),
      import("@codemirror/commands"),
      import("@codemirror/search"),
    ]);
  const trouveHighlightStyle = HighlightStyle.define([
    { tag: [tags.keyword, tags.controlKeyword, tags.operatorKeyword], color: "var(--trouve-syn-keyword)" },
    { tag: [tags.string, tags.regexp], color: "var(--trouve-syn-string)" },
    { tag: [tags.number, tags.bool, tags.atom], color: "var(--trouve-syn-number)" },
    { tag: tags.comment, color: "var(--trouve-syn-comment)" },
    { tag: [tags.typeName, tags.className, tags.namespace], color: "var(--trouve-syn-type)" },
    { tag: tags.invalid, color: "var(--trouve-err)", textDecoration: "underline wavy" },
  ]);
  const extensions: Extension[] = [
    EditorState.readOnly.of(true),
    view.EditorView.editable.of(false),
    options.lineNumbers === undefined
      ? view.lineNumbers()
      : view.lineNumbers({
          formatNumber: (lineNumber) =>
            formatMappedLineNumber(lineNumber, options.lineNumbers ?? []),
        }),
    view.highlightActiveLineGutter(),
    view.drawSelection(),
    view.rectangularSelection(),
    view.crosshairCursor(),
    view.highlightSpecialChars(),
    view.keymap.of([
      ...commands.defaultKeymap,
      ...commands.historyKeymap,
      ...commands.standardKeymap,
      ...search.searchKeymap,
    ]),
    search.search({ top: true }),
    search.highlightSelectionMatches(),
    syntaxHighlighting(trouveHighlightStyle, { fallback: true }),
    view.EditorView.contentAttributes.of({
      "aria-label": options.label,
      "aria-multiline": "true",
      "aria-readonly": "true",
      role: "textbox",
      tabindex: "0",
    }),
    view.EditorView.theme({
      "&": {
        height: "100%",
        backgroundColor: "var(--trouve-code-bg)",
        color: "var(--trouve-code-fg)",
        fontFamily: "var(--trouve-font-mono)",
        fontSize: "var(--trouve-font-size)",
      },
      ".cm-scroller": { overflow: "auto" },
      ".cm-gutters": {
        backgroundColor: "var(--trouve-panel-bg)",
        color: "var(--trouve-text-faint)",
        borderRightColor: "var(--trouve-rule)",
      },
      ".cm-activeLine, .cm-activeLineGutter": {
        backgroundColor: "var(--trouve-hover-bg)",
      },
      ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
        backgroundColor: "var(--trouve-selection-bg)",
      },
      ".cm-cursor": { borderLeftColor: "var(--trouve-text-hi)" },
      ".tok-keyword, .tok-operatorKeyword": { color: "var(--trouve-syn-keyword)" },
      ".tok-string, .tok-regexp": { color: "var(--trouve-syn-string)" },
      ".tok-number, .tok-bool, .tok-atom": { color: "var(--trouve-syn-number)" },
      ".tok-comment": { color: "var(--trouve-syn-comment)" },
      ".tok-typeName, .tok-className, .tok-namespace": { color: "var(--trouve-syn-type)" },
      ".tok-invalid": { color: "var(--trouve-err)", textDecoration: "underline wavy" },
      ".cm-panels": {
        color: "var(--trouve-text)",
        backgroundColor: "var(--trouve-panel-bg)",
      },
      ".cm-search": { display: "flex", flexWrap: "wrap", gap: "4px" },
      ".cm-search input, .cm-search button": {
        minHeight: "26px",
        border: "1px solid var(--trouve-border-strong)",
        borderRadius: "var(--trouve-radius-sm)",
        color: "var(--trouve-text)",
        backgroundColor: "var(--trouve-control-bg)",
        font: "inherit",
      },
    }),
  ];
  if (options.lineWrapping) extensions.push(view.EditorView.lineWrapping);
  if (options.parseLanguage !== false) {
    extensions.push(...(await languageExtension(options.language)));
  }
  return extensions;
};

export class TrouveCodeView extends LitElement {
  static override properties = {
    content: { type: String },
    language: { type: String },
    label: { type: String },
    lineWrapping: { type: Boolean, attribute: "line-wrapping" },
  };

  static override styles = [
    css`
      :host { display: block; min-width: 0; min-height: 0; height: 100%; border: 1px solid var(--trouve-border); background: var(--trouve-code-bg); }
      #editor { width: 100%; height: 100%; min-height: 8rem; }
      .tok-keyword, .tok-operatorKeyword { color: var(--trouve-syn-keyword, var(--trouve-accent)); }
      .tok-string, .tok-regexp { color: var(--trouve-syn-string, var(--trouve-ok)); }
      .tok-number, .tok-bool, .tok-atom { color: var(--trouve-syn-number, var(--trouve-warn)); }
      .tok-comment { color: var(--trouve-syn-comment, var(--trouve-text-dim)); }
      .tok-typeName, .tok-className, .tok-namespace { color: var(--trouve-syn-type, var(--trouve-term-cyan)); }
      .tok-variableName, .tok-propertyName, .tok-labelName { color: var(--trouve-code-fg); }
      .tok-invalid { color: var(--trouve-err); text-decoration: underline wavy; }
    `,
  ];

  content = "";
  language = "text";
  label = "Source code";
  lineWrapping = false;

  #view: import("@codemirror/view").EditorView | undefined;
  #generation = 0;

  protected override firstUpdated(): void {
    void this.#updatePresentation();
  }

  protected override updated(changed: PropertyValues<this>): void {
    const previousContent = changed.get("content");
    const largeModeChanged = typeof previousContent === "string"
      && (previousContent.length > LARGE_CODE_VIEW_THRESHOLD) !== this.#usesLargeDocument;
    if (
      changed.has("language")
      || changed.has("lineWrapping")
      || changed.has("label")
      || largeModeChanged
      || (changed.has("content") && this.#view === undefined)
    ) {
      void this.#updatePresentation();
      return;
    }
    if (changed.has("content") && this.#view !== undefined) {
      const current = this.#view.state.doc.toString();
      if (current !== this.content) {
        this.#view.dispatch({ changes: { from: 0, to: current.length, insert: this.content } });
      }
    }
  }

  override disconnectedCallback(): void {
    this.#generation += 1;
    this.#view?.destroy();
    this.#view = undefined;
    super.disconnectedCallback();
  }

  revealRange(from: number, to = from): void {
    const view = this.#view;
    if (view === undefined) return;
    const safeFrom = Math.max(0, Math.min(from, view.state.doc.length));
    const safeTo = Math.max(safeFrom, Math.min(to, view.state.doc.length));
    view.dispatch({
      selection: { anchor: safeFrom, head: safeTo },
      scrollIntoView: true,
    });
    view.focus();
  }

  async #mount(): Promise<void> {
    if (!this.hasUpdated || !this.isConnected) return;
    const generation = ++this.#generation;
    const [{ EditorState }, { EditorView }, extensions] = await Promise.all([
      import("@codemirror/state"),
      import("@codemirror/view"),
      loadCodeMirrorExtensions({
        language: this.language,
        lineWrapping: this.lineWrapping,
        label: this.label,
        parseLanguage: !this.#usesLargeDocument,
      }),
    ]);
    if (generation !== this.#generation || !this.isConnected) return;
    const parent = this.renderRoot.querySelector<HTMLElement>("#editor");
    if (parent === null) return;
    this.#view?.destroy();
    parent.replaceChildren();
    this.#view = new EditorView({
      state: EditorState.create({ doc: this.content, extensions }),
      parent,
      ...(this.renderRoot instanceof ShadowRoot ? { root: this.renderRoot } : {}),
    });
  }

  get #usesLargeDocument(): boolean {
    return this.content.length > LARGE_CODE_VIEW_THRESHOLD;
  }

  async #updatePresentation(): Promise<void> {
    await this.#mount();
  }

  override render() {
    return html`<div id="editor" data-large-document=${this.#usesLargeDocument ? "true" : "false"}></div>`;
  }
}

customElements.define("trouve-code-view", TrouveCodeView);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-view": TrouveCodeView;
  }
}
