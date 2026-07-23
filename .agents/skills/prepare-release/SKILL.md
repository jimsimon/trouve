---
name: prepare-release
description: Prepare trouve releases end to end by selecting SemVer from changes since the latest tag, updating the canonical workspace version, synchronizing Cargo, Node, plugin, lockfile, and container references, promoting release notes and deployment examples, auditing version references, validating the workspace, and returning a GitHub-ready Markdown changelog. Use when preparing, validating, or publishing a trouve release, changing the product version, adding or changing a version-bearing artifact, or editing release automation.
---

# Prepare a Trouve Release

Treat root `[workspace.package].version` as the sole manually edited product
version. Follow ADR 0012 and keep every first-party artifact in lockstep.

## Establish the release

1. Read `docs/adr/0012-single-version-monorepo-release-train.md`.
2. Inspect the worktree status, root workspace version, latest repository
   `vX.Y.Z` tag, changelog, and commits since that tag. Preserve unrelated
   user changes.
3. Choose SemVer from the most severe user-visible change anywhere in the
   repository. Explain a non-obvious major or minor bump.

## Update the release

1. Edit only `[workspace.package].version` in root `Cargo.toml`.
2. Run:

   ```bash
   python3 scripts/sync_versions.py
   ```

   Let the script update Node packages, plugin manifests, local lock records,
   internal dependency pins, root workspace dependency pins, and Cargo.lock.
   Do not hand-maintain copies that the script owns.
3. Add or promote the dated release section in `CHANGELOG.md`. Derive it from
   the complete commit range, organize notable changes under Keep a Changelog
   headings, and preserve historical entries and links unchanged. Include a
   comparison link for the new `vX.Y.Z` tag.
4. Update current-version deployment and installation examples.
5. Search exhaustively for the previous current version and obsolete
   component-specific tag prefixes. Classify every match before editing it;
   retain historical releases, fixtures, and independent compatibility or
   dependency versions.

Do not commit, tag, push, publish, or deploy unless the user explicitly asks.
When authorized, use the single repository tag `vX.Y.Z`.

## Write useful release notes

- Describe user-visible features, behavior changes, fixes, security changes,
  operational changes, and required migration steps.
- Do not list mechanical version synchronization or routine dependency/action
  updates unless they materially affect users or operators.
- Keep the release section concise enough to paste directly into a GitHub
  Release page while covering every notable change in the release range.

## Add or update a versioned artifact

- Give every Cargo workspace member `version.workspace = true`.
- Put first-party path dependencies in `[workspace.dependencies]` with the
  root version and consume them with `.workspace = true`.
- Give every `@trouve-ai/*` package a version, including private npm workspace
  roots and web apps.
- Add any new plugin or package manifest format to automatic discovery or an
  explicit check in `scripts/sync_versions.py`.
- Use the `create-trouve-crate` skill as well when creating or renaming a
  Cargo crate or Node package.

## Preserve independent compatibility numbers

Do not replace wire protocol, OpenAPI, database schema, cache/snapshot/file
format, MCP protocol, vendor CLI, fixture, Rust toolchain, GitHub Action, or
third-party dependency versions. They do not identify trouve releases.

## Verify

Run all of the following before finishing:

```bash
python3 -m unittest scripts/test_sync_versions.py
python3 scripts/sync_versions.py --check
cargo metadata --no-deps --locked --offline --format-version 1
```

Then run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test --workspace`, plus relevant Node tests, type checks, and builds.
Confirm release automation compares `vX.Y.Z` with the root workspace version
and does not reintroduce a component-specific version source.

## Finish with copy-ready Markdown

At the end of every successful release-preparation response, include a fenced
`markdown` block containing the complete new release section, ready to paste
into GitHub. Put this block last in the response.

- Use a standalone heading such as `## 3.1.0 — 2026-07-23`.
- Include every changelog subsection and item for that release.
- Omit the changelog document preamble, older releases, and comparison-link
  definitions.
- Keep verification summaries and other handoff notes outside the block.
