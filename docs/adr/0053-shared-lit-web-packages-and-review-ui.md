# ADR 0053: Shared Lit web packages and review UI

Status: Accepted (2026-09), amended by ADR 0054 (the shared review UI is drawn
from the desktop design system rather than its own stylesheet).

Supersedes ADR 0013 (the review dashboard is no longer a Preact application)
and amends ADR 0011 (the review web UI is no longer a separately implemented
SPA).

## Context

ADR 0011 made automated code review a separately deployed server with its own
database and a standalone Preact web UI (`review-ui`). The desktop client
later grew a second, Lit-based code-review dashboard embedded under the
pull-requests screen and pointed at the desktop's own embedded server. That
left two implementations of the same screens, drifting in features (the
desktop copy never rendered reviewer progress, reasoning, or tool output the
way the web UI did) and doubling maintenance for every protocol change.

The two frontends also shared nothing at the source level. `web/app-ui` held
the protocol client, design tokens and themes, Markdown and diff rendering,
and the chat transcript as private modules, so `review-ui` could not use them
without copying, and every improvement to how the desktop renders a
conversation stayed on the desktop.

## Decision

- The web frontend is one npm workspace under `web/` with `apps/app-ui`
  (desktop and PWA shell), `apps/review-ui` (standalone review shell), and
  layered shared packages: `@trouve-ai/ui-foundation`, `@trouve-ai/protocol`,
  `@trouve-ai/content-rendering`, `@trouve-ai/transcript`, and
  `@trouve-ai/code-review`. All of them are Lit and TypeScript. Nothing in
  `web/` uses Preact any more.
- The shared packages are extracted from `app-ui` rather than written anew:
  the desktop imports them through their `exports` maps and keeps its
  behaviour, bundle budgets, and tests. Packages are consumed as TypeScript
  source; there is no publish step.
- `@trouve-ai/code-review` owns every review screen as Lit elements that
  render into light DOM against the review stylesheet, plus a review API
  client parameterized by base URL. The port preserves the existing layout,
  styling, and behaviour; it is a framework change, not a redesign.
- `review-ui` is a thin shell: it mounts the shared root element, syncs the
  hash route, and serves the same-origin API exactly as before. Its Docker
  image builds from the workspace and copies only the packages the shell
  depends on; a lint check keeps that list in step with the manifests.
- How the desktop app surfaces a remote review server is out of scope for
  this decision. The desktop's embedded code-review dashboard is untouched
  here; its future is a separate product decision.

## Consequences

Every review screen is implemented once, and the modules both frontends need
live in exactly one place: a transcript, Markdown, diff, or design-token fix
is a fix in both products. Shared packages carry their own tests and
import-boundary checks, so `app-ui` cannot quietly reach back into a
package's private files. The review site gains the desktop's rendering
quality for free where it adopts the shared packages (ADR 0054 does so for
the design system and task transcripts).

The cost is workspace plumbing: one lockfile, per-package `tsconfig`s, and
version numbers that `scripts/sync_versions.py` keeps aligned with the Cargo
workspace.

## Alternatives rejected

Rewriting the review screens inside `app-ui` only would have discarded the
working web UI and left `review-ui` on Preact. Keeping `review-ui` on Preact
and sharing only framework-neutral modules would have left the transcript
and every Lit element unshareable, which is most of what the review UI needs.
Publishing the shared packages to a registry would add a release step for
code that has exactly two consumers in one repository.
