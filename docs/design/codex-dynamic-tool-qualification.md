# Codex dynamic-tool qualification

Status: retained comparison probe; not the shipping tool transport.

Trouve currently drives Codex through app-server and exposes the complete
Trouve-owned tool surface through MCP. App-server also has an experimental
`dynamicTools` capability that can register host-owned tools directly. That
could remove one translation layer, but it does not disable Codex built-ins and
does not change the requirement that every mutation cross `ToolExecutor`.

Run the probe against the installed, already-authenticated Codex CLI:

```sh
python3 scripts/qualify_codex_dynamic_tools.py
```

The script creates a disposable read-only thread, registers one dynamic tool,
handles its callback exactly once, restarts app-server, resumes the thread
without re-registering the schema, repeats the callback, and deletes the
disposable thread.

Before dynamic tools can replace MCP, qualification must prove:

1. Trouve's complete effective tool schema registers without semantic loss,
   including deferred or searchable tools where applicable.
2. Every `item/tool/call` reaches `ToolExecutor` exactly once, with text,
   structured, and inline-image results translated correctly.
3. Approval, concurrent reads, serialized mutations, cancellation, steering,
   reconnect, resume, and multiple clients cannot orphan or duplicate effects.
4. Built-in mutation tools remain disabled or confined and cannot bypass
   `ToolExecutor`.
5. The supported Codex release keeps the API stable enough to ship with a safe
   production fallback.

## Current evidence

On 2026-08-24, `codex-cli 0.145.0` passed the baseline with two turns. Each
turn produced one callback with matching started/completed items, used the
returned text, and showed no built-in tool invocation. A cold app-server
restart resumed the thread and restored its dynamic-tool schema.

Keep the candidate and probe, but do not replace the shipping MCP bridge yet.
The app-server capability remains experimental, and the broader schema, image,
concurrency, approval, cancellation, steering, and multi-client gates above
have not all been proven. Passing them would justify an internal comparison;
production should change only when the compatibility burden is accepted and
the direct path measurably improves reliability or latency.

See the official
[Codex app-server protocol](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#dynamic-tool-calls-experimental).
