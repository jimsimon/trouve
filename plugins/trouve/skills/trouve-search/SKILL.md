---
name: trouve-search
description: Search a codebase by meaning with trouve. Use when looking for where something is implemented, how a feature works, or code related to a known location — instead of grep for exploratory or semantic questions.
---

# Trouve Code Search

The trouve MCP server (bundled with this plugin) provides two tools:

- `search` — search a codebase with a natural-language or code query.
- `find_related` — find code similar to a specific file and line.

Use `search` to find where something is implemented — instead of grepping to
discover files. After trouve returns the file and line, navigate there
directly and read that file. Do not grep for the same content again.

## Workflow

1. Call `search` with a query describing what the code does or its name
   (function/class names or behaviour descriptions, not error messages).
   Pass the project root as `repo`; https:// git URLs also work. Results
   include 10 lines of context each — signature plus first body lines,
   enough to confirm the location.
2. Navigate directly to the top result's file and line. Read only the
   function or class at that location.
3. Make the edit. Do not re-search or grep for the same content.
4. Optionally call `find_related` with `file_path` and `line` from a search
   result to discover similar code elsewhere (implementations of an
   interface, callers, tests).
5. Grep only when you need every occurrence of a literal string across the
   whole repo (e.g., all callers of a renamed function).

The index is warmed in the background at session start where the harness
supports startup hooks (and built on first use otherwise); it is cached,
and updates are incremental and shared across branches and worktrees.

## CLI fallback

Without MCP access, the `trouve` CLI provides the same search:

```bash
trouve search "authentication flow" ./my-project --max-snippet-lines 10
trouve search "deployment guide" ./my-project --content docs
trouve search "database host port" ./my-project --content config
trouve find-related src/auth.py 42 ./my-project
```

`--content` selects what to search: `code` (default), `docs`, `config`, or
`all`.

## Requirements

The `trouve` binary must be on PATH. Install with `cargo install trouve` or
download a release binary from https://github.com/jimsimon/trouve/releases.
