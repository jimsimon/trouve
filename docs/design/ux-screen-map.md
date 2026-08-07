# UX screen map

The shared information architecture and screen inventory for every trouve
client. The Lit/Wry desktop is the staged product default; Slint remains the
rollback path and visual baseline while qualification and soak evidence is
collected. The Servo-first embedder remains a qualification preview; the same
Lit frontend supplies the initial mobile PWA. `trouve-client-core` and protocol
fixtures define shared semantics while rendering layers adapt layout without
redesigning the experience.

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
5. **Migration is not a redesign.** The Lit frontend preserves the Slint
   frontend's themes, semantic colors, typography, density, layout,
   information hierarchy, and core interactions. Ordinary web control chrome
   may vary where the same intent and Trouve styling remain recognizable.

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
- **S3 Inspection panel** — tabs: terminal, diff, plan, files, Pull request.
  The Pull request tab covers the session branch's PR status and available
  lifecycle actions. Desktop: column 3. Mobile: reachable from session detail.
- **S4 Diff review** — session branch vs base; per-file list; unified or
  split (desktop only).
- **S5 Settings** — `/settings/<section>` shell with Appearance, Workspaces,
  Providers, Vendor CLIs, Local models, Modes, Git & worktrees, MCP,
  Integrations, Notifications, Capabilities, and About. Appearance retains the
  existing design-token themes and visual preview; capability-dependent
  sections report unavailable operations instead of implying support.
- **S6 First-run / provider onboarding** — API key entry or OAuth login
  (device code must render well on mobile: show code, open browser).
- **S7 About** — `/settings/about`; frontend, server, protocol, deployment,
  connectivity, version, and licenses. Retain `AboutSlint` attribution while
  any distributed artifact contains Slint.
- **S8 Code-review dashboard** — `/reviews`; App health, recent review jobs,
  execution limits, GitHub App configuration, repository review policy and
  routing, and built-in/custom reviewer administration.
- **S9 Automations** — `/automations`; list, create, edit, run, and delete
  server-scheduled prompts that execute in fresh sessions, including template,
  workspace, schedule, mode, model, and permission configuration.

## Desktop layout (three columns, keyboard-driven)

```text
┌───────────┬──────────────────────────────┬───────────────────────┐
│ nav       │ thread tabs  [+]             │term│diff│plan│file│PR │
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
- Sending while a turn runs queues the prompt (persisted server-side, so a
  crash/restart keeps it). A panel above the composer lists queued prompts
  in run order with edit / reorder (drag or ↑/↓) / delete; the queue drains
  automatically between turns, including on sessions that aren't currently
  open. Queues never auto-run at startup — a crash may have cut the
  in-flight turn short, so continuing on top of half-finished work is the
  user's call — and a failed turn pauses its queue (a persistent error
  can't burn every prompt). The "Send now" pill resumes either case.
- Status bar always shows the permission mode; YOLO renders in warning
  color everywhere it appears.

## Mobile PWA layout (stack navigation, monitor-first)

- **Home** = S1 sorted by needs-attention; pull to refresh; push notification
  on approval blocks where the installed PWA and deployment advertise that
  capability.
- **Session view** = S2 full-screen; tool cards tap-to-expand; approval
  prompts as bottom sheets with large approve/deny targets.
- **Diff review** = S4 as per-file list → single-file unified diff (no
  side-by-side on narrow screens). Read and approve only.
- Composer: text + quick-reply chips ("continue", "explain", "undo").
- **Code review, automations, and settings** = S8, S9, and S5 as full-screen
  routes using the same responsive panels and controls as desktop.

## Mobile-first discipline (applies to desktop now)

Every screen composes from stackable panels — the three desktop columns are
three panels that collapse to a stack. Touch-target sizing, no hover-only
affordances, needs-attention inbox as the home concept. This is what makes
the mobile PWA a layout adaptation, not a redesign. Themes, semantic colors,
typography, component language, and status hierarchy remain shared with the
desktop frontend.

The PWA is the initial mobile solution. Native or embedded mobile alternatives
will be evaluated later using measured adoption, workflow, platform, and
capability evidence; they are not a prerequisite for the initial delivery.

## Lit functional port and remaining gates

The Lit application now implements the named screens and the existing Slint
`AppWindow` callback contract across the exact-nightly Servo embedder, the
default system-webview host, and the responsive PWA. The desktop hosts
expose the versioned typed capability boundary for preferences, pickers,
clipboard, validated local-file and HTTPS opening, notifications, attention,
sleep, and window/lifecycle state. The PWA uses browser capability adapters
and retains explicit fallbacks or explanations where the browser cannot
provide the equivalent operation.

This functional closure supports the staged default in ADR 0027; it is not a
claim that desktop qualification is complete. Native and browser notification paths are
wired, including preference gating, event-derived summaries, focused-session
suppression, activation routing, and a user-initiated test; dependable PWA
background delivery remains a publication gate. Promotion still requires the
platform, accessibility, security, memory, widget, visual-parity,
offline-packaging, rollback, and soak gates in
[ADR 0023](../adr/0023-lit-web-frontend-and-webview-host.md). Slint remains the
explicit rollback while those gates and the Wry rollout are completed.

## Key workflows

1. **New session**: choose a workspace and write the initial prompt, optionally
   attaching files → generate the session title from that prompt, with a
   bounded prompt-derived fallback if generation is unavailable → choose the
   base branch and whether to fetch its upstream before worktree creation →
   choose optional mode, model, permission, and model-derived thinking value →
   create the session worktree and branch → create its first thread → send the
   prompt with its attachments. Defaults keep the visible flow short; failures
   after session creation leave the created session available and report which
   later step did not complete.
2. **New thread**: one action from an open session (tab "+" / palette);
   opens a provisional, cancelable form before any server mutation; inherits
   the worktree and chooses mode, model, thinking, and permission, with an
   optional initial prompt and bounded file/paste attachments. Canonical flow:
   plan thread → code thread → review thread on one branch.
3. **Approval loop**: prompt inline in chat and as a notification; show
   exactly what will run; "always allow" is the ask → allow-list migration
   path; resolving from any client updates all (SSE).
4. **Diff review & apply**: turn/session ends → review S4 → accept or
   revert (checkpoint undo/redo backs this).
5. **PR flow**: session branch → Pull request inspection tab → inspect current
   status and use the lifecycle actions the server reports as available.
6. **Provider onboarding**: S6 on first run and from settings.
7. **Automated code review**: `/reviews` → inspect App/job health → tune
   execution settings → configure repositories and reviewer routing/personas →
   monitor or act on review jobs.
8. **Automation administration**: `/automations` → start from a template or a
   blank automation → select workspace, schedule, prompt, and run defaults →
   save, run on demand, or delete with confirmation.

## Component patterns (shared vocabulary)

- **Tool card**: collapsed = icon + summary + status chip; expanded =
  args, streamed output, exit code, duration. Denied/failed states visually
  distinct.
- **Approval prompt**: renders the allow-list key it would create (e.g.
  `shell:cargo`), three actions, keyboard/tap parity.
- **Status chips**: one component for session state, turn state, tool
  state, and CI state — consistent colors.
- **Markdown stream**: renders incrementally from `assistant.delta` events;
  code blocks use the same highlight tokens as the file viewer.
