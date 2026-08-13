# ADR 0029: Short session branch names by default

Status: Accepted (2026-08)

## Context

ADR 0017 coupled the generated session title to the session's Git branch so
both names were available before the worktree was created. The title remains
useful navigation metadata, but prompt-derived branch slugs are often long,
noisy, and needlessly expose prompt text in local branches, remotes, pull
requests, logs, and shell output. A session already has a random stable
identifier that can provide a concise branch suffix without depending on
title-generation latency or quality.

Some users still prefer descriptive branches derived from session names. That
preference is a session-naming policy, not a general Git or worktree control,
and changing it must not rename existing worktrees or branches.

## Decision

- A newly created session uses `trouve/<short-id>` by default, where
  `<short-id>` is the first six characters of the session identifier after its
  `se_` prefix.
- A persisted, server-owned session-naming option may re-enable descriptive
  branches. When enabled, new sessions use
  `trouve/<session-title-slug>-<short-id>`.
- The option affects future sessions only. Trouve never renames an existing
  session branch or worktree when the setting changes.
- Session titles remain server-owned display metadata generated before
  session creation with the model and deterministic fallback defined by ADR
  0017. Branch creation no longer requires the title to form the default
  branch name.
- The option is additive in the versioned protocol. An absent value defaults
  to short branches when reading older persisted data. An older client that
  updates the remaining naming settings without sending the new field
  preserves the server's current value.
- The settings UI groups title generation and branch naming under
  **Sessions & Chat**. The empty **Git & Worktrees** settings section is
  removed; the existing protocol route and type names remain as compatibility
  details until a separately versioned migration is justified.

This decision supersedes ADR 0017. It retains ADR 0017's server-owned title
model and fallback lifecycle while replacing the requirement that every
branch derive from that title.

## Consequences

- Default branch names are short, predictable, and do not reveal session or
  prompt text. The session title remains visible in application navigation and
  other session metadata.
- Users who value descriptive Git history can opt into the former title-based
  shape without sacrificing the random suffix that distinguishes same-titled
  sessions.
- Existing sessions, remotes, pull requests, and worktrees remain untouched,
  so changing the preference is safe and non-disruptive.
- The six-character identifier has the same random suffix length previously
  appended to title-derived branches. A repository collision still fails
  through the normal branch/worktree creation path rather than silently
  selecting a different branch.
- Protocol consumers may continue to encounter the historical
  `GitWorktreeSettings` and `/v1/config/git-worktrees` names even though the
  product surface now presents them as session-naming settings.

## Alternatives rejected

- **Always derive branches from session titles.** This preserves descriptive
  names but also preserves long branches and unnecessary prompt disclosure.
- **Use the full session identifier.** It reduces collision risk further but
  defeats the requested short, human-friendly branch form.
- **Rename branches when the setting or title changes.** This would disrupt
  worktrees, remotes, pull requests, and user tooling.
- **Let each client choose the branch form.** That would make naming behavior
  client-dependent and bypass the server-owned session policy.
