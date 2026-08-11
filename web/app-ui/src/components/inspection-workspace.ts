import { ContextConsumer } from "@lit/context";
import { html, LitElement, nothing, type PropertyValues } from "lit";

import {
  appServicesContext,
  hostCapabilitiesContext,
  sessionContext,
} from "../contexts/app-contexts.js";
import {
  ProtocolClientError,
  type ProtocolFileContent,
  type ProtocolSessionDiffFileSummary,
} from "../services/protocol-client.js";
import { prepareUnifiedDiffOffThread } from "../services/content-worker-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  selectedDiffIndexAfterRefresh,
  type ParsedDiffFile,
} from "./diff-parser.js";
import type { DiffMode } from "./diff-mode.js";
import {
  copyRawDiffToClipboard,
  type ClipboardTextWriter,
} from "./inspection-diff-controls.js";
import {
  fileTreeDirectoriesForPaths,
  InspectionFileTreeModel,
  type FileTreeRow,
} from "./inspection-file-tree.js";
import { lineRangeOffsets, parentDirectories } from "./file-reveal.js";
import { languageForPath } from "./file-language.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import type { TrouveCodeView } from "./code-view.js";
import "./code-view.js";
import "./diff-view.js";
import "./markdown-view.js";

type WorkspaceInspection = "diff" | "files";
const DIFF_REFRESH_MS = 2_000;
const FILES_REFRESH_MS = 10_000;
const MOBILE_FILES_QUERY = "(max-width: 760px)";

export class TrouveInspectionWorkspace extends withSignalTracking(LitElement) {
  static override properties = {
    sessionId: { type: String, attribute: "session-id" },
    panel: { type: String },
  };

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  sessionId = "";
  panel: WorkspaceInspection = "diff";
  #generation = 0;
  #loading = false;
  #diffRequestActive = false;
  #diffLoaded = false;
  #refreshing = false;
  #notice = "";
  #noticeIsError = false;
  #copyFeedback = "";
  #copyFeedbackIsError = false;
  #copiedDiffPath = "";
  #copySequence = 0;
  #copyFeedbackTimer: ReturnType<typeof setTimeout> | undefined;
  #diffRefreshTimer: ReturnType<typeof setInterval> | undefined;
  #filesRefreshTimer: ReturnType<typeof setInterval> | undefined;
  #filesRefreshActive = false;
  #error = "";
  #diffManifest = "";
  #diffFiles: readonly ProtocolSessionDiffFileSummary[] = [];
  #selectedDiffFile: ParsedDiffFile | undefined;
  #selectedDiffText = "";
  #diffFileLoadingPath = "";
  #diffFileError = "";
  #diffFileErrorPath = "";
  #diffFileGeneration = 0;
  #selectedDiff = 0;
  #diffMode: DiffMode = "unified";
  readonly #diffFileTree = new InspectionFileTreeModel();
  #diffTreeDirectories = new Set<string>();
  #diffTreeExpanded = new Set<string>();
  #diffTreeInitialized = false;
  #diffTreeCollapsed = false;
  #fileTreeGeneration = 0;
  #fileGeneration = 0;
  readonly #fileTree = new InspectionFileTreeModel();
  #file: ProtocolFileContent | undefined;
  #fileTargetPath = "";
  #fileLoadingPath = "";
  #fileError = "";
  #filePreview = false;
  #fileTreeCollapsed = false;
  #fileActionPending: "open" | "reveal" | "" = "";
  #fileActionGeneration = 0;
  #observedSessionId = "";
  #sessionScopeChanged = false;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #capabilities = new ContextConsumer(this, {
    context: hostCapabilitiesContext,
    subscribe: true,
  });
  readonly #sessionScope = new ContextConsumer(this, {
    context: sessionContext,
    subscribe: true,
  });

  get #effectiveSessionId(): string {
    return this.sessionId || this.#sessionScope.value?.sessionId || "";
  }

  readonly #checkpointRestored = (event: Event): void => {
    const detail = (event as CustomEvent<{ readonly sessionId?: string }>).detail;
    if (detail?.sessionId !== this.#effectiveSessionId || !this.isConnected) return;
    if (this.panel === "diff") {
      void this.#refreshDiff({ silent: false, force: true });
      return;
    }
    const selectedPath = this.#file?.path ?? this.#fileTargetPath;
    void this.#loadRootDirectory(true).then(() => {
      if (selectedPath !== "" && this.panel === "files" && this.isConnected) {
        void this.#loadFile(selectedPath);
      }
    });
  };

  override connectedCallback(): void {
    super.connectedCallback();
    globalThis.addEventListener("trouve-checkpoint-restored", this.#checkpointRestored);
    this.#diffRefreshTimer ??= globalThis.setInterval(() => {
      if (
        this.panel !== "diff" ||
        this.#diffRequestActive ||
        (typeof document !== "undefined" && document.visibilityState === "hidden")
      ) return;
      void this.#refreshDiff({ silent: true });
    }, DIFF_REFRESH_MS);
    this.#filesRefreshTimer ??= globalThis.setInterval(() => {
      if (
        this.panel !== "files"
        || (typeof document !== "undefined" && document.visibilityState === "hidden")
      ) return;
      void this.#refreshVisibleFiles();
    }, FILES_REFRESH_MS);
  }

  protected override willUpdate(changed: PropertyValues<this>): void {
    const effectiveSessionId = this.#effectiveSessionId;
    this.#sessionScopeChanged =
      changed.has("sessionId") || effectiveSessionId !== this.#observedSessionId;
    if (this.#sessionScopeChanged) this.#observedSessionId = effectiveSessionId;
    if (changed.has("panel")) {
      this.#generation += 1;
      this.#diffFileGeneration += 1;
      this.#copySequence += 1;
      this.#loading = false;
      this.#diffRequestActive = false;
      this.#refreshing = false;
      this.#copyFeedback = "";
      this.#copyFeedbackIsError = false;
      this.#copiedDiffPath = "";
      this.#clearCopyFeedbackTimer();
      this.#error = "";
    }
    if (this.#sessionScopeChanged) {
      this.#generation += 1;
      this.#diffFileGeneration += 1;
      this.#fileTreeGeneration += 1;
      this.#fileGeneration += 1;
      this.#fileActionGeneration += 1;
      this.#copySequence += 1;
      this.#loading = false;
      this.#diffRequestActive = false;
      this.#diffLoaded = false;
      this.#refreshing = false;
      this.#notice = "";
      this.#noticeIsError = false;
      this.#copyFeedback = "";
      this.#copyFeedbackIsError = false;
      this.#copiedDiffPath = "";
      this.#clearCopyFeedbackTimer();
      this.#error = "";
      this.#diffManifest = "";
      this.#diffFiles = [];
      this.#selectedDiffFile = undefined;
      this.#selectedDiffText = "";
      this.#diffFileLoadingPath = "";
      this.#diffFileError = "";
      this.#diffFileErrorPath = "";
      this.#selectedDiff = 0;
      this.#diffMode = "unified";
      this.#diffFileTree.clear();
      this.#diffTreeDirectories.clear();
      this.#diffTreeExpanded.clear();
      this.#diffTreeInitialized = false;
      this.#diffTreeCollapsed = false;
      this.#fileTree.clear();
      this.#file = undefined;
      this.#fileTargetPath = "";
      this.#fileLoadingPath = "";
      this.#fileError = "";
      this.#filePreview = false;
      this.#fileTreeCollapsed = false;
      this.#fileActionPending = "";
    }
  }

  protected override updated(changed: PropertyValues<this>): void {
    if (this.#sessionScopeChanged || changed.has("panel")) void this.#load();
    this.#sessionScopeChanged = false;
  }

  override disconnectedCallback(): void {
    globalThis.removeEventListener("trouve-checkpoint-restored", this.#checkpointRestored);
    this.#generation += 1;
    this.#diffFileGeneration += 1;
    this.#fileTreeGeneration += 1;
    this.#fileGeneration += 1;
    this.#fileActionGeneration += 1;
    this.#copySequence += 1;
    this.#clearCopyFeedbackTimer();
    if (this.#diffRefreshTimer !== undefined) {
      globalThis.clearInterval(this.#diffRefreshTimer);
      this.#diffRefreshTimer = undefined;
    }
    if (this.#filesRefreshTimer !== undefined) {
      globalThis.clearInterval(this.#filesRefreshTimer);
      this.#filesRefreshTimer = undefined;
    }
    super.disconnectedCallback();
  }

  override render() {
    if (this.panel === "diff" && this.#loading) {
      return html`<div class="screen-empty" role="status"><span>Loading ${this.panel}…</span></div>`;
    }
    if (this.panel === "diff" && this.#error !== "") {
      return html`<div class="screen-empty" role="alert"><strong>Unable to load ${this.panel}</strong><span>${this.#error}</span><span>Retrying automatically.</span></div>`;
    }
    return this.panel === "diff" ? this.#renderDiff() : this.#renderFiles();
  }

  #renderDiff() {
    const additions = this.#diffFiles.reduce((total, file) => total + file.additions, 0);
    const deletions = this.#diffFiles.reduce((total, file) => total + file.deletions, 0);
    const selectedFile = this.#diffFiles[this.#selectedDiff] ?? this.#diffFiles[0];
    const selectedPatch = this.#selectedDiffFile?.path === selectedFile?.path
      ? this.#selectedDiffFile
      : undefined;
    const diffFilesByPath = new Map(this.#diffFiles.map((file) => [file.path, file]));
    return html`
      <section
        class=${`inspection-split diff-inspection ${
          this.#diffTreeCollapsed ? "diff-tree-collapsed" : ""
        } ${this.#diffFiles.length === 0 ? "diff-empty-inspection" : ""}`}
        aria-busy=${
          this.#refreshing ||
          this.#diffFileLoadingPath !== ""
            ? "true"
            : "false"
        }
      >
        <header class="inspection-summary diff-summary">
          <span class="visually-hidden">${this.#diffFiles.length} file${this.#diffFiles.length === 1 ? "" : "s"} changed, ${additions} additions, ${deletions} deletions</span>
          ${selectedFile === undefined
            ? nothing
            : html`<button
                type="button"
                aria-expanded=${this.#diffTreeCollapsed ? "false" : "true"}
                aria-controls="session-diff-file-tree"
                aria-label=${this.#diffTreeCollapsed ? "Show changed files" : "Hide changed files"}
                title=${this.#diffTreeCollapsed ? "Show changed files" : "Hide changed files"}
                @click=${() => {
                  this.#diffTreeCollapsed = !this.#diffTreeCollapsed;
                  this.requestUpdate();
                }}
              >${fontAwesomeIcon("folder-tree")}</button>
              <strong title=${selectedFile.path}>${selectedFile.path}</strong>`}
          <span class="diff-summary-actions" role="group" aria-label="Diff actions">
            ${selectedPatch === undefined
              ? nothing
              : html`<button
                  type="button"
                  aria-label=${`Copy raw diff for ${selectedPatch.path}`}
                  title=${`Copy raw diff for ${selectedPatch.path}`}
                  @click=${() => void this.#copyFileDiff(selectedPatch)}
                >${fontAwesomeIcon(
                  this.#copiedDiffPath === selectedPatch.path && !this.#copyFeedbackIsError
                    ? "check"
                    : "copy",
                )}</button>`}
          </span>
          ${this.#notice === ""
            ? nothing
            : html`<span
                class="diff-notice ${this.#noticeIsError ? "error" : ""}"
                role=${this.#noticeIsError ? "alert" : "status"}
                aria-live="polite"
              >${this.#notice}</span>`}
          ${this.#copyFeedback === ""
            ? nothing
            : html`<span
                class="diff-notice ${this.#copyFeedbackIsError ? "error" : ""}"
                role=${this.#copyFeedbackIsError ? "alert" : "status"}
                aria-live="polite"
                aria-atomic="true"
              >${this.#copyFeedback}</span>`}
        </header>
        ${this.#diffFiles.length === 0
          ? html`<div class="diff-empty" role="status" aria-label="No changes"></div>`
          : html`
              <div
                id="session-diff-file-tree"
                class="inspection-file-list file-tree diff-file-tree"
                role="tree"
                aria-label="Changed files"
              >
                ${this.#diffFileTree.rows.map((row) => {
                  const file = row.isDirectory
                    ? undefined
                    : diffFilesByPath.get(row.path);
                  const selected = file !== undefined && file.path === selectedFile?.path;
                  return html`<button
                    type="button"
                    role="treeitem"
                    aria-level=${row.level}
                    aria-posinset=${row.positionInSet}
                    aria-setsize=${row.setSize}
                    aria-expanded=${row.isDirectory ? String(row.expanded) : nothing}
                    aria-selected=${row.isDirectory ? nothing : selected ? "true" : "false"}
                    tabindex=${this.#diffFileTree.activePath === row.path ? 0 : -1}
                    data-diff-file-path=${row.path}
                    class=${`file-row file-tree-item diff-file-tree-item ${selected ? "selected" : ""}`}
                    style=${`--file-tree-indent:${row.depth * 15}px`}
                    title=${row.path}
                    @focus=${() => this.#diffFileTree.setActive(row.path)}
                    @keydown=${(event: KeyboardEvent) => this.#navigateDiffFileTree(event, row.path)}
                    @click=${() => this.#activateDiffFileTreeRow(row.path)}
                  >
                    <span class="file-tree-label">
                      ${row.isDirectory
                        ? fontAwesomeIcon(row.expanded ? "caret-down" : "caret-right", {
                            className: "file-tree-disclosure",
                          })
                        : html`<span class="file-tree-disclosure"></span>`}
                      ${fontAwesomeIcon(
                        row.isDirectory ? row.expanded ? "folder-open" : "folder" : "file-lines",
                        { className: "file-tree-icon" },
                      )}
                      <span class="file-tree-name">${row.name}</span>
                      ${file === undefined
                        ? nothing
                        : html`<small class="diff-tree-change-count">+${file.additions} −${file.deletions}</small>`}
                    </span>
                  </button>`;
                })}
              </div>
              ${selectedFile === undefined
                ? html`<div class="screen-empty diff-view-state"><span>Select a changed file to view its diff.</span></div>`
                : this.#renderSelectedDiff(selectedFile, selectedPatch)}
            `}
      </section>
    `;
  }

  #renderSelectedDiff(
    file: ProtocolSessionDiffFileSummary,
    patch: ParsedDiffFile | undefined,
  ) {
    if (file.binary) {
      return html`<div class="screen-empty diff-view-state" role="status"><span>Binary file changed.</span></div>`;
    }
    if (this.#diffFileLoadingPath === file.path) {
      return html`<div class="screen-empty diff-view-state" role="status"><span>Loading ${file.path}…</span></div>`;
    }
    if (this.#diffFileErrorPath === file.path && this.#diffFileError !== "") {
      return html`<div class="screen-empty diff-view-state" role="alert">
        <strong>Unable to load ${file.path}</strong>
        <span>${this.#diffFileError}</span>
        <span>Retrying automatically.</span>
      </div>`;
    }
    if (patch === undefined) {
      return html`<div class="screen-empty diff-view-state" role="status"><span>No diff content is available for ${file.path}.</span></div>`;
    }
    return html`
      <trouve-diff-view
        class="inspection-widget diff-view-shell"
        .original=${patch.original}
        .modified=${patch.modified}
        .originalLineNumbers=${patch.originalLineNumbers}
        .modifiedLineNumbers=${patch.modifiedLineNumbers}
        .mode=${this.#diffMode}
        language=${languageForPath(file.path)}
        label=${file.path}
        @trouve-diff-mode-change=${(event: CustomEvent<{ readonly mode: DiffMode }>) => {
          this.#diffMode = event.detail.mode;
          this.requestUpdate();
        }}
      ></trouve-diff-view>
    `;
  }

  #navigateDiffFileTree(event: KeyboardEvent, currentPath: string): void {
    if (event.altKey || event.ctrlKey || event.metaKey || event.isComposing) return;
    const action = this.#diffFileTree.actionForKey(event.key, currentPath);
    if (action === undefined) return;
    event.preventDefault();
    event.stopPropagation();
    if (action.kind === "focus") {
      this.#diffFileTree.setActive(action.path);
      this.#focusDiffFileTreePath(action.path);
      return;
    }
    if (action.kind === "expand") {
      this.#diffFileTree.expand(action.path);
      this.#diffTreeExpanded.add(action.path);
      this.#restoreDiffTreeExpansions();
      this.#diffFileTree.setActive(action.path);
      this.#focusDiffFileTreePath(action.path);
      return;
    }
    if (action.kind === "collapse") {
      this.#diffTreeExpanded.delete(action.path);
      this.#diffFileTree.collapse(action.path);
      this.#focusDiffFileTreePath(action.path);
      return;
    }
    this.#activateDiffFileTreeRow(action.path);
  }

  #activateDiffFileTreeRow(path: string): void {
    const row = this.#diffFileTree.row(path);
    if (row === undefined) return;
    this.#diffFileTree.setActive(path);
    if (row.isDirectory) {
      const result = this.#diffFileTree.toggle(path);
      if (result === "expanded") {
        this.#diffTreeExpanded.add(path);
        this.#restoreDiffTreeExpansions();
        this.#diffFileTree.setActive(path);
      } else if (result === "collapsed") {
        this.#diffTreeExpanded.delete(path);
      }
      this.requestUpdate();
      return;
    }
    const index = this.#diffFiles.findIndex((file) => file.path === path);
    if (index < 0) return;
    this.#selectedDiff = index;
    if (globalThis.matchMedia?.(MOBILE_FILES_QUERY).matches === true) {
      this.#diffTreeCollapsed = true;
    }
    this.requestUpdate();
    if (this.#diffFiles[index]?.binary === true) {
      this.#clearSelectedDiff();
    } else {
      void this.#loadDiffFile(path, { force: false, silent: false });
    }
  }

  #focusDiffFileTreePath(path: string): void {
    this.requestUpdate();
    void this.updateComplete.then(() => {
      if (
        !this.isConnected ||
        this.panel !== "diff" ||
        this.#diffFileTree.activePath !== path
      ) return;
      const target = [...this.querySelectorAll<HTMLButtonElement>(".diff-file-tree-item")]
        .find((candidate) => candidate.dataset["diffFilePath"] === path);
      target?.focus();
    });
  }

  #restoreDiffTreeExpansions(): void {
    const directories = [...this.#diffTreeExpanded].sort(
      (left, right) => left.split("/").length - right.split("/").length,
    );
    for (const directory of directories) this.#diffFileTree.expand(directory);
  }

  #syncDiffFileTree(): void {
    const directories = fileTreeDirectoriesForPaths(
      this.#diffFiles.map((file) => file.path),
    );
    const nextDirectoryPaths = new Set(
      [...directories.keys()].filter((path) => path !== "."),
    );
    const expanded = this.#diffTreeInitialized
      ? new Set([
          ...[...this.#diffTreeExpanded].filter((path) => nextDirectoryPaths.has(path)),
          ...[...nextDirectoryPaths].filter((path) => !this.#diffTreeDirectories.has(path)),
        ])
      : new Set(nextDirectoryPaths);
    this.#diffFileTree.clear();
    for (const [directory, entries] of directories) {
      this.#diffFileTree.resolveDirectory(directory, entries);
    }
    this.#diffTreeDirectories = nextDirectoryPaths;
    this.#diffTreeExpanded = expanded;
    this.#diffTreeInitialized = true;
    this.#restoreDiffTreeExpansions();
    const selected = this.#diffFiles[this.#selectedDiff] ?? this.#diffFiles[0];
    if (selected !== undefined) this.#diffFileTree.setActive(selected.path);
  }

  #clearSelectedDiff(): void {
    this.#diffFileGeneration += 1;
    this.#selectedDiffFile = undefined;
    this.#selectedDiffText = "";
    this.#diffFileLoadingPath = "";
    this.#diffFileError = "";
    this.#diffFileErrorPath = "";
  }

  async #copyFileDiff(file: ParsedDiffFile): Promise<void> {
    await this.#copyClipboardText(
      file.raw,
      `${file.path} diff copied to the clipboard.`,
      file.path,
    );
  }

  async #copyFileContents(file: ProtocolFileContent): Promise<void> {
    await this.#copyClipboardText(
      file.content,
      `${file.path} copied to the clipboard.`,
      file.path,
      "The file contents could not be copied.",
      true,
    );
  }

  async #copyClipboardText(
    text: string,
    copiedMessage: string,
    copiedPath = "",
    failedMessage = "The raw diff could not be copied.",
    allowEmpty = false,
  ): Promise<void> {
    if (text === "" && !allowEmpty) return;
    const sequence = ++this.#copySequence;
    this.#clearCopyFeedbackTimer();
    this.#copyFeedback = "";
    this.#copyFeedbackIsError = false;
    this.#copiedDiffPath = "";
    this.requestUpdate();

    let clipboard: ClipboardTextWriter | undefined;
    try {
      clipboard = globalThis.navigator?.clipboard;
    } catch {
      clipboard = undefined;
    }
    const result = await copyRawDiffToClipboard(text, clipboard);
    if (sequence !== this.#copySequence || !this.isConnected) return;
    this.#copyFeedback = result === "copied"
      ? copiedMessage
      : result === "unavailable"
        ? "Clipboard access is unavailable in this context."
        : failedMessage;
    this.#copyFeedbackIsError = result !== "copied";
    this.#copiedDiffPath = result === "copied" ? copiedPath : "";
    this.requestUpdate();
    this.#copyFeedbackTimer = globalThis.setTimeout(() => {
      if (sequence !== this.#copySequence || !this.isConnected) return;
      this.#copyFeedback = "";
      this.#copyFeedbackIsError = false;
      this.#copiedDiffPath = "";
      this.#copyFeedbackTimer = undefined;
      this.requestUpdate();
    }, 2_000);
  }

  #clearCopyFeedbackTimer(): void {
    if (this.#copyFeedbackTimer === undefined) return;
    globalThis.clearTimeout(this.#copyFeedbackTimer);
    this.#copyFeedbackTimer = undefined;
  }

  #renderFiles() {
    const root = this.#fileTree.directory(".");
    const rows = this.#fileTree.rows;
    const file = this.#file;
    const markdown = file !== undefined && /\.(?:md|markdown)$/iu.test(file.path);
    const capabilities = this.#capabilities.value === undefined
      ? undefined
      : readSignal(this.#capabilities.value.current);
    const fileAction = this.#services.value?.nativeHost?.actOnSessionFile;
    const canOpen = capabilities?.openLocalFile === true && fileAction !== undefined;
    const canReveal = capabilities?.revealLocalFile === true && fileAction !== undefined;
    return html`
      <section
        class="inspection-split files-inspection ${this.#fileTreeCollapsed ? "file-tree-collapsed" : ""}"
        aria-busy=${this.#fileTree.loading || this.#fileLoadingPath !== ""
          ? "true"
          : "false"}
      >
        <header class="inspection-summary">
          <button
            type="button"
            aria-expanded=${this.#fileTreeCollapsed ? "false" : "true"}
            aria-controls="session-file-tree"
            title=${this.#fileTreeCollapsed ? "Show file tree" : "Hide file tree"}
            @click=${() => {
              this.#fileTreeCollapsed = !this.#fileTreeCollapsed;
              this.requestUpdate();
            }}
          >${fontAwesomeIcon("folder-tree")}</button>
          <strong title=${file?.path ?? "Session worktree"}>${file?.path ?? ""}</strong>
          ${file === undefined
            ? nothing
            : html`
                ${canOpen
                  ? html`<button
                      type="button"
                      aria-label="Open in editor"
                      title="Open in editor"
                      ?disabled=${this.#fileActionPending !== ""}
                      @click=${() => void this.#actOnFile(file, "open")}
                    >${fontAwesomeIcon(
                      this.#fileActionPending === "open"
                        ? "spinner"
                        : "arrow-up-right-from-square",
                      { spin: this.#fileActionPending === "open" },
                    )}</button>`
                  : nothing}
                ${canReveal
                  ? html`<button
                      class="file-additive-action"
                      type="button"
                      aria-label="Reveal in desktop folder"
                      title="Reveal in desktop folder"
                      ?disabled=${this.#fileActionPending !== ""}
                      @click=${() => void this.#actOnFile(file, "reveal")}
                    >${this.#fileActionPending === "reveal" ? "…" : "Reveal"}</button>`
                  : nothing}
                <button
                  type="button"
                  title="Copy file contents"
                  aria-label=${`Copy contents of ${file.path}`}
                  @click=${() => void this.#copyFileContents(file)}
                >${fontAwesomeIcon(
                  this.#copiedDiffPath === file.path && !this.#copyFeedbackIsError
                    ? "check"
                    : "copy",
                )}</button>
                ${markdown
                  ? html`<button
                      type="button"
                      aria-label=${this.#filePreview ? "Show source" : "Preview markdown"}
                      title=${this.#filePreview ? "Show source" : "Preview markdown"}
                      aria-pressed=${this.#filePreview ? "true" : "false"}
                      @click=${() => {
                        this.#filePreview = !this.#filePreview;
                        this.requestUpdate();
                      }}
                    >${fontAwesomeIcon("eye")}</button>`
                  : nothing}
              `}
        </header>
        <div
          id="session-file-tree"
          class="inspection-file-list file-tree"
          role="tree"
          aria-label="Session files"
          aria-busy=${this.#fileTree.loading ? "true" : "false"}
        >
          ${rows.map(
            (row) => html`
              <button
                type="button"
                role="treeitem"
                aria-level=${row.level}
                aria-posinset=${row.positionInSet}
                aria-setsize=${row.setSize}
                aria-expanded=${row.isDirectory ? String(row.expanded) : nothing}
                aria-selected=${this.#file?.path === row.path ? "true" : "false"}
                aria-busy=${row.directoryStatus === "loading" ? "true" : "false"}
                tabindex=${this.#fileTree.activePath === row.path ? 0 : -1}
                data-file-path=${row.path}
                class="file-row file-tree-item ${this.#file?.path === row.path ? "selected" : ""}"
                style=${`--file-tree-indent:${row.depth * 15}px`}
                @focus=${() => this.#fileTree.setActive(row.path)}
                @keydown=${(event: KeyboardEvent) => this.#navigateFileTree(event, row.path)}
                @click=${() => void this.#activateFileTreeRow(row.path)}
              >
                <span class="file-tree-label">
                  ${row.isDirectory
                    ? fontAwesomeIcon(row.expanded ? "caret-down" : "caret-right", {
                        className: "file-tree-disclosure",
                      })
                    : html`<span class="file-tree-disclosure"></span>`}
                  ${fontAwesomeIcon(
                    row.isDirectory ? row.expanded ? "folder-open" : "folder" : "file-lines",
                    { className: "file-tree-icon" },
                  )}
                  <span class="file-tree-name">${row.name}</span>
                </span>
              </button>
              ${this.#renderDirectoryState(row)}
            `,
          )}
          ${root.status === "unloaded"
            ? html`<p class="file-tree-state root" role="status">Files are not loaded yet.</p>`
            : root.status === "loading"
              ? html`<p class="file-tree-state root" role="status">Loading files…</p>`
              : root.status === "error"
                ? html`<p class="file-tree-state root error" role="alert">The file tree could not be loaded. Retrying automatically.</p>`
                : root.entries.length === 0
                  ? html`<p class="file-tree-state root">This session worktree is empty.</p>`
                  : nothing}
        </div>
        ${this.#fileLoadingPath !== ""
          ? html`<div class="screen-empty file-view-state" role="status"><span>Loading ${this.#fileLoadingPath}…</span></div>`
          : this.#fileError !== ""
            ? html`
                <div class="screen-empty file-view-state" role="alert">
                  <strong>Unable to open file</strong>
                  <span>${this.#fileError}</span>
                  <span>Retrying automatically.</span>
                </div>
              `
            : this.#file === undefined
              ? html`<div class="screen-empty file-view-state"><span>Select a file to view it.</span></div>`
              : this.#renderFileViewer(this.#file)}
      </section>
    `;
  }

  #renderFileViewer(file: ProtocolFileContent) {
    const markdown = /\.(?:md|markdown)$/iu.test(file.path);
    return html`
      <section class="file-view-shell" aria-label=${`File ${file.path}`}>
        ${this.#copyFeedback === ""
          ? nothing
          : html`<p
              class="file-view-notice ${this.#copyFeedbackIsError ? "error" : ""}"
              role=${this.#copyFeedbackIsError ? "alert" : "status"}
              aria-live="polite"
            >${this.#copyFeedback}</p>`}
        ${markdown && this.#filePreview
          ? html`<div class="file-markdown-preview"><trouve-markdown-view .content=${file.content}></trouve-markdown-view></div>`
          : html`<trouve-code-view
              class="inspection-widget"
              label=${file.path}
              language=${languageForPath(file.path)}
              .content=${file.content}
            ></trouve-code-view>`}
      </section>
    `;
  }

  async #actOnFile(
    file: ProtocolFileContent,
    action: "open" | "reveal",
  ): Promise<void> {
    const nativeAction = this.#services.value?.nativeHost?.actOnSessionFile;
    if (
      nativeAction === undefined
      || this.#effectiveSessionId === ""
      || this.#fileActionPending !== ""
    ) return;
    const sessionId = this.#effectiveSessionId;
    const generation = ++this.#fileActionGeneration;
    this.#fileActionPending = action;
    this.#copyFeedback = "";
    this.#copyFeedbackIsError = false;
    this.#copiedDiffPath = "";
    this.#clearCopyFeedbackTimer();
    this.requestUpdate();
    try {
      await nativeAction(sessionId, file.path, action);
      if (generation !== this.#fileActionGeneration || sessionId !== this.#effectiveSessionId) return;
      this.#copyFeedback = action === "open"
        ? "Opened the file in its default desktop application."
        : "Revealed the file in its desktop folder.";
    } catch {
      if (generation !== this.#fileActionGeneration || sessionId !== this.#effectiveSessionId) return;
      this.#copyFeedback = action === "open"
        ? "The desktop host could not open this file."
        : "The desktop host could not reveal this file.";
      this.#copyFeedbackIsError = true;
    } finally {
      if (generation === this.#fileActionGeneration && sessionId === this.#effectiveSessionId) {
        this.#fileActionPending = "";
        this.requestUpdate();
      }
    }
  }

  /** Open a typed session-relative file action from chat/tool metadata and
   * reveal its optional one-based line range. */
  async openFile(path: string, from = 0, to = from): Promise<void> {
    if (path === "") return;
    this.#fileTreeCollapsed = false;
    this.#filePreview = false;
    await this.#revealParentDirectories(path);
    if (!(await this.#loadFile(path))) return;
    await this.updateComplete;
    if (!this.isConnected || this.#file?.path !== path) return;
    const view = this.querySelector<TrouveCodeView>("trouve-code-view");
    if (view === null) return;
    const range = lineRangeOffsets(this.#file.content, from, to);
    view.revealRange(range.from, range.to);
  }

  async #revealParentDirectories(path: string): Promise<void> {
    await this.#loadRootDirectory(false);
    for (const directory of parentDirectories(path)) {
      const row = this.#fileTree.row(directory);
      if (row === undefined || !row.isDirectory) break;
      this.#fileTree.expand(directory);
      await this.#ensureDirectory(directory);
    }
    this.#fileTree.setActive(path);
    this.requestUpdate();
  }

  #renderDirectoryState(row: FileTreeRow) {
    if (!row.isDirectory || !row.expanded) return nothing;
    const directory = this.#fileTree.directory(row.path);
    const style = `--file-tree-indent:${(row.depth + 1) * 15}px`;
    if (directory.status === "loading") {
      return html`<p class="file-tree-state" style=${style} role="status">Loading ${row.name}…</p>`;
    }
    if (directory.status === "error") {
      return html`<p class="file-tree-state error" style=${style} role="alert">${row.name} could not be loaded. Retrying automatically.</p>`;
    }
    if (directory.status === "loaded" && directory.entries.length === 0) {
      return html`<p class="file-tree-state" style=${style}>Empty directory</p>`;
    }
    return nothing;
  }

  #navigateFileTree(event: KeyboardEvent, currentPath: string): void {
    if (
      event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
      event.isComposing
    ) return;
    const action = this.#fileTree.actionForKey(event.key, currentPath);
    if (action === undefined) return;
    event.preventDefault();
    event.stopPropagation();

    if (action.kind === "focus") {
      this.#fileTree.setActive(action.path);
      this.#focusFileTreePath(action.path);
      return;
    }
    if (action.kind === "expand") {
      this.#fileTree.expand(action.path);
      this.requestUpdate();
      void this.#ensureDirectory(action.path);
      this.#focusFileTreePath(action.path);
      return;
    }
    if (action.kind === "collapse") {
      this.#fileTree.collapse(action.path);
      this.#focusFileTreePath(action.path);
      return;
    }
    void this.#activateFileTreeRow(action.path);
  }

  async #activateFileTreeRow(path: string): Promise<void> {
    const row = this.#fileTree.row(path);
    if (row === undefined) return;
    this.#fileTree.setActive(path);
    if (!row.isDirectory) {
      await this.#loadFile(path);
      return;
    }

    const result = this.#fileTree.toggle(path);
    this.requestUpdate();
    if (result === "expanded") await this.#ensureDirectory(path);
  }

  #focusFileTreePath(path: string): void {
    this.requestUpdate();
    void this.updateComplete.then(() => {
      if (
        !this.isConnected ||
        this.panel !== "files" ||
        this.#fileTree.activePath !== path
      ) return;
      const target = [...this.querySelectorAll<HTMLButtonElement>(".file-tree-item")]
        .find((candidate) => candidate.dataset["filePath"] === path);
      target?.focus();
    });
  }

  async #load(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#effectiveSessionId === "") return;
    if (this.panel === "diff") {
      await this.#refreshDiff({ silent: false, force: true });
    } else {
      await this.#loadRootDirectory(false);
    }
  }

  async #refreshDiff(options: { readonly silent: boolean; readonly force?: boolean }): Promise<boolean> {
    const services = this.#services.value;
    if (services === undefined || this.#effectiveSessionId === "") return false;
    if (this.#diffRequestActive && options.force !== true) return false;
    const sessionId = this.#effectiveSessionId;
    const generation = ++this.#generation;
    let shouldRender = !options.silent;
    this.#diffRequestActive = true;
    if (!options.silent) {
      this.#notice = "";
      this.#noticeIsError = false;
      this.#error = "";
      if (this.#diffLoaded) this.#refreshing = true;
      else this.#loading = true;
      this.requestUpdate();
    }
    try {
      const response = await services.protocol.sessionDiffSummary(sessionId);
      if (generation !== this.#generation || sessionId !== this.#effectiveSessionId) return false;
      if (this.#error !== "") {
        this.#error = "";
        shouldRender = true;
      }
      const nextManifest = JSON.stringify(response.files);
      if (!this.#diffLoaded || nextManifest !== this.#diffManifest) {
        this.#selectedDiff = selectedDiffIndexAfterRefresh(
          this.#diffFiles,
          this.#selectedDiff,
          response.files,
        );
        this.#diffFiles = response.files;
        this.#syncDiffFileTree();
        this.#diffManifest = nextManifest;
        shouldRender = true;
      }
      this.#diffLoaded = true;
      const selected = this.#diffFiles[this.#selectedDiff] ?? this.#diffFiles[0];
      if (selected === undefined || selected.binary) {
        this.#clearSelectedDiff();
      } else {
        await this.#loadDiffFile(selected.path, {
          force: true,
          silent: options.silent,
        });
      }
      return true;
    } catch {
      if (generation === this.#generation && !options.silent) {
        if (this.#diffLoaded) {
          this.#notice = "The diff refresh failed. The last loaded diff is still shown.";
          this.#noticeIsError = true;
        } else {
          this.#error = "The diff request failed.";
        }
      }
      return false;
    } finally {
      if (generation === this.#generation) {
        this.#diffRequestActive = false;
        this.#loading = false;
        this.#refreshing = false;
        if (shouldRender) this.requestUpdate();
      }
    }
  }

  async #loadDiffFile(
    path: string,
    options: { readonly force: boolean; readonly silent: boolean },
  ): Promise<boolean> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (services === undefined || sessionId === "" || path === "") return false;
    if (
      options.force !== true &&
      this.#selectedDiffFile?.path === path &&
      this.#diffFileErrorPath !== path
    ) return true;

    const generation = ++this.#diffFileGeneration;
    const previousForPath = this.#selectedDiffFile?.path === path;
    const previousErrorForPath = this.#diffFileErrorPath === path;
    const hasStableState = previousForPath || previousErrorForPath;
    let shouldRender = !options.silent || !hasStableState;
    if (!options.silent || !hasStableState) {
      this.#diffFileLoadingPath = path;
      this.#diffFileError = "";
      this.#diffFileErrorPath = "";
    }
    if (shouldRender) this.requestUpdate();
    try {
      const response = await services.protocol.sessionFileDiff(sessionId, path);
      if (
        generation !== this.#diffFileGeneration ||
        sessionId !== this.#effectiveSessionId ||
        path !== (this.#diffFiles[this.#selectedDiff] ?? this.#diffFiles[0])?.path
      ) return false;
      if (previousForPath && response.diff === this.#selectedDiffText) return true;
      const parsed = (await prepareUnifiedDiffOffThread(response.diff))[0];
      if (
        generation !== this.#diffFileGeneration ||
        sessionId !== this.#effectiveSessionId ||
        path !== (this.#diffFiles[this.#selectedDiff] ?? this.#diffFiles[0])?.path
      ) return false;
      this.#selectedDiffFile = parsed === undefined
        ? undefined
        : { ...parsed, path: response.path };
      this.#selectedDiffText = response.diff;
      this.#diffFileError = "";
      this.#diffFileErrorPath = "";
      shouldRender = true;
      return true;
    } catch (error) {
      if (
        generation === this.#diffFileGeneration &&
        sessionId === this.#effectiveSessionId &&
        (!options.silent || !hasStableState)
      ) {
        this.#selectedDiffFile = undefined;
        this.#selectedDiffText = "";
        this.#diffFileError = error instanceof ProtocolClientError
          ? error.message
          : "The selected file diff request failed.";
        this.#diffFileErrorPath = path;
        shouldRender = true;
      }
      return false;
    } finally {
      if (generation === this.#diffFileGeneration) {
        this.#diffFileLoadingPath = "";
        if (shouldRender) this.requestUpdate();
      }
    }
  }

  async #loadRootDirectory(
    refresh: boolean,
  ): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#effectiveSessionId === "") return;
    const current = this.#fileTree.directory(".").status;
    if (!refresh && (current === "loading" || current === "loaded")) return;

    if (refresh) {
      this.#fileTreeGeneration += 1;
      this.#fileTree.reset(this.#fileTree.activePath ?? this.#file?.path);
    }
    const generation = this.#fileTreeGeneration;
    const sessionId = this.#effectiveSessionId;
    this.#fileTree.beginLoading(".");
    this.requestUpdate();
    try {
      const entries = await services.protocol.sessionFiles(sessionId, ".");
      if (
        generation !== this.#fileTreeGeneration ||
        sessionId !== this.#effectiveSessionId
      ) return;
      this.#fileTree.resolveDirectory(".", entries);
    } catch {
      if (
        generation === this.#fileTreeGeneration &&
        sessionId === this.#effectiveSessionId
      ) this.#fileTree.failDirectory(".");
    } finally {
      if (
        generation === this.#fileTreeGeneration &&
        sessionId === this.#effectiveSessionId
      ) {
        this.requestUpdate();
      }
    }
  }

  /** Refresh the visible Files projection without replacing loaded rows with
   * loading placeholders. Expanded directories are the only nested listings
   * the user can currently observe, so refreshing those keeps network work
   * bounded while still recovering failed listings automatically. */
  async #refreshVisibleFiles(): Promise<void> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (
      services === undefined
      || sessionId === ""
      || this.#filesRefreshActive
      || this.panel !== "files"
    ) return;

    const rootStatus = this.#fileTree.directory(".").status;
    if (rootStatus === "unloaded" || rootStatus === "error") {
      await this.#loadRootDirectory(false);
      return;
    }
    if (rootStatus === "loading") return;

    const generation = this.#fileTreeGeneration;
    const directories = [
      ".",
      ...this.#fileTree.rows
        .filter((row) => row.isDirectory && row.expanded)
        .map((row) => row.path),
    ].filter((path) => this.#fileTree.directory(path).status !== "loading");
    this.#filesRefreshActive = true;
    try {
      const results = await Promise.allSettled(
        directories.map(async (path) => ({
          path,
          entries: await services.protocol.sessionFiles(sessionId, path),
        })),
      );
      if (
        generation !== this.#fileTreeGeneration
        || sessionId !== this.#effectiveSessionId
        || this.panel !== "files"
      ) return;

      let changed = false;
      for (const result of results) {
        if (result.status !== "fulfilled") continue;
        const previous = this.#fileTree.directory(result.value.path);
        const next = [...result.value.entries].sort((left, right) => {
          if (left.is_dir !== right.is_dir) return left.is_dir ? -1 : 1;
          return left.name.localeCompare(right.name);
        });
        const same = previous.status === "loaded"
          && previous.entries.length === next.length
          && previous.entries.every((entry, index) => {
            const candidate = next[index];
            return candidate?.name === entry.name && candidate.is_dir === entry.is_dir;
          });
        if (same) continue;
        this.#fileTree.resolveDirectory(result.value.path, result.value.entries);
        changed = true;
      }
      if (changed) this.requestUpdate();

      if (this.#fileError !== "" && this.#fileTargetPath !== "") {
        await this.#loadFile(this.#fileTargetPath);
      }
    } finally {
      this.#filesRefreshActive = false;
    }
  }

  async #ensureDirectory(path: string): Promise<void> {
    const services = this.#services.value;
    if (
      services === undefined ||
      this.#effectiveSessionId === "" ||
      !this.#fileTree.needsLoad(path)
    ) return;
    const generation = this.#fileTreeGeneration;
    const sessionId = this.#effectiveSessionId;
    this.#fileTree.beginLoading(path);
    this.requestUpdate();
    try {
      const entries = await services.protocol.sessionFiles(sessionId, path);
      if (
        generation !== this.#fileTreeGeneration ||
        sessionId !== this.#effectiveSessionId
      ) return;
      this.#fileTree.resolveDirectory(path, entries);
    } catch {
      if (
        generation === this.#fileTreeGeneration &&
        sessionId === this.#effectiveSessionId
      ) this.#fileTree.failDirectory(path);
    } finally {
      if (
        generation === this.#fileTreeGeneration &&
        sessionId === this.#effectiveSessionId
      ) {
        this.requestUpdate();
      }
    }
  }

  async #loadFile(path: string): Promise<boolean> {
    const services = this.#services.value;
    if (services === undefined || this.#effectiveSessionId === "" || path === "") return false;
    const generation = ++this.#fileGeneration;
    const sessionId = this.#effectiveSessionId;
    this.#fileTargetPath = path;
    this.#fileLoadingPath = path;
    this.#fileError = "";
    this.requestUpdate();
    try {
      const file = await services.protocol.sessionFile(sessionId, path);
      if (generation === this.#fileGeneration && sessionId === this.#effectiveSessionId) {
        this.#file = file;
        this.#fileTree.setActive(path);
        this.#filePreview = false;
        if (globalThis.matchMedia?.(MOBILE_FILES_QUERY).matches === true) {
          this.#fileTreeCollapsed = true;
        }
        return true;
      }
    } catch {
      if (generation === this.#fileGeneration && sessionId === this.#effectiveSessionId) {
        this.#fileError = "The file request failed.";
      }
    } finally {
      if (generation === this.#fileGeneration && sessionId === this.#effectiveSessionId) {
        this.#fileLoadingPath = "";
        this.requestUpdate();
      }
    }
    return false;
  }
}

customElements.define("trouve-inspection-workspace", TrouveInspectionWorkspace);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-inspection-workspace": TrouveInspectionWorkspace;
  }
}
