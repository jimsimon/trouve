# Account-centric, multi-instance GitHub integration

Status: Accepted (2026-07)

## Context

The pull-request dashboard was derived from registered workspaces and accepted
credentials from environment variables, pasted tokens, OAuth, and the `gh`
keyring. That made the visible feed repository-centric, duplicated requests for
duplicate remotes, omitted relevant repositories without local workspaces, and
made the active identity difficult to reason about.

## Decision

GitHub OAuth device flow is the sole credential boundary. Users may configure
multiple GitHub instances, each with its own OAuth application and session.
The server queries each authenticated account for PRs the user authored, was
requested to review, or was involved in through comments or mentions. It emits
one persisted snapshot per GitHub instance. Clients aggregate those snapshots
and may associate repositories with local workspaces for navigation.

## Consequences

The dashboard includes relevant PRs even when their repository is not a local
workspace, and one refresh covers every configured instance. Instance failures
can be represented independently, and repository identity must include its
host. Existing environment, PAT, and `gh` credentials are intentionally ignored;
users sign in once per instance. This replaces the workspace-scoped dashboard
route and event, requiring a protocol major-version bump.

## Alternatives rejected

Retaining workspace fan-out would preserve local-project filtering but cannot
discover relevant PRs outside registered repositories. Combining credential
sources was rejected because precedence rules obscure which account is active.
