---
name: create-trouve-crate
description: Create or rename Cargo crates and Node packages in the trouve workspace while enforcing the trouve- Cargo prefix and @trouve-ai/ npm scope. Use whenever adding, scaffolding, splitting, extracting, or renaming a Rust crate, Cargo workspace member, Node package, private web app, package.json, or package-lock.json in this repository.
---

# Create a Trouve Package

Keep every Cargo and Node package visibly owned by trouve and update all references as one change.

## Name Cargo crates

- Name every new Cargo package `trouve-<purpose>`.
- Name its directory `crates/trouve-<purpose>` so the directory matches the package.
- Keep `trouve-app` as the existing main application package. Do not use the app exception to create another unprefixed package.
- Let Rust normalize package hyphens to underscores in source imports: package `trouve-example` becomes crate path `trouve_example`.
- Reject names that could imply first-party ownership by another project, including names beginning with `slint-`.

## Name Node packages

- Name every Node package `@trouve-ai/<purpose>`, including private applications that are not published to npm.
- Keep the package scope in the root `name` and `packages[""].name` fields of a colocated `package-lock.json`.
- Keep npm workspace package entries in shared lockfiles under the `@trouve-ai/` scope.
- Do not rename Docker or GHCR images solely to match the npm scope; container image identifiers are not Node package names.

## Update the workspace

1. Create or rename the package in the appropriate workspace directory.
2. Set Cargo `[package].name` or Node `package.json.name` to the required prefixed or scoped name.
3. Update path dependencies, workspace dependencies, feature references, package-selection commands, imports, examples, documentation, and lockfile entries.
4. Search exhaustively for the old package name, old directory path, and old underscore-normalized Rust crate name when renaming.
5. Preserve the architectural boundaries in `AGENTS.md`; a naming change must not introduce trouve-specific public types into generic widget crates.

## Verify the invariant

Run the bundled check after refreshing Cargo metadata and Node lockfiles:

```bash
python3 .agents/skills/create-trouve-crate/scripts/check_crate_names.py
```

Then run the relevant package build plus the repository-required formatting, tests, and clippy checks from `AGENTS.md`. Do not finish while an unprefixed Cargo crate, unscoped Node package, or stale local lockfile package name remains.
