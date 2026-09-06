# ADR 0048: Configured asynchronous session naming

Status: Accepted (2026-09)

## Context

Session and thread names were derived before creation by a dedicated built-in
local model with rule-based fallbacks. That model was separate from the
provider/model catalog users already configure for agent work, required its
own install and resource lifecycle, and could not consume attachments. The
fallback rules frequently selected incidental prompt text, especially when
the prompt referred to an attached screenshot.

Waiting for naming also put cosmetic work on the creation path. At the same
time, the optional title-derived branch policy appeared to require the final
title before worktree creation, even though Git permits the checked-out branch
to be renamed later.

## Decision

- Session naming uses a provider-qualified model id from the same configured
  model catalog as every other Model picker. There is no dedicated built-in
  naming model, installer, resource policy, or heuristic fallback. Until the
  user explicitly selects a naming model, the configured default model is the
  effective naming model when one exists.
- A new session is created as `New Session` and a new thread as `New Thread`.
  Creation and the first message do not wait for naming. The client starts a
  bounded background request after the entity exists and retains the
  placeholder when naming is unavailable or fails.
- The naming request includes the owning session id, the first prompt, and its
  image attachments when the selected model advertises image input support.
  Direct providers receive their native multimodal user message. Subscription
  and CLI backends run a tool-free, read-only turn in the session worktree with
  the same attachments. Model pickers identify text-only models rather than
  silently implying that they can inspect screenshots.
- The naming system prompt is a product-owned contract: it preserves the
  user's language and exact technical identifiers, treats attachments as
  evidence, and requires only a short title. It is not user-editable. Naming
  automatically selects the model's lowest advertised reasoning level, or
  disables reasoning when the model supports that choice.
- Model output must satisfy only short-title shape and length constraints.
  There is no semantic scoring heuristic; invalid output is an error and the
  server does not invent a replacement. Clients update session and thread
  titles with an expected-placeholder compare-and-set so a late result cannot
  overwrite a user rename.
- The manual session and thread rename dialogs offer a Generate action. Unlike
  initial naming, this recovery path derives its context from the current
  conversation: a session suggestion spans all its threads, while a thread
  suggestion stays scoped to that thread. User messages, steering, and final
  assistant outcomes are included; tool output, progress, and reasoning are
  excluded. The selected naming model performs title generation in one pass;
  Trouve does not spend a second model call creating an intermediate summary.
  The suggestion only fills the rename field and remains subject to user
  confirmation.
- The branch is initially the compact session branch. When
  `derive_branch_name_from_session_title` is enabled and a session title
  update wins, the server renames the checked-out local branch through
  `ToolExecutor`, verifies that the title is still current, and persists the
  new branch name. Existing remote branches are not renamed.
- Naming configuration is durable server state and is projected through
  `settings.session_naming_updated`.

## Consequences

Session and thread creation is immediate and no longer depends on a local
model download. Naming quality and attachment support follow the model the
user selected, including any cost, latency, and provider availability of that
model. While naming is active, clients may present a transient loading state;
failed cosmetic requests reveal the explicit durable placeholder instead of a
misleading heuristic title.

Title-derived branch names arrive shortly after creation rather than being
known up front. Code that observes session metadata must therefore already
handle the normal `session.updated` transition for both title and branch.
The branch rename remains auditable and permission-confined at the tool
execution boundary.
