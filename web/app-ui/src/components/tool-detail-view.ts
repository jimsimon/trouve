import { css, html, LitElement, nothing, type PropertyValues } from "lit";

import { highlightSourceOffThread } from "../services/content-worker-client.js";
import type { HighlightToken } from "../workers/content-worker-protocol.js";
import { parseUnifiedDiff } from "./diff-parser.js";
import { languageForPath } from "./file-language.js";
import "./markdown-view.js";
import {
  presentToolDetail,
  type ToolDetailField,
  type ToolDetailPresentation,
} from "./tool-presentation.js";

interface SourceLine {
  readonly number: number;
  readonly from: number;
  readonly to: number;
  readonly text: string;
}

const sourceLines = (source: string, startLine: number): readonly SourceLine[] => {
  if (source === "") return [];
  const rows = source.split("\n");
  if (rows.at(-1) === "") rows.pop();
  let offset = 0;
  return rows.map((text, index) => {
    const line = { number: startLine + index, from: offset, to: offset + text.length, text };
    offset += text.length + 1;
    return line;
  });
};

/** Lightweight selectable source excerpt. Unlike the full Files code view,
 * this does not mount an editor per search result. Lezer's recovery parser
 * highlights partial JS/TS syntax; other languages use the bounded lexical
 * highlighter, so excerpts do not require a complete syntax tree. */
export class TrouveSourceExcerpt extends LitElement {
  static override properties = {
    content: { type: String },
    path: { type: String },
    startLine: { type: Number, attribute: "start-line" },
    totalLines: { type: Number, attribute: "total-lines" },
    truncated: { type: Boolean },
    label: { type: String },
    tokens: { state: true },
  };

  static override styles = css`
    :host { min-width: 0; max-width: 100%; display: block; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius-sm); background: var(--trouve-code-bg); color: var(--trouve-code-fg); }
    .source-scroll { min-width: 0; max-width: 100%; max-height: min(34rem, 62vh); overflow: auto; }
    .source-empty { margin: 0; padding: 10px 12px; color: var(--trouve-text-dim); font-style: italic; }
    .source-line { width: max-content; min-width: 100%; display: grid; grid-template-columns: minmax(3.5em, auto) minmax(max-content, 1fr); align-items: stretch; font-family: var(--trouve-font-mono); font-size: 12px; line-height: 1.55; }
    .source-line:hover { background: var(--trouve-hover-bg); }
    .source-number { position: sticky; inset-inline-start: 0; z-index: 1; padding: 0 9px 0 7px; border-inline-end: 1px solid var(--trouve-rule); color: var(--trouve-text-faint); background: var(--trouve-panel-bg); text-align: end; font-variant-numeric: tabular-nums; user-select: none; }
    code { min-width: max-content; padding: 0 12px; color: inherit; white-space: pre; }
    footer { padding: 5px 9px; border-block-start: 1px solid var(--trouve-rule); color: var(--trouve-text-dim); font-size: 11px; }
    .tok-keyword, .tok-operatorKeyword { color: var(--trouve-syn-keyword, var(--trouve-accent)); }
    .tok-string, .tok-regexp { color: var(--trouve-syn-string, var(--trouve-ok)); }
    .tok-number, .tok-bool, .tok-atom { color: var(--trouve-syn-number, var(--trouve-warn)); }
    .tok-comment { color: var(--trouve-syn-comment, var(--trouve-text-dim)); }
    .tok-typeName, .tok-className, .tok-namespace { color: var(--trouve-syn-type, var(--trouve-term-cyan)); }
    .tok-variableName, .tok-propertyName, .tok-labelName { color: var(--trouve-code-fg); }
    .tok-invalid { color: var(--trouve-err); text-decoration: underline wavy; }
  `;

  content = "";
  path = "";
  startLine = 1;
  totalLines = 0;
  truncated = false;
  label = "Source excerpt";
  protected tokens: readonly HighlightToken[] = [];

  #highlightGeneration = 0;

  protected override updated(changed: PropertyValues<this>): void {
    if (changed.has("content") || changed.has("path")) void this.#highlight();
  }

  override disconnectedCallback(): void {
    this.#highlightGeneration += 1;
    super.disconnectedCallback();
  }

  async #highlight(): Promise<void> {
    const generation = ++this.#highlightGeneration;
    this.tokens = [];
    let tokens: readonly HighlightToken[];
    try {
      tokens = await highlightSourceOffThread(this.content, languageForPath(this.path));
    } catch {
      return;
    }
    if (generation !== this.#highlightGeneration || !this.isConnected) return;
    this.tokens = tokens;
  }

  #renderLine(line: SourceLine) {
    const fragments = [];
    let cursor = line.from;
    for (const token of this.tokens) {
      if (token.to <= line.from) continue;
      if (token.from >= line.to) break;
      const from = Math.max(cursor, token.from, line.from);
      const to = Math.min(token.to, line.to);
      if (from > cursor) fragments.push(this.content.slice(cursor, from));
      if (to > from) {
        fragments.push(html`<span class=${token.classes}>${this.content.slice(from, to)}</span>`);
        cursor = to;
      }
    }
    if (cursor < line.to) fragments.push(this.content.slice(cursor, line.to));
    return html`<div class="source-line" role="row">
      <span class="source-number" role="rowheader">${line.number}</span>
      <code role="cell">${fragments}</code>
    </div>`;
  }

  override render() {
    const lines = sourceLines(this.content, Math.max(1, this.startLine));
    const lastLine = lines.at(-1)?.number ?? Math.max(1, this.startLine);
    const summary = this.totalLines > 0
      ? `Showing lines ${Math.max(1, this.startLine)}–${lastLine} of ${this.totalLines}.`
      : `Showing lines ${Math.max(1, this.startLine)}–${lastLine}.`;
    return html`
      <div class="source-scroll" role="table" aria-label=${this.label}>
        ${lines.length === 0
          ? html`<p class="source-empty">No source text was returned.</p>`
          : lines.map((line) => this.#renderLine(line))}
      </div>
      ${this.truncated ? html`<footer>${summary} More source is available.</footer>` : nothing}
    `;
  }
}

export class TrouveToolDetailView extends LitElement {
  static override properties = {
    tool: { type: String },
    args: { attribute: false },
    result: { attribute: false },
    output: { type: String },
    outputOmitted: { type: Boolean, attribute: "output-omitted" },
  };

  static override styles = css`
    :host { min-width: 0; max-width: 100%; display: block; border-block-start: 1px solid var(--trouve-rule); color: var(--trouve-text); }
    .tool-detail { min-width: 0; max-width: 100%; display: grid; gap: 10px; padding: 10px 12px 12px; }
    section { min-width: 0; max-width: 100%; display: grid; gap: 6px; }
    h4 { margin: 0; color: var(--trouve-text-mid); font-size: 11px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; }
    dl { min-width: 0; display: grid; grid-template-columns: minmax(7rem, max-content) minmax(0, 1fr); gap: 4px 12px; margin: 0; }
    dt { color: var(--trouve-text-dim); }
    dd { min-width: 0; margin: 0; color: var(--trouve-text); overflow-wrap: anywhere; white-space: pre-wrap; }
    dd code, td code, .path-list code { color: var(--trouve-code-fg); font-family: var(--trouve-font-mono); }
    .result-scroll { min-width: 0; max-width: 100%; overflow: auto; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius-sm); }
    table { width: 100%; min-width: 32rem; border-collapse: collapse; font-size: 12px; }
    caption { padding: 6px 8px; color: var(--trouve-text-dim); text-align: start; }
    th { position: sticky; inset-block-start: 0; z-index: 1; padding: 6px 8px; border-block-end: 1px solid var(--trouve-rule); color: var(--trouve-text-dim); background: var(--trouve-panel-bg); text-align: start; font-weight: 600; }
    td { min-width: 0; padding: 6px 8px; border-block-end: 1px solid var(--trouve-rule); vertical-align: top; }
    tbody:last-child tr:last-child td { border-block-end: 0; }
    .rank, .score, .line-number { width: 1%; color: var(--trouve-text-dim); text-align: end; white-space: nowrap; font-variant-numeric: tabular-nums; }
    .search-location { overflow-wrap: anywhere; }
    .location-button { max-width: 100%; display: inline; padding: 0; border: 0; color: var(--trouve-link, var(--trouve-accent)); background: transparent; font: inherit; text-align: start; overflow-wrap: anywhere; cursor: pointer; }
    .location-button:hover { text-decoration: underline; }
    .location-button:focus-visible { outline: 2px solid var(--trouve-accent); outline-offset: 2px; border-radius: 2px; }
    .location-button code { color: inherit; }
    .search-snippet td { padding: 0 8px 8px; background: color-mix(in srgb, var(--trouve-code-bg) 45%, transparent); }
    .search-snippet trouve-source-excerpt { max-height: 18rem; }
    .match-text { font-family: var(--trouve-font-mono); white-space: pre-wrap; overflow-wrap: anywhere; }
    .path-list { max-height: min(28rem, 55vh); display: grid; gap: 2px; overflow: auto; margin: 0; padding: 7px 10px 7px 2.2rem; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius-sm); background: var(--trouve-code-bg); }
    .path-list li { padding-inline-start: 3px; color: var(--trouve-text-dim); }
    .path-list code { color: var(--trouve-code-fg); overflow-wrap: anywhere; }
    pre { max-width: 100%; max-height: min(32rem, 60vh); overflow: auto; margin: 0; padding: 9px 11px; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius-sm); color: var(--trouve-code-fg); background: var(--trouve-code-bg); font-family: var(--trouve-font-mono); font-size: 12px; line-height: 1.5; white-space: pre-wrap; overflow-wrap: anywhere; }
    pre.stderr, pre.error { border-color: color-mix(in srgb, var(--trouve-err) 55%, var(--trouve-card-border)); color: var(--trouve-err); }
    .document { max-height: min(38rem, 65vh); overflow: auto; padding: 10px 12px; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius-sm); background: var(--trouve-code-bg); }
    .diff-files { min-width: 0; max-height: min(42rem, 68vh); display: grid; gap: 10px; overflow: auto; }
    .diff-file { gap: 0; overflow: hidden; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius-sm); background: var(--trouve-code-bg); }
    .diff-file-header { min-width: 0; display: flex; align-items: center; gap: 8px; padding: 6px 9px; border-block-end: 1px solid var(--trouve-rule); background: var(--trouve-panel-bg); }
    .diff-file-header code { min-width: 0; flex: 1; overflow: hidden; color: var(--trouve-code-fg); font-family: var(--trouve-font-mono); text-overflow: ellipsis; white-space: nowrap; }
    .diff-stat { font-size: 11px; font-variant-numeric: tabular-nums; white-space: nowrap; }
    .diff-stat.add { color: var(--trouve-ok); }
    .diff-stat.delete { color: var(--trouve-err); }
    .diff-rows { min-width: max-content; width: 100%; display: table; border-collapse: collapse; font-family: var(--trouve-font-mono); font-size: 12px; line-height: 1.5; }
    .diff-row { display: table-row; }
    .diff-row > span, .diff-row > code { display: table-cell; }
    .diff-row.add { background: var(--trouve-diff-add-bg); }
    .diff-row.delete { background: var(--trouve-diff-del-bg); }
    .diff-row.hunk { color: var(--trouve-text-dim); background: var(--trouve-raised-bg); }
    .diff-number { width: 1%; min-width: 4ch; padding: 0 6px; border-inline-end: 1px solid var(--trouve-rule); color: var(--trouve-text-faint); text-align: end; user-select: none; }
    .diff-mark { width: 1%; padding-inline: 7px 3px; color: var(--trouve-text-dim); user-select: none; }
    .diff-row.add .diff-mark { color: var(--trouve-ok); }
    .diff-row.delete .diff-mark { color: var(--trouve-err); }
    .diff-row code { min-width: 30ch; padding-inline-end: 12px; color: var(--trouve-code-fg); white-space: pre; }
    .notice, .empty { margin: 0; color: var(--trouve-text-dim); font-size: 12px; }
    .notice { padding: 5px 8px; border-inline-start: 3px solid var(--trouve-warn); background: var(--trouve-warn-bg); }
    @media (max-width: 680px) {
      .tool-detail { padding-inline: 8px; }
      dl { grid-template-columns: minmax(0, 1fr); gap: 1px; }
      dd { margin-block-end: 5px; }
    }
  `;

  tool = "";
  args: unknown = {};
  result: unknown;
  output = "";
  outputOmitted = false;

  #renderFields(title: string, fields: readonly ToolDetailField[]) {
    if (fields.length === 0) return nothing;
    return html`<section aria-label=${title}>
      <h4>${title}</h4>
      <dl>${fields.map((field) => html`
        <dt>${field.label}</dt>
        <dd>${field.code ? html`<code>${field.value}</code>` : field.value}</dd>
      `)}</dl>
    </section>`;
  }

  #openFile(path: string, from: number, to = from): void {
    this.dispatchEvent(new CustomEvent("trouve-open-file", {
      detail: { path, from, to },
      bubbles: true,
      composed: true,
    }));
  }

  #renderSearch(detail: Extract<ToolDetailPresentation, { readonly kind: "search" }>) {
    return html`
      ${this.#renderFields("Inputs", detail.inputs)}
      <section aria-label="Results">
        <h4>Results</h4>
        ${detail.results.length === 0
          ? html`<p class="empty">No matching code was found.</p>`
          : html`<div class="result-scroll"><table>
              <caption>${detail.results.length} ranked ${detail.results.length === 1 ? "result" : "results"}</caption>
              <thead><tr><th scope="col">#</th><th scope="col">Location</th><th scope="col">Score</th></tr></thead>
              ${detail.results.map((row, index) => html`<tbody>
                <tr>
                  <td class="rank">${index + 1}</td>
                  <td class="search-location"><button
                    class="location-button"
                    type="button"
                    title=${`Open ${row.path} at lines ${row.startLine}–${row.endLine}`}
                    @click=${() => this.#openFile(row.path, row.startLine, row.endLine)}
                  ><code>${row.path}:${row.startLine}${row.endLine > row.startLine ? `–${row.endLine}` : ""}</code></button></td>
                  <td class="score">${row.score === undefined ? "—" : row.score.toFixed(3)}</td>
                </tr>
                ${row.content === "" ? nothing : html`<tr class="search-snippet"><td></td><td colspan="2">
                  <trouve-source-excerpt
                    .content=${row.content}
                    .path=${row.path}
                    .startLine=${row.startLine}
                    label=${`${row.path} search result`}
                  ></trouve-source-excerpt>
                </td></tr>`}
              </tbody>`)}
            </table></div>`}
        ${detail.truncated ? html`<p class="notice">Additional results were omitted.</p>` : nothing}
      </section>
    `;
  }

  #renderMatches(detail: Extract<ToolDetailPresentation, { readonly kind: "matches" }>) {
    return html`
      ${this.#renderFields("Inputs", detail.inputs)}
      <section aria-label="Matches">
        <h4>Matches</h4>
        ${detail.matches.length === 0
          ? html`<p class="empty">No matches were found.</p>`
          : html`<div class="result-scroll"><table>
              <caption>${detail.matches.length} ${detail.matches.length === 1 ? "match" : "matches"}</caption>
              <thead><tr><th scope="col">Location</th><th scope="col">Match</th></tr></thead>
              <tbody>${detail.matches.map((match) => html`<tr>
                <td class="search-location"><button
                  class="location-button"
                  type="button"
                  title=${`Open ${match.path} at line ${match.line}`}
                  @click=${() => this.#openFile(match.path, match.line)}
                ><code>${match.path}:${match.line}</code></button></td>
                <td class="match-text">${match.text}</td>
              </tr>`)}</tbody>
            </table></div>`}
        ${detail.truncated ? html`<p class="notice">The match limit was reached; additional matches were omitted.</p>` : nothing}
      </section>
    `;
  }

  #renderTranscript(detail: Extract<ToolDetailPresentation, { readonly kind: "transcript" }>) {
    return html`
      ${this.#renderFields("Inputs", detail.inputs)}
      ${detail.matches.length === 0 ? nothing : html`<section aria-label="Transcript matches">
        <h4>Matches</h4>
        <div class="result-scroll"><table>
          <thead><tr><th scope="col">Thread / turn</th><th scope="col">Role</th><th scope="col">Snippet</th></tr></thead>
          <tbody>${detail.matches.map((match) => html`<tr>
            <td class="search-location"><code>${match.threadId} · ${match.turn}</code></td>
            <td>${match.role}</td>
            <td>${match.snippet}</td>
          </tr>`)}</tbody>
        </table></div>
      </section>`}
      ${this.#renderFields("Turn messages", detail.messages)}
      ${detail.matches.length === 0 && detail.messages.length === 0
        ? html`<p class="empty">No transcript content was returned.</p>`
        : nothing}
      ${detail.truncated ? html`<p class="notice">Additional transcript matches were omitted.</p>` : nothing}
    `;
  }

  #renderDiff(detail: Extract<ToolDetailPresentation, { readonly kind: "diff" }>) {
    const files = parseUnifiedDiff(detail.diff);
    return html`
      ${this.#renderFields("Request", detail.inputs)}
      <section aria-label="Diff"><h4>Diff</h4>
        ${detail.diff === ""
          ? html`<p class="empty">No changes were returned.</p>`
          : files.length === 0
            ? html`<pre>${detail.diff}</pre>`
            : html`<div class="diff-files">${files.map((file) => html`
                <section class="diff-file" aria-label=${`${file.path} diff`}>
                  <header class="diff-file-header">
                    <code title=${file.path}>${file.path}</code>
                    <span class="diff-stat add">+${file.additions}</span>
                    <span class="diff-stat delete">−${file.deletions}</span>
                  </header>
                  <div class="result-scroll">
                    <div class="diff-rows" role="table" aria-label=${`${file.path} unified diff`}>
                      ${file.rows.map((row) => html`<div class=${`diff-row ${row.kind}`} role="row">
                        <span class="diff-number" role="cell">${row.oldNumber ?? ""}</span>
                        <span class="diff-number" role="cell">${row.newNumber ?? ""}</span>
                        <span class="diff-mark" aria-hidden="true">${row.kind === "add" ? "+" : row.kind === "delete" ? "−" : ""}</span>
                        <code role="cell">${row.text}</code>
                      </div>`)}
                    </div>
                  </div>
                </section>
              `)}</div>`}
        ${detail.truncated ? html`<p class="notice">This is a partial diff${detail.nextOffset === undefined ? "" : `; continue at byte ${detail.nextOffset}`}${detail.totalBytes === undefined ? "" : ` of ${detail.totalBytes}`}.</p>` : nothing}
      </section>
    `;
  }

  #renderDetail(detail: ToolDetailPresentation) {
    switch (detail.kind) {
      case "source":
        return html`
          ${this.#renderFields("Request", detail.inputs)}
          <trouve-source-excerpt
            .content=${detail.source.content}
            .path=${detail.source.path}
            .startLine=${detail.source.startLine}
            .totalLines=${detail.source.totalLines ?? 0}
            .truncated=${detail.source.truncated}
            label=${`${detail.source.path || "File"} source excerpt`}
          ></trouve-source-excerpt>
        `;
      case "search": return this.#renderSearch(detail);
      case "matches": return this.#renderMatches(detail);
      case "paths":
        return html`
          ${this.#renderFields("Inputs", detail.inputs)}
          <section aria-label="Paths"><h4>Paths</h4>
            ${detail.paths.length === 0
              ? html`<p class="empty">No paths were found.</p>`
              : html`<ol class="path-list">${detail.paths.map((path) => html`<li><code>${path}</code></li>`)}</ol>`}
            ${detail.truncated ? html`<p class="notice">Additional paths were omitted.</p>` : nothing}
          </section>
        `;
      case "command":
        return html`
          ${this.#renderFields("Execution", detail.inputs)}
          ${detail.stdout === "" ? nothing : html`<section><h4>Standard output</h4><pre>${detail.stdout}</pre></section>`}
          ${detail.stderr === "" ? nothing : html`<section><h4>Standard error</h4><pre class="stderr">${detail.stderr}</pre></section>`}
          ${detail.stdout === "" && detail.stderr === "" ? html`<p class="empty">The command produced no output.</p>` : nothing}
          ${detail.truncated ? html`<p class="notice">Command output was truncated.</p>` : nothing}
        `;
      case "document":
        return html`
          ${this.#renderFields("Request", detail.inputs)}
          <section><h4>Content</h4>
            ${detail.language === "json"
              ? html`<trouve-source-excerpt .content=${detail.content} path="response.json" label="Fetched JSON"></trouve-source-excerpt>`
              : html`<div class="document"><trouve-markdown-view .content=${detail.content}></trouve-markdown-view></div>`}
          </section>
          ${detail.truncated ? html`<p class="notice">Only the requested page of this document is shown.</p>` : nothing}
        `;
      case "diff": return this.#renderDiff(detail);
      case "transcript": return this.#renderTranscript(detail);
      case "structured":
        return html`
          ${this.#renderFields("Inputs", detail.inputs)}
          ${detail.resultText === ""
            ? html`<p class="empty">No result has been returned yet.</p>`
            : html`<section><h4>${detail.error ? "Error" : "Result"}</h4><pre class=${detail.error ? "error" : ""}>${detail.resultText}</pre></section>`}
        `;
    }
  }

  override render() {
    const detail = presentToolDetail(this.tool, this.args, this.result);
    const representedOutput = detail.kind === "command"
      ? `${detail.stdout}\n${detail.stderr}`.trim()
      : "";
    const liveOutput = this.output.trim() !== "" && !representedOutput.includes(this.output.trim())
      ? this.output
      : "";
    return html`<div class="tool-detail">
      ${this.#renderDetail(detail)}
      ${liveOutput === "" && !this.outputOmitted ? nothing : html`<section aria-label="Live tool output">
        <h4>Live output</h4>
        ${this.outputOmitted ? html`<p class="notice">Earlier live output was omitted to keep this session responsive.</p>` : nothing}
        ${liveOutput === "" ? nothing : html`<pre>${liveOutput}</pre>`}
      </section>`}
    </div>`;
  }
}

customElements.define("trouve-source-excerpt", TrouveSourceExcerpt);
customElements.define("trouve-tool-detail-view", TrouveToolDetailView);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-source-excerpt": TrouveSourceExcerpt;
    "trouve-tool-detail-view": TrouveToolDetailView;
  }
}
