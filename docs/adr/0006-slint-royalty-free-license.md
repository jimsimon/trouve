# ADR 0006: Slint under the Royalty-Free license

Status: Accepted (2026-07)

## Context

Slint is triple-licensed: GPLv3, a paid commercial license, and the **Slint
Royalty-Free Desktop, Mobile, and Web Applications License**. GPLv3 would
force the Slint-linked client binaries to be GPLv3 (fine for open source,
constrains future re-licensing and closed distribution). The Royalty-Free
license permits free commercial distribution of desktop/mobile/web
applications with two obligations: attribution ("Built with Slint" /
AboutSlint) and no re-licensing of Slint itself. It does not cover embedded
devices, which we don't target.

## Decision

Use Slint under the Royalty-Free license for all trouve native clients and
the `slint-*` widget crates.

- `trouve-app` shows the `AboutSlint` attribution in its About screen.
- Our own code stays MIT (workspace default): the widget crates and app are
  MIT-licensed; users who *depend* on them accept Slint's license terms for
  the Slint dependency itself, which each crate's README states.
- If requirements change (e.g. embedded targets), options are GPLv3 or a
  commercial license; switching is a dependency-license change, not a code
  change.

## Consequences

- No copyleft obligations on trouve's own code; future licensing stays
  flexible.
- Attribution requirement is a UI checkbox-level task and is tracked in the
  desktop phase.
- The widget crates remain usable by both GPLv3 and Royalty-Free Slint
  users since MIT is compatible with both.
