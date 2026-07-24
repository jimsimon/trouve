---
name: publish-release
description: Prepare and publish trouve releases end to end by selecting SemVer, synchronizing every version-bearing artifact, writing release notes, validating the workspace, opening and merging a release pull request after its checks pass, and creating and verifying the GitHub tag and release. Use when preparing, validating, or publishing a trouve release, changing the product version, adding or changing a version-bearing artifact, or editing release automation.
---

# Publish a Trouve Release

Treat root `[workspace.package].version` as the sole manually edited product
version. Follow ADR 0012 and keep every first-party artifact in lockstep.

For an actual release request, complete every phase through the published
GitHub release. The request authorizes the release branch, commit, push, pull
request, ordinary merge, tag, and release operations described here. It does
not authorize bypassing branch protection, moving an existing tag, or
deleting or recreating a release.

For preparation-only work, release validation, version-bearing artifact
changes, or release-automation maintenance that is not an actual release,
apply the relevant update and verification sections, then stop before
publishing through a pull request.

## Preflight

1. Read `docs/adr/0012-single-version-monorepo-release-train.md` and inspect
   `.github/workflows/release.yml`.
2. Inspect the worktree status, remotes, current branch, root workspace
   version, changelog, and GitHub authentication. Determine the repository's
   default branch with `gh`, then fetch that branch and remote tags.
3. Preserve unrelated user changes. Publish only intended release changes
   from a dedicated branch based on the current remote default branch. If
   unrelated changes cannot be isolated without rewriting or discarding user
   work, stop and ask for direction.
4. Confirm the authenticated identity can push a branch, open and merge a
   pull request, and create repository tags and releases. Do not request
   broader scopes than the workflow needs.

## Establish the release

1. Validate a remote tag before treating it as a release baseline: resolve
   its commit and read `[workspace.package].version` from that commit. The tag
   must equal `v<workspace-version>`. Ignore and report stale or pre-created
   tags that do not match, then select the newest valid tag.
2. Inspect the complete commit range since that valid baseline. Choose SemVer
   from the most severe user-visible change anywhere in the repository.
   Explain a non-obvious major or minor bump.
3. Set the release tag to `v<next-version>` and the release date to the
   current date. Before editing, check both the remote tag namespace and
   GitHub Releases for that tag.
4. If the tag or release already exists, resolve all associated commits and
   versions. Resume only when every existing object exactly matches the
   intended release; otherwise stop and require explicit authorization before
   moving, deleting, or recreating anything.

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

## Write useful release notes

- Describe user-visible features, behavior changes, fixes, security changes,
  operational changes, and required migration steps.
- Do not list mechanical version synchronization or routine dependency/action
  updates unless they materially affect users or operators.
- Keep the release section concise enough to use directly as the GitHub
  Release body while covering every notable change in the release range.

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

Run all of the following before opening the release pull request:

```bash
python3 -m unittest scripts/test_sync_versions.py
python3 scripts/sync_versions.py --check
cargo metadata --no-deps --locked --offline --format-version 1
```

Then run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test --workspace`, plus relevant Node tests, type checks, and builds.
Confirm release automation compares `vX.Y.Z` with the root workspace version
and does not reintroduce a component-specific version source.

## Publish through a pull request

1. Review the complete diff and `git diff --check`. Confirm it contains only
   the intended synchronized release state and release notes.
2. Commit the release as `Prepare <version> release`. Record the exact release
   branch head SHA, push the branch, and open a non-draft pull request against
   the repository's default branch. Put the release summary and completed
   verification in the pull request body.
3. Record the pull request number and URL. Watch every pull request check, not
   only required checks, until all checks finish successfully. Use
   `gh pr checks <pr> --watch --fail-fast` or an equivalent monitor that
   yields status updates at least once per minute.
4. If a check fails, inspect its logs, fix the cause on the same release
   branch, rerun relevant local verification, push the fix, update the
   recorded head SHA, and watch the new checks. Do not merge while any check
   is failing, pending, cancelled, or missing.
5. Immediately before merging, confirm the pull request is open, non-draft,
   mergeable, targets the expected default branch, and still has the recorded
   head SHA. Respect required reviews and merge queues. Never use an admin
   bypass.
6. Merge with the repository's established squash strategy and protect
   against a changed head:

   ```bash
   gh pr merge "$release_pr" --squash \
     --match-head-commit "$release_branch_head_sha"
   ```

7. If GitHub queues the merge, continue monitoring until the pull request
   state is `MERGED`. Read the actual merge commit from
   `gh pr view <pr> --json mergeCommit`; do not infer it from the local
   branch.
8. Fetch the remote default branch. Confirm the merge commit is reachable
   from it, and read `Cargo.toml` at that exact commit to verify the workspace
   version is the release version.

## Create the tag and GitHub release

1. Recheck the remote `vX.Y.Z` tag and GitHub release immediately before
   creation. If either appeared concurrently, apply the existing-object rules
   from **Establish the release**.
2. Create a lightweight `vX.Y.Z` tag at the verified pull request merge
   commit and push that exact tag ref. Never tag a local branch name, an
   unmerged release commit, or a later default-branch tip:

   ```bash
   git tag "$release_tag" "$verified_merge_sha"
   git push origin "refs/tags/$release_tag"
   ```

3. Confirm the remote tag resolves to the verified merge commit and that
   `[workspace.package].version` at the tagged commit equals `X.Y.Z`.
4. Create the GitHub release for the already-pushed tag with
   `gh release create "$release_tag" --verify-tag --title "$release_title"
   --notes-file "$release_notes_file" --fail-on-no-commits`. Use the dated
   changelog heading as the release title and the complete section as its
   body; do not substitute mechanically generated notes for the curated
   release notes.
5. Locate the tag-triggered `Release` workflow run by workflow name, tag, and
   merge SHA. Watch it to completion with exit-status checking, yielding a
   user-visible update at least once per minute. A successful run is required
   because it builds release assets and publishes the crate, npm packages,
   and containers.
6. Verify the GitHub release is published, non-draft, non-prerelease, targets
   the expected tag, contains `SHA256SUMS` and the expected binary assets, and
   retains the curated release notes. Record the release and workflow URLs.

If the tag-triggered workflow fails, inspect the failed job logs and report
which artifacts may already have published. Do not move the tag or
delete/recreate the release automatically: registry versions can be
immutable, and recovery may require a new patch release or an explicitly
authorized workflow repair.

## Finish with copy-ready Markdown

At the end of every successful release-preparation or publication response,
report the pull request, merge commit, tag, GitHub release, workflow result,
and verification summary as applicable. Then include a fenced `markdown`
block containing the complete new release section. Put this block last in the
response.

- Use a standalone heading such as `## 3.1.0 — 2026-07-23`.
- Include every changelog subsection and item for that release.
- Omit the changelog document preamble, older releases, and comparison-link
  definitions.
- Keep verification summaries and other handoff notes outside the block.
