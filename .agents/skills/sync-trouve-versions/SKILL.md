---
name: sync-trouve-versions
description: Maintain trouve's single-version release train across Cargo crates, Node packages, lockfiles, agent plugin manifests, internal dependency pins, container images, changelogs, and release tags. Use whenever bumping or checking a version, preparing or publishing a release, adding or changing a Cargo/Node package or versioned artifact, editing release automation, or changing version synchronization tooling.
---

# Sync Trouve Versions

Treat root `[workspace.package].version` as the sole manually edited product
version. Follow ADR 0012 and keep every first-party artifact in lockstep.

## Change the release version

1. Read `docs/adr/0012-single-version-monorepo-release-train.md` and inspect
   the existing release range before choosing SemVer. Use the most severe
   user-visible change anywhere in the repository.
2. Edit only `[workspace.package].version` in root `Cargo.toml`.
3. Run:

   ```bash
   python3 scripts/sync_versions.py
   ```

   Let the script update Node packages, plugin manifests, local lock records,
   internal dependency pins, and Cargo.lock. Do not hand-maintain copies that
   the script owns.
4. Promote the changelog entry, update current-version deployment examples,
   and use the repository tag `vX.Y.Z`. Keep historical release entries and
   links unchanged.
5. Search exhaustively for the previous current version and obsolete tag
   prefixes. Classify every match before editing it.

## Add a versioned artifact

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

Then run the repository-required formatting, clippy, and tests appropriate to
the change. Confirm release automation compares `vX.Y.Z` with the root
workspace version and does not reintroduce a component-specific source.
