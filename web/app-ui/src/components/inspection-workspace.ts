import { ContextConsumer } from "@lit/context";
import { html, LitElement, nothing, type PropertyValues } from "lit";

import {
  appServicesContext,
  hostCapabilitiesContext,
  sessionContext,
} from "../contexts/app-contexts.js";
import type {
  ProtocolFileContent,
  ProtocolRelativeRestoreDirection,
} from "../services/protocol-client.js";
import { prepareUnifiedDiffOffThread } from "../services/content-worker-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  selectedDiffIndexAfterRefresh,
  type ParsedDiffFile,
} from "./diff-parser.js";
import type { DiffMode } from "./diff-mode.js";
import {
  checkpointAvailabilityDescription,
  checkpointHintsAfterRestore,
  copyRawDiffToClipboard,
  initialCheckpointHints,
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
  #restorePending: ProtocolRelativeRestoreDirection | "" = "";
  #notice = "";
  #noticeIsError = false;
  #copyFeedback = "";
  #copyFeedbackIsError = false;
  #copiedDiffPath = "";
  #copySequence = 0;
  #copyFeedbackTimer: ReturnType<typeof setTimeout> | undefined;
  #checkpointHints = initialCheckpointHints();
  #diffRefreshTimer: ReturnType<typeof setInterval> | undefined;
  #error = "";
  #diffText = "";
  #diffFiles: readonly ParsedDiffFile[] = [];
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
        this.#restorePending !== "" ||
        (typeof document !== "undefined" && document.visibilityState === "hidden")
      ) return;
      void this.#refreshDiff({ silent: true });
    }, DIFF_REFRESH_MS);
  }

  protected override willUpdate(changed: PropertyValues<this>): void {
    const effectiveSessionId = this.#effectiveSessionId;
    this.#sessionScopeChanged =
      changed.has("sessionId") || effectiveSessionId !== this.#observedSessionId;
    if (this.#sessionScopeChanged) this.#observedSessionId = effectiveSessionId;
    if (changed.has("panel")) {
      this.#generation += 1;
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
      this.#fileTreeGeneration += 1;
      this.#fileGeneration += 1;
      this.#fileActionGeneration += 1;
      this.#copySequence += 1;
      this.#loading = false;
      this.#diffRequestActive = false;
      this.#diffLoaded = false;
      this.#refreshing = false;
      this.#restorePending = "";
      this.#notice = "";
      this.#noticeIsError = false;
      this.#copyFeedback = "";
      this.#copyFeedbackIsError = false;
      this.#copiedDiffPath = "";
      this.#clearCopyFeedbackTimer();
      this.#checkpointHints = initialCheckpointHints();
      this.#error = "";
      this.#diffText = "";
      this.#diffFiles = [];
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
    this.#fileTreeGeneration += 1;
    this.#fileGeneration += 1;
    this.#fileActionGeneration += 1;
    this.#copySequence += 1;
    this.#clearCopyFeedbackTimer();
    if (this.#diffRefreshTimer !== undefined) {
      globalThis.clearInterval(this.#diffRefreshTimer);
      this.#diffRefreshTimer = undefined;
    }
    super.disconnectedCallback();
  }

  override render() {
    if (this.panel === "diff" && this.#loading) {
      return html`<div class="screen-empty" role="status"><span>Loading ${this.panel}…</span></div>`;
    }
    if (this.panel === "diff" && this.#error !== "") {
      return html`<div class="screen-empty" role="alert"><strong>Unable to load ${this.panel}</strong><span>${this.#error}</span><button type="button" @click=${() => this.#load()}>Retry</button></div>`;
    }
    return this.panel === "diff" ? this.#renderDiff() : this.#renderFiles();
  }

  #renderDiff() {
    const additions = this.#diffFiles.reduce((total, file) => total + file.additions, 0);
    const deletions = this.#diffFiles.reduce((total, file) => total + file.deletions, 0);
    const selectedFile = this.#diffFiles[this.#selectedDiff] ?? this.#diffFiles[0];
    const diffFilesByPath = new Map(this.#diffFiles.map((file) => [file.path, file]));
    return html`
      <section
        class=${`inspection-split diff-inspection ${
          this.#diffTreeCollapsed ? "diff-tree-collapsed" : ""
        } ${this.#diffFiles.length === 0 ? "diff-empty-inspection" : ""}`}
        aria-busy=${this.#refreshing || this.#restorePending !== "" ? "true" : "false"}
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
          <span class="checkpoint-actions" role="group" aria-label="Diff actions">
            <button
              type="button"
              aria-label=${`Undo turn. ${checkpointAvailabilityDescription(
                "undo",
                this.#checkpointHints.undo,
              )}`}
              title=${checkpointAvailabilityDescription(
                "undo",
                this.#checkpointHints.undo,
              )}
              ?disabled=${this.#loading || this.#refreshing || this.#restorePending !== ""}
              @click=${() => void this.#restoreCheckpoint("undo")}
            >${this.#restorePending === "undo"
              ? "Undoing…"
              : html`${fontAwesomeIcon("rotate-left")} Undo turn`}</button>
            <button
              type="button"
              aria-label=${`Redo. ${checkpointAvailabilityDescription(
                "redo",
                this.#checkpointHints.redo,
              )}`}
              title=${checkpointAvailabilityDescription(
                "redo",
                this.#checkpointHints.redo,
              )}
              ?disabled=${this.#loading || this.#refreshing || this.#restorePending !== ""}
              @click=${() => void this.#restoreCheckpoint("redo")}
            >${this.#restorePending === "redo"
              ? "Redoing…"
              : html`${fontAwesomeIcon("rotate-right")} Redo`}</button>
            ${this.#diffText === ""
              ? nothing
              : html`<button
                  type="button"
                  aria-label="Copy complete diff"
                  title="Copy complete diff"
                  @click=${() => void this.#copyRawDiff()}
                >${fontAwesomeIcon("copy")} copy diff</button>`}
            ${selectedFile === undefined
              ? nothing
              : html`<button
                  type="button"
                  aria-label=${`Copy raw diff for ${selectedFile.path}`}
                  title=${`Copy raw diff for ${selectedFile.path}`}
                  @click=${() => void this.#copyFileDiff(selectedFile)}
                >${fontAwesomeIcon(
                  this.#copiedDiffPath === selectedFile.path && !this.#copyFeedbackIsError
                    ? "check"
                    : "copy",
                )}</button>`}
            <button
              class="diff-refresh-action"
              type="button"
              aria-label="Refresh diff"
              title="Refresh diff"
              ?disabled=${this.#loading || this.#refreshing || this.#restorePending !== ""}
              @click=${() => void this.#refreshDiff({ silent: false })}
            >${fontAwesomeIcon(this.#refreshing ? "spinner" : "arrows-rotate", {
              spin: this.#refreshing,
            })}</button>
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
                : this.#renderSelectedDiff(selectedFile)}
            `}
      </section>
    `;
  }

  #renderSelectedDiff(file: ParsedDiffFile) {
    if (file.binary) {
      return html`<div class="screen-empty diff-view-state" role="status"><span>Binary file changed.</span></div>`;
    }
    return html`
      <trouve-diff-view
        class="inspection-widget diff-view-shell"
        .original=${file.original}
        .modified=${file.modified}
        .originalLineNumbers=${file.originalLineNumbers}
        .modifiedLineNumbers=${file.modifiedLineNumbers}
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

  async #copyRawDiff(): Promise<void> {
    await this.#copyClipboardText(
      this.#diffText,
      "Raw diff copied to the clipboard.",
    );
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
          <button
            class="files-refresh-action"
            type="button"
            aria-label="Refresh files"
            ?disabled=${root.status === "loading"}
            @click=${() => void this.#loadRootDirectory(true)}
          >${fontAwesomeIcon("arrows-rotate")}</button>
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
                ? html`
                    <div class="file-tree-state root error" role="alert">
                      <span>The file tree could not be loaded.</span>
                      <button
                        type="button"
                        data-file-tree-retry="."
                        @click=${() => void this.#loadRootDirectory(false, true)}
                      >Retry</button>
                    </div>
                  `
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
                  <button
                    type="button"
                    ?disabled=${this.#fileTargetPath === ""}
                    @click=${() => void this.#loadFile(this.#fileTargetPath)}
                  >Retry</button>
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
      return html`
        <div class="file-tree-state error" style=${style} role="alert">
          <span>${row.name} could not be loaded.</span>
          <button
            type="button"
            data-file-tree-retry=${row.path}
            @click=${() => void this.#ensureDirectory(row.path, true)}
          >Retry</button>
        </div>
      `;
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

  #recoverFileTreeFocus(retryPath: string): void {
    this.requestUpdate();
    void this.updateComplete.then(() => {
      if (!this.isConnected || this.panel !== "files") return;
      const active = this.#fileTree.activePath;
      const rows = [...this.querySelectorAll<HTMLButtonElement>(".file-tree-item")];
      const activeRow = active === undefined
        ? undefined
        : rows.find((candidate) => candidate.dataset["filePath"] === active);
      if (activeRow !== undefined) {
        activeRow.focus();
        return;
      }
      [...this.querySelectorAll<HTMLButtonElement>("[data-file-tree-retry]")]
        .find((candidate) => candidate.dataset["fileTreeRetry"] === retryPath)
        ?.focus();
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
      const response = await services.protocol.sessionDiff(sessionId);
      if (generation !== this.#generation || sessionId !== this.#effectiveSessionId) return false;
      if (!this.#diffLoaded || response.diff !== this.#diffText) {
        const nextFiles = await prepareUnifiedDiffOffThread(response.diff);
        if (generation !== this.#generation || sessionId !== this.#effectiveSessionId) return false;
        this.#selectedDiff = selectedDiffIndexAfterRefresh(
          this.#diffFiles,
          this.#selectedDiff,
          nextFiles,
        );
        this.#diffFiles = nextFiles;
        this.#syncDiffFileTree();
        this.#diffText = response.diff;
        shouldRender = true;
      }
      this.#diffLoaded = true;
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

  async #restoreCheckpoint(direction: ProtocolRelativeRestoreDirection): Promise<void> {
    const services = this.#services.value;
    if (
      services === undefined ||
      this.#effectiveSessionId === "" ||
      this.#restorePending !== ""
    ) return;
    const sessionId = this.#effectiveSessionId;
    this.#generation += 1;
    this.#diffRequestActive = false;
    this.#loading = false;
    this.#refreshing = false;
    this.#restorePending = direction;
    this.#notice = "";
    this.#noticeIsError = false;
    this.requestUpdate();
    try {
      await services.protocol.restoreSessionCheckpoint(sessionId, direction);
      if (
        sessionId !== this.#effectiveSessionId ||
        this.panel !== "diff" ||
        !this.isConnected
      ) return;
      this.#checkpointHints = checkpointHintsAfterRestore(
        this.#checkpointHints,
        direction,
      );
      const refreshed = await this.#refreshDiff({ silent: false, force: true });
      if (refreshed && sessionId === this.#effectiveSessionId && this.isConnected) {
        this.#notice = direction === "undo"
          ? "Restored the previous checkpoint. Redo is now available."
          : "Restored the next checkpoint. Undo is now available.";
        this.#noticeIsError = false;
      }
    } catch {
      if (sessionId === this.#effectiveSessionId && this.isConnected) {
        this.#notice = direction === "undo"
          ? "Undo was not completed. Availability could not be determined; retry after checking the connection and checkpoint history."
          : "Redo was not completed. Availability could not be determined; retry after checking the connection and checkpoint history.";
        this.#noticeIsError = true;
      }
    } finally {
      if (sessionId === this.#effectiveSessionId) {
        this.#restorePending = "";
        this.requestUpdate();
      }
    }
  }

  async #loadRootDirectory(
    refresh: boolean,
    recoverFocus = false,
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
        if (recoverFocus) this.#recoverFileTreeFocus(".");
      }
    }
  }

  async #ensureDirectory(path: string, recoverFocus = false): Promise<void> {
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
        if (recoverFocus) this.#recoverFileTreeFocus(path);
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
