# Multiple ephemeral terminals per session

Status: Accepted (2026-07)

## Context

The integrated terminal originally allowed one PTY per session. That was
enough for a single terminal panel, but terminal tabs require independent
shells with separate process, size, output backlog, and lifecycle state.
Terminal output is high-volume, transient byte transport and was already kept
outside the persisted agent event log.

## Decision

A session may own zero or more live or exited terminal instances. Every
terminal starts in the session worktree and remains addressable by its own
terminal id. The protocol adds plural session endpoints to list existing
terminal instances and create a new one. The existing singular open endpoint
continues to open or reattach to a default terminal for compatibility.

Terminal tabs, parsed screen state, titles, selection, and search remain
client concerns. The server owns PTYs, capped raw-output backlogs, and terminal
lifecycle. Terminal instances and output remain ephemeral: they are available
while the server process lives but are not written to the persisted event log.

## Consequences

- Clients can reconnect to and display every terminal owned by a session.
- Closing a tab kills and removes only that terminal; archiving or deleting a
  session, or closing its workspace, kills all affected terminals. Reopening
  a session or workspace allows fresh terminals. Server shutdown kills every
  remaining terminal explicitly.
- PTY output continues to use resumable, byte-offset SSE rather than durable
  domain events.
- The client must follow multiple terminal streams and retain an independent
  parser/grid for every open tab.

## Alternatives rejected

- Multiplexing shells inside one PTY would couple tabs to a program such as
  tmux and would not provide independent server-side lifecycle control.
- Persisting raw terminal output in the agent event log would mix transient,
  high-volume transport with durable session history.
