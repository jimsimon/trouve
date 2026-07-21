---
name: create-trouve-crate
description: Create or rename Cargo crates in the trouve workspace while enforcing the trouve- package and directory prefix. Use whenever adding, scaffolding, splitting, extracting, or renaming a Rust crate or Cargo workspace member in this repository.
---

# Create a Trouve Crate

Keep every workspace crate visibly owned by trouve and update all references as one change.

## Name the crate

- Name every new Cargo package `trouve-<purpose>`.
- Name its directory `crates/trouve-<purpose>` so the directory matches the package.
- Keep `trouve-app` as the existing main application package. Do not use the app exception to create another unprefixed package.
- Let Rust normalize package hyphens to underscores in source imports: package `trouve-example` becomes crate path `trouve_example`.
- Reject names that could imply first-party ownership by another project, including names beginning with `slint-`.

## Update the workspace

1. Create or rename the crate under `crates/`.
2. Set `[package].name` to the prefixed name.
3. Update path dependencies, workspace dependencies, feature references, package-selection commands, examples, documentation, and lockfile entries.
4. Search exhaustively for the old package name, old directory path, and old underscore-normalized Rust crate name when renaming.
5. Preserve the architectural boundaries in `AGENTS.md`; a naming change must not introduce trouve-specific public types into generic widget crates.

## Verify the invariant

Run the bundled check after Cargo has refreshed workspace metadata:

```bash
python3 .agents/skills/create-trouve-crate/scripts/check_crate_names.py
```

Then run the repository-required formatting, tests, and clippy checks from `AGENTS.md`. Do not finish while an unprefixed non-app package remains in Cargo metadata.
