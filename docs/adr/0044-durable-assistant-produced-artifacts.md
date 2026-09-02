# 0044: Durable assistant-produced artifacts

**Status:** Accepted

**Date:** 2026-09-01

## Context

Tools can return screenshots, audio, video, and other binary resources. The
provider bridge already forwarded image bytes to models as vision content, but
those bytes were either removed from the tool result or left inside transient
MCP result JSON. Neither representation produced a durable transcript item, so
clients could not present the output after a live turn or reconstruct it after
a restart.

Embedding base64 payloads directly in the append-only event log would make
event replay and thread snapshots unbounded. A separate UI-only channel would
also violate the durable event-log architecture.

## Decision

Assistant- and tool-produced binary output uses the existing attachment store.
The engine extracts supported inline media blocks from first-party tool results
and standard MCP image, audio, and embedded-resource content. It validates and
stages the decoded bytes with the same limits and confinement used for uploaded
attachments, removes the base64 payload from the persisted tool result, and
atomically commits the attachment rows with a new `assistant.artifacts` event.

The event contains only attachment metadata plus its turn and optional tool
call identifier. The shared thread-view fold exposes a corresponding artifacts
item. Product clients render that item with the same attachment list and media
preview components used for user uploads; attachment bytes continue to be read
through the existing protocol endpoint.

Images may additionally continue to flow to the active model as native vision
content. Markdown and arbitrary tool JSON do not gain permission to embed local
files or data URLs.

## Consequences

- Screenshots and other tool-produced files survive replay, pagination, and
  application restarts.
- Event records remain small and cursor-friendly because binary bytes live in
  the bounded attachment store.
- Attachment validation, cleanup, authorization, and serving have one shared
  implementation for user- and assistant-produced content.
- New producer-specific binary formats must be normalized into attachment
  uploads rather than adding another durable blob channel.
