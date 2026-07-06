# UX screen map

The shared information architecture and screen inventory for every trouve
client (Slint desktop/mobile now, web later). `trouve-client-core` view
models encode this structure once; rendering layers differ, screens don't.

## Design principles

1. **Chat-first, not IDE-first.** The primary object is the agent session,
   not the file buffer. Files, diffs, and terminals appear in service of
   reviewing what the agent did. Zed and VS Code exist; don't compete.
2. **The diff is the product.** The most important screen is "what did the
   agent change and do I accept it." Review UX outranks everything except
   the chat stream.
3. **Every surface degrades gracefully to read-only.** Mobile is a
   monitor/approve/steer surface — same screens, reduced interaction.
4. **Trust through visibility.** Tool calls stream as expandable cards
   (command, output, exit status), never spinners. Users must be able to
   audit what ran — non-negotiable given the no-OS-sandbox decision
   (ADR 0004).

## Information architecture

Four levels, consistent on every platform (mirrors the session model):

| Level | What it is | Key UI signal |
| --- | --- | --- |
| Workspace | a repo bound to a server (local or remote) | workspace switcher = root nav |
| Session | unit of work; own worktree + branch | status: running / **needs attention** / idle / done |
| Thread | parallel conversation in a session | tab strip; own mode + model + options |
| Session detail | the main screen | chat stream + inspection tabs |

Sessions are the inbox. "Needs attention" (blocked on approval) is the key
signal and the sort key of every session list.

## Screen inventory

- **S1 Session inbox** — session list across workspaces, status badges,
  branch names. Desktop: column 1. Mobile: home screen.
- **S2 Session detail** — active thread chat + thread tabs. Desktop:
  column 2. Mobile: full-screen view.
- **S3 Inspection panel** — tabs: terminal, GitHub (phase 5), diff, plan,
  files. Desktop: column 3. Mobile: reachable from session detail.
- **S4 Diff review** — session branch vs base; per-file list; unified or
  split (desktop only).
- **S5 Settings** — providers, integrations, MCP servers, skills,
  look & feel (design-token themes), agent modes, git/worktrees.
- **S6 First-run / provider onboarding** — API key entry or OAuth login
  (device code must render well on mobile: show code, open browser).
- **S7 About** — version, licenses, `AboutSlint` attribution (license
  requirement).

## Desktop layout (three columns, keyboard-driven)

```
┌───────────┬──────────────────────────────┬───────────────────────┐
│ nav       │ thread tabs  [+]             │ term │ diff │ plan │… │
│ workspace │ mode ▾  model ▾  options ▾   │                       │
│ switcher  │──────────────────────────────│   inspection tab      │
│           │ chat stream:                 │   content             │
│ session   │  · user message              │                       │
│ list      │  · assistant markdown (live) │                       │
│  ● run    │  · tool card (collapsed)     │                       │
│  ◐ needs  │  · approval prompt inline    │                       │
│  ○ idle   │    [approve] [always] [deny] │                       │
├───────────┴──────────────────────────────┴───────────────────────┤
│ status bar: model · tokens/$ · permission mode (1-click change)  │
└──────────────────────────────────────────────────────────────────┘
```

- Column 1 collapsible; command palette (Ctrl/Cmd-K) for session/thread
  switching and actions.
- Thread header renders model options dynamically from the model's
  `options_schema` (`GET /v1/models`) — no hardcoded per-model UI.
- Tool cards collapsed by default: icon, one-line summary, exit status.
  Expand for full command/output. Approval prompts are keyboard-first
  (y / a / n).
- Status bar always shows the permission mode; YOLO renders in warning
  color everywhere it appears.

## Mobile layout (stack navigation, monitor-first)

- **Home** = S1 sorted by needs-attention; pull to refresh; push
  notification on approval blocks (mobile phase).
- **Session view** = S2 full-screen; tool cards tap-to-expand; approval
  prompts as bottom sheets with large approve/deny targets.
- **Diff review** = S4 as per-file list → single-file unified diff (no
  side-by-side on narrow screens). Read and approve only.
- Composer: text + quick-reply chips ("continue", "explain", "undo").

## Mobile-first discipline (applies to desktop now)

Every screen composes from stackable panels — the three desktop columns are
three panels that collapse to a stack. Touch-target sizing, no hover-only
affordances, needs-attention inbox as the home concept. This is what makes
the mobile phase a layout adaptation, not a redesign.

## Key workflows

1. **New session**: workspace → prompt → run (mode/model/permissions
   optional; defaults make it two actions). Worktree + branch created
   automatically.
2. **New thread**: one action from an open session (tab "+" / palette);
   inherits worktree, picks its own mode/model. Canonical flow: plan thread
   → code thread → review thread on one branch.
3. **Approval loop**: prompt inline in chat and as a notification; show
   exactly what will run; "always allow" is the ask → allow-list migration
   path; resolving from any client updates all (SSE).
4. **Diff review & apply**: turn/session ends → review S4 → accept or
   revert (checkpoint undo/redo backs this).
5. **PR flow (phase 5)**: session branch → PR with generated description →
   full lifecycle in the GitHub tab; PR comments flow back into a thread.
6. **Provider onboarding**: S6 on first run and from settings.

## Component patterns (shared vocabulary)

- **Tool card**: collapsed = icon + summary + status chip; expanded =
  args, streamed output, exit code, duration. Denied/failed states visually
  distinct.
- **Approval prompt**: renders the allow-list key it would create (e.g.
  `shell:cargo`), three actions, keyboard/tap parity.
- **Status chips**: one component for session state, turn state, tool
  state, CI state (phase 5) — consistent colors.
- **Markdown stream**: renders incrementally from `assistant.delta` events;
  code blocks use the same highlight tokens as the file viewer.
