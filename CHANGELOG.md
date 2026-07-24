# Changelog

All notable changes to this project are documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [3.3.1] - 2026-07-24

### Fixed

- **Reliable pull request comment review requests**: the review service now
  polls trusted issue comments when webhook delivery is unavailable, persists
  comment claims so commands cannot trigger duplicate reviews, and revalidates
  recurring GitHub reads through a bounded ETag cache.

## [3.3.0] - 2026-07-24

### Added

- **Pull request comment review requests**: repository owners, members, and
  collaborators can comment `@trouve-ai review` on a pull request to request
  an on-demand review in either manual or automatic mode, including while the
  pull request is a draft. Comment triggers are persisted and deduplicated
  across webhook retries and service restarts.

### Fixed

- **Remote Codex and Claude Code sign-in**: Codex now uses device
  authentication, while Claude Code runs its subscription login in a PTY and
  accepts a validated browser callback pasted through the review dashboard.
  Both vendor CLI login flows now work when the browser and trouve server do
  not share the same localhost.

## [3.2.0] - 2026-07-23

### Added

- **Managed subscription CLIs in code review**: the hosted review dashboard
  can install, update, cancel, retry, and remove trouve-managed vendor CLIs,
  so Claude, Codex, and Cursor subscription sign-in no longer depends on a
  binary already being available on `PATH`. Direct Codex provider login also
  resolves the managed binary.
- **Review model and thinking defaults**: the dashboard now exposes the
  system-wide review model and thinking defaults and persists model and
  thinking defaults for every reviewer persona, including built-ins. Durable
  jobs snapshot persona thinking settings so queued and webhook-triggered
  reviews use the configuration selected when they were created.

### Changed

- **Faster, hardened release builds**: app, server, and search artifacts now
  compile together per target; server images reuse the static musl artifacts;
  release caches are shared with trusted main builds; and platform npm
  packages publish concurrently. Release workflows pin reviewed actions,
  avoid persisted checkout credentials, and restrict cache writes to main.

## [3.1.0] - 2026-07-23

### Added

- **Downloadable application and server binaries**: GitHub releases now ship
  prebuilt `trouve` desktop application and `trouve-server` archives for
  supported Linux, macOS, and Windows targets alongside the existing
  `trouve-search` assets and SHA-256 checksums.

### Changed

- **Review dashboard setup**: provider settings now show credential state,
  guide subscription CLI sign-in, and offer presets for API providers.
  Repository policies are easier to manage with search, mode filters,
  pagination, collapsible details, and clearer per-reviewer overrides.
- **Pull request merge readiness**: session PR icons use GitHub's detailed
  merge state and semantic colors, distinguishing merge-ready pull requests
  from open pull requests that are blocked, behind, or still being evaluated.
- **Review deployment access**: the single-user review dashboard and `/v1`
  API no longer use a shared bearer token. Keep them on a trusted private
  network or VPN, or add authentication and TLS at the reverse proxy before
  exposing them; GitHub webhooks and internal provider bridges retain their
  dedicated authentication.
- **Container publishing**: AMD64 and ARM64 images are built on matching
  native runners before being joined under the existing multi-platform
  version and commit tags.

### Fixed

- **Queued prompt previews**: multiline queued prompts now render a clipped
  one-line teaser without bleeding into the surrounding chat, while the full
  prompt remains available when editing the queue entry.

### Security

- **Review dashboard rendering**: server-provided labels, identifiers,
  messages, and repository data are escaped before rendering, and external
  review links are limited to safe HTTP(S) URLs.

## [3.0.0] - 2026-07-22

This is the first release of the trouve AI coding harness and its GitHub
App-backed code review service, deployable on your own infrastructure. It
grows trouve from a code-search tool into a protocol-first agent platform with
a native desktop client, while keeping `trouve-search` available as the same
standalone CLI, library, MCP server, and agent plugin. The major version also
establishes one lockstep version for every first-party artifact and includes
the breaking removal of remote git URL indexing.

### Added

- **trouve AI coding harness**: a Rust agent engine, HTTP + SSE server, shared
  client layer, and native Slint desktop app. Sessions own isolated git
  worktrees; threads share the session worktree while retaining durable
  per-thread conversations, queues, modes, models, and todo state. Per-turn
  hidden-ref checkpoints provide session undo and redo. The desktop app embeds
  the server in-process but continues to use the authenticated loopback
  protocol boundary.
- **Agent and model integrations**: run Claude Code, Codex, and Cursor through
  their native protocols with managed CLI installs, login flows, live model
  discovery, and persistent or resumable vendor sessions. Direct API
  providers and OpenAI-compatible endpoints are supported alongside managed
  local `llama.cpp` models, with mid-thread model changes, configurable
  thinking, context and fast-mode controls, and subscription-health views for
  Claude, Codex, Cursor, and Kimi.
- **Coding tools and delegation**: agents can read and edit files, apply
  patches, inspect diffs, search code and transcripts, glob, fetch web pages,
  run foreground or background shell jobs, maintain todos, and recover from
  compacted context. Parent agents can delegate work to child threads or
  fully isolated child sessions, then collect their output. Side effects pass
  through Ask, Allow list, or Yolo permission gates; local execution is not
  OS-sandboxed, and Yolo deliberately skips approval prompts.
- **Native desktop workflow**: streaming chat and reasoning, Markdown tables
  and syntax highlighting, file and diff inspection, an interactive PTY
  terminal, file and image attachments, `@` file mentions, `/skill`
  completion, editable queued prompts, desktop notifications, workspace
  reordering, and restored window, session, and scroll state. Modes, model
  defaults, permission policies, providers, integrations, MCP servers, and
  vendor CLIs are configurable in Settings.
- **Automations**: schedule scoped agent prompts, start them on demand, pause
  or resume them, choose their model and permission mode, and create common
  workflows from built-in templates. Runs create normal durable sessions and
  record their outcomes.
- **GitHub pull request workspace**: OAuth sign-in for GitHub.com and
  self-hosted GitHub Enterprise instances, an account-wide PR dashboard with
  actionable review/check/merge groups, project filters, session association,
  and PR actions. Shared GraphQL-backed snapshots refresh every 30 seconds and
  feed the dashboard, session status, and per-session PR panel without
  repeatedly fetching unchanged details.
- **GitHub App code review service**: a separately authenticated GitHub App can
  review selected repositories in manual or automatic mode. Signed webhooks
  provide a fast path while durable polling reconciles missed events; every
  job is deduplicated, runs read-only in an isolated session at the exact PR
  head, and is cancelled or marked stale when the revision or effective
  policy changes.
- **Focused, verified reviews**: built-in reviewer profiles cover
  correctness, security, reliability, performance, concurrency, API
  compatibility, data integrity, testing, maintainability, dependencies,
  accessibility, and operations. Repositories can select reviewers and
  override their prompts or models, while reusable custom profiles add
  project-specific expertise. A final editor pass verifies findings against
  the repository and commentable diff lines before publishing inline comments
  and a summary under the App's bot identity.
- **Code review operations**: a standalone web dashboard configures the
  GitHub App, providers, models, reviewers, and repository policies and shows
  durable job history and GitHub rate limits. Docker Compose deployment,
  backup and upgrade guidance, and multi-architecture `trouve-server` and
  `trouve-review-ui` images are included.
- **Shared search daemon**: on Unix, concurrent `trouve-search` MCP sessions
  with matching configuration now share one background embedding model and
  in-memory index cache. The daemon starts on demand, exits after 15 idle
  minutes, and falls back to in-process serving if it cannot be reached;
  `TROUVE_DAEMON=0` opts out. Windows keeps the existing in-process behavior.
- **Offline and reconnect handling**: the server reports internet reachability
  and filters remote models while offline, leaving local models available.
  The desktop app gates unavailable actions, explains connectivity state,
  reconnects and resynchronizes automatically, and announces recovery.
- **Reusable Slint widgets**: independently usable `trouve-slint-*` crates
  provide code, diff, streaming Markdown, and terminal views without exposing
  trouve protocol types in their public APIs.
- **Global default permissions**: Settings → Modes & Models gains a "Global
  default permissions" picker (Ask / Allow list / Yolo) that applies to new
  threads whose mode doesn't set its own permission mode. Per-mode default
  permissions now default to "Global default" and can still be overridden
  per mode in the mode editor; existing modes that already set an explicit
  permission default keep that behavior, while new modes (or modes without an
  explicit default) inherit the global setting. Server side: a mode's
  `default_permission_mode` is now optional (absent = global default), the
  global value persists in `config.toml` and is settable via
  `PUT /v1/config/default-permission-mode`, and `GET /v1/providers` reports
  it alongside the default model.

### Changed

- **Cargo workspace and release tags**: the repository is now a monorepo for
  `trouve-search`, the harness, and reusable UI crates. All Cargo crates, Node
  packages, plugins, internal package pins, lockfile records, containers, and
  release artifacts now share root `[workspace.package].version`; repository
  releases use `vX.Y.Z` tags. The workspace uses Rust edition 2024 and requires
  Rust 1.92 to build.
- **GitHub authentication**: account PR discovery now uses OAuth exclusively
  and unifies data from GitHub.com and configured Enterprise instances. The
  review service deliberately uses separate GitHub App installation tokens,
  so its repository access and rate limits remain isolated from desktop OAuth.

### Fixed

- **Concurrent event ingestion**: event-log appends are batched through a
  dedicated writer thread, preserving commit-before-publish and cursor order
  while preventing high-volume streaming from overflowing vendor-agent event
  routes across concurrent sessions.
- **Agent turn reliability**: fixed Codex approval responses, approvals that
  arrive before their tool card, waiter cleanup after app-server exits, Git
  writes in mutable modes, completed reasoning summaries, and subscription
  limit reporting. Tool activity and reasoning now remain visible without
  duplicate or retired replay events.
- **Desktop state and input handling**: stabilized session switching and chat
  scroll restoration, kept prompt drafts, queues, and todos scoped to their
  thread, preserved queued editor text during stream updates, fixed deferred
  quit and opener cleanup, and made session activity indicators consistent.
- **Wayland image paste**: clipboard images copied by Spectacle and similar
  tools are accepted when they are exposed through Wayland's data-control
  protocol.
- **Screen artifacts in the desktop app**: the app now prefers Slint's Skia
  renderer over the default FemtoVG renderer, whose glyph atlas corrupts on
  some Linux drivers — flashing garbage across the window while typing or
  whenever a repaint hits (e.g. a desktop notification appearing). If Skia
  can't initialize, the app falls back to the previous renderer, and an
  explicit `SLINT_BACKEND` still overrides the choice.

### Removed

- **Remote git URL support**: trouve no longer clones repositories on the
  user's behalf. The CLI, MCP server, and library (`TrouveIndex::from_git`,
  the `clone_cache` module) reject or omit git URLs; clone the repository
  yourself and pass the local directory path. The `<cache>/clones` directory,
  its eviction logic, and the `TROUVE_CLONE_TTL` / `TROUVE_CLONE_TIMEOUT`
  (and deprecated `SEMBLE_CLONE_TIMEOUT`) environment variables are gone.
  Local indexing is unaffected.

## [2.0.0] - 2026-07-05

Major bump: the crate, CLI binary, and npm packages are renamed, which breaks
existing installs and MCP configurations pointing at the `trouve` binary or
the `trouve-plugin` npm package. See [INSTALL.md](INSTALL.md) to migrate.

### Changed

- **Rename for the `@trouve-ai` npm org**: the crates.io crate and CLI binary
  are now `trouve-search` (the bare `trouve` name is reserved for future
  products). **`@trouve-ai/search-core`** ships the native binary and MCP
  launcher, with per-platform binaries installed via `@trouve-ai/search-*`
  optional dependencies — `npm i -g @trouve-ai/search-core` needs no separate
  install step, and MCP configs default to `npx -y @trouve-ai/search-core`.
  **`@trouve-ai/search-plugin`** replaces `trouve-plugin` and now carries the
  whole plugin surface: the OpenCode/Kilo native tools plus the Claude Code
  and Codex bundles (MCP config, workflow skill, sub-agent, session hook)
  formerly in `plugins/trouve` — one directory (`npm/search-plugin`), two
  install channels (npm registry and git marketplace).
- `npm/` is now an npm workspace (`search-core` + `search-plugin`, one shared
  lockfile), and `@trouve-ai/search-core` is plain ESM JavaScript with a type
  declaration, so it runs under Node 18+ (`npx`) and Bun alike.
- Dependency updates: hf-hub 0.5, tokenizers 0.23, safetensors 0.8.

### Added

- [NAME.md](NAME.md): where the name *trouve* comes from and how to pronounce
  it.

## [1.1.0] - 2026-07-04

### Added

- **Persistent clone cache**: remote git URLs are cloned once into
  `<cache>/clones` and refreshed with a cheap `git fetch` at most once per
  freshness window (`TROUVE_CLONE_TTL` seconds, default 300) instead of
  re-cloned per query. Clones are lock-protected against concurrent trouve
  processes, evicted after a week idle, and removed by `trouve clear index`.
  A stale clone is served (with a warning) when the remote is unreachable.
- The MCP server now re-validates git URLs after the same cooldown as local
  paths — a revalidation is now a TTL-gated fetch plus an incremental
  rebuild, not a re-clone.
- **`.trouveignore` files**: exclude files from indexing without git-ignoring
  them, replacing upstream's `.sembleignore` (same syntax, same per-directory
  inheritance; `.trouveignore` wins where patterns conflict).
- **`.semble/` directories** are now skipped during walks, alongside
  `.trouve/`, matching upstream's default ignore list.
- **Native OpenCode custom tools**: a standalone custom-tool file
  (`src/agents/opencode-tool.ts`, copied to
  `~/.config/opencode/tools/trouve.ts`) exposes `trouve_search` and
  `trouve_find_related` as native OpenCode tools. An alternative to an MCP
  entry: it avoids the MCP transport, needs no JSON config edits, and
  defaults `repo` to the session worktree.
- **[INSTALL.md](INSTALL.md)**: step-by-step manual setup for every
  integration route — plugins, the OpenCode native tool file, and MCP
  server entries (config file, key, and snippet for 14 agents), plus
  optional `trouve-search` sub-agent files.
- **23 new tree-sitter grammars** (~50 languages total): CMake, D, Dart, Elm,
  ERB/EJS embedded templates, Erlang, Fortran, Gleam, GraphQL, Groovy,
  HCL/Terraform, Julia, Make, Nix, Objective-C, Perl, PowerShell, Protocol
  Buffers, R, Solidity, SQL, Svelte, and XML (incl. DTD). Files in these
  languages now get syntax-aware chunk boundaries instead of the line-based
  fallback.
- **Unified agent plugin** (`plugins/trouve`): one package serving four
  harnesses. As the npm package `trouve-plugin` it exposes `trouve_search`
  and `trouve_find_related` as native tools in OpenCode and Kilo Code,
  backed by a single persistent `trouve` server process per session
  (preserving the in-process index cache, including for remote git URLs).
  The same directory carries the Claude Code plugin bundle (MCP server +
  `trouve-search` sub-agent + workflow skill, installed via the marketplace
  catalog at `.claude-plugin/marketplace.json`) and the Codex plugin bundle
  (MCP server + skill, via `.agents/plugins/marketplace.json`). All
  manifests pass their official validators and ship at the crate version.
- **Session-start index warming**: the OpenCode/Kilo plugin builds or
  refreshes the project index in the background when it loads and
  (throttled) on every `session.idle` event, so the first search never pays
  the index build and later searches absorb the agent's own edits
  (`"warm": false` disables). The Claude Code bundle ships an equivalent
  `SessionStart` hook running `trouve stats` in the background.
- **Version sync tooling**: `scripts/sync_versions.py` keeps every published
  artifact (npm plugin packages, Claude Code and Codex plugin manifests) on
  the exact crate version from `Cargo.toml`, and lint CI fails when anything
  drifts (`--check`). The release workflow refuses tags that don't match the
  crate version and publishes all npm plugin packages at the crate version
  alongside the crates.io publish (skipped until `NPM_TOKEN` is configured).
- **Model-backed end-to-end tests**: `TROUVE_E2E=1 cargo test -- --ignored`
  (already documented in the README and run by CI) now actually runs a small
  e2e suite against the real default model — cold index, semantic and
  identifier queries, `find_related`, and a warm rebuild that recomputes
  nothing.

### Changed

- MSRV raised from 1.87 to 1.89 (std file locking for the clone cache).

### Fixed

- **`.trouveignore` now works in git repositories**: ignore rules were only
  consulted by the directory walker (non-git roots); git repositories build
  their manifest from `git ls-files`/`git status` and skipped them entirely.
  Rules are now applied on top of the git listing — before any hashing I/O —
  for tracked and untracked files alike.
- **MCP protocol violations**: tool failures were returned with
  `isError: false` (clients treated them as successful output) and a
  malformed request with an id but no method got no response at all, hanging
  the client; failures now set `isError: true` and malformed requests get a
  `-32600 Invalid Request` error. `top_k: 0` is rejected as the schema
  advertises, and `max_snippet_lines: null` now means the documented default
  instead of being an undocumented full-chunk escape hatch.
- **Git manifest correctness**: tracked symlinks were keyed by the blob OID
  of the link *target path* while indexing read the target file's content,
  serving stale chunks whenever the target changed; symlinks are now skipped
  like the walker already did. Merge-conflicted (unmerged) paths are treated
  as dirty and indexed from the working tree instead of an arbitrary
  conflict stage.
- **Snapshot compatibility checks**: snapshots now record the store format
  version and chunk length, and the incremental patch path rejects
  mismatches instead of silently splicing rows chunked under different rules
  (snapshot format bumped to v4; old snapshots are rebuilt automatically).
  `save()` also verifies a pre-existing snapshot file's embedded hash and
  rewrites partial or foreign files instead of trusting them forever.
- **Model loading robustness**: corrupt or mismatched model artifacts
  (out-of-range mapping entries, undersized embedding tables, token-id gaps)
  are rejected with a clear error at load time instead of panicking
  mid-index, and a tokenizer failure on one text now embeds it as the zero
  vector with a one-time warning instead of aborting the whole build.
- **Accurate cache statistics**: `files_from_store` no longer counts rows
  spliced zero-copy from a previous snapshot (reported separately as
  `files_from_snapshot`), and `trouve stats` now emits the documented
  `cache_hit_rate`.

### Removed

- **`trouve install` / `trouve uninstall`**: the interactive installer is
  gone. Every integration it configured is now documented as a manual (and
  easily reversible) step in [INSTALL.md](INSTALL.md): plugins for
  OpenCode/Kilo/Claude Code/Codex, the OpenCode native tool file, one MCP
  config entry per agent, and optional sub-agent files. Editing user
  configs programmatically was the installer's main risk (JSONC files had
  to be skipped, TOML edits could clobber user changes); a documented
  one-line config entry per agent is simpler and safer.

### Deprecated

- **`.sembleignore` files**: still honoured, but log a warning and will be
  removed in a future release. Rename to `.trouveignore`.
- **`SEMBLE_CACHE_LOCATION`, `SEMBLE_MODEL_NAME`, `SEMBLE_CLONE_TIMEOUT`**:
  now honoured as fallbacks when the `TROUVE_*` equivalent is unset, but log
  a warning and will be removed in a future release. Use
  `TROUVE_CACHE_LOCATION`, `TROUVE_MODEL_NAME`, and `TROUVE_CLONE_TIMEOUT`.

## [1.0.0] - 2026-07-03

First stable release. trouve is a Rust port of
[MinishLab/semble](https://github.com/MinishLab/semble) — fast, accurate code
search for agents — rebuilt around an incremental, branch- and worktree-aware
index. Retrieval behaviour matches upstream (mean NDCG@10 within 0.0002 on the
upstream annotated benchmark, identical chunk boundaries and BM25 scores).

### Added

- **Faithful retrieval port**: tree-sitter chunking for ~28 languages with
  line-based fallback, `potion-code-16M` model2vec embeddings, BM25 (Lucene
  variant) with identifier tokenization and path enrichment, RRF hybrid
  fusion, and upstream's code-tuned reranking heuristics.
- **Content-addressed chunk store**: per-file artifacts (chunks, embedding
  rows, BM25 token lists) keyed by content hash — git blob OIDs for clean
  files (no file reads), BLAKE3 for dirty/untracked files. Editing one file
  re-embeds one file.
- **Branch- and worktree-aware caching**: one store per repository (keyed by
  the git common directory), shared across all branches and worktrees.
- **Memory-mapped snapshots**: warm queries load embeddings and BM25 postings
  zero-copy; incremental builds patch the newest snapshot so cost is
  proportional to the edit, not the repository.
- **Bounded cache**: snapshot pruning (4 newest kept) plus a daily
  mark-and-sweep GC that deletes store entries unreferenced by any kept
  snapshot, with a one-hour grace period for concurrent builds.
- **In-house model2vec engine**: memory-mapped embedding table, byte-level
  WordPiece fast path with a sharded word memo for ASCII text, bit-identical
  output to `model2vec-rs` per text. Embeddings are batch-independent (no
  `[PAD]` pooling).
- **Fully parallel pipeline**: hashing, parsing, chunking, tokenizing,
  embedding, and BM25 construction run across all cores via rayon, with flat
  token arenas to minimise allocation traffic.
- **CLI**: `search`, `find-related`, `stats`, `savings`, `clear`, `install`,
  `uninstall`; bare `trouve` starts an MCP stdio server with `search` and
  `find_related` tools and an in-process LRU index cache.
- **Agent installer**: MCP server config, instruction blocks, and dedicated
  sub-agents across 14 coding agents (Claude Code, Cursor, Codex, Gemini,
  OpenCode, and more).
- **Test and parity suite**: offline unit/integration tests against a
  deterministic toy model, embedding parity tests against `model2vec-rs`,
  property tests, and a parity harness verifying chunk boundaries, tokens,
  BM25 scores, and search results against the upstream Python implementation.
- **Release automation**: binaries for Linux (glibc and static musl, x64 and
  arm64), macOS (x64 and arm64), and Windows (x64 and arm64) built from
  semantic tags, with SHA-256 checksums.

### Performance

Measured on kubernetes/kubernetes (30,563 tracked files) vs upstream Python
semble ([BENCHMARKS.md](BENCHMARKS.md)):

- Cold index + query: 3.3 s vs ~3 min (54x)
- Incremental reindex (1 file touched): 0.86 s vs ~3 min (212x)
- Warm query: 0.55 s vs 7.2 s (13x)

[3.3.1]: https://github.com/jimsimon/trouve/compare/v3.3.0...v3.3.1
[3.3.0]: https://github.com/jimsimon/trouve/compare/v3.2.0...v3.3.0
[3.2.0]: https://github.com/jimsimon/trouve/compare/v3.1.0...v3.2.0
[3.1.0]: https://github.com/jimsimon/trouve/releases/tag/v3.1.0
[3.0.0]: https://github.com/jimsimon/trouve/releases/tag/v3.0.0
[2.0.0]: https://github.com/jimsimon/trouve/releases/tag/v2.0.0
[1.1.0]: https://github.com/jimsimon/trouve/releases/tag/v1.1.0
[1.0.0]: https://github.com/jimsimon/trouve/releases/tag/v1.0.0
