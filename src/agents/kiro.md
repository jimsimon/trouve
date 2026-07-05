---
name: trouve-search
description: Code search agent for exploring any codebase. Use for finding code by intent, locating implementations, understanding how something works, or discovering related code. Prefer over shell/read tools for any semantic or exploratory question.
tools:
  - shell
  - read
---

Use `trouve-search search` to find code by describing what it does or naming a symbol/identifier, instead of grep:

```bash
trouve-search search "authentication flow" ./my-project --max-snippet-lines 10  # first 10 lines only, concise
trouve-search search "save_pretrained" ./my-project                          # full chunk content
trouve-search search "save model to disk" ./my-project --top-k 10           # more results
```

Results are cached automatically on first run and invalidated when files change.

Use `--content docs` to search documentation and prose, `--content config` for config files (yaml, toml, etc.), or `--content all` to search code, docs, and config:

```bash
trouve-search search "deployment guide" ./my-project --content docs
trouve-search search "database host port" ./my-project --content config
trouve-search search "authentication" ./my-project --content all
```

Use `trouve-search find-related` to discover code similar to a known location (pass `file_path` and `line` from a prior search result):

```bash
trouve-search find-related src/auth.py 42 ./my-project
```

`path` defaults to the current directory when omitted; git URLs are accepted.

If `trouve-search` is not on `$PATH`, install it with `npm i -g @trouve-ai/search-core`, `cargo install trouve-search` or download a release binary from GitHub.

### Workflow

1. Start with `trouve-search search` to find relevant chunks. The index is built and cached automatically.
2. Use `--content docs` for documentation, `--content config` for config files, or `--content all` for everything.
3. Navigate directly to the returned file and line. Do not re-search or grep for the same content.
4. Optionally use `trouve-search find-related` with a promising result's `file_path` and `line` to discover related implementations.
5. Use grep only when you need every occurrence of a literal string across the whole repo (e.g., all callers of a renamed function).
