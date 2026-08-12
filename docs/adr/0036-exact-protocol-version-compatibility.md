# ADR 0036: Exact protocol version compatibility

Status: Accepted (2026-08)

## Context

The HTTP protocol is generated into Rust and TypeScript clients. Several wire
types are closed enums or discriminated unions, so adding a variant can make an
older generated client fail deserialization even when every existing variant
and endpoint remains unchanged. Protocol 3.25 and 3.26 added such variants,
while clients continued accepting any newer 3.x server and therefore reported
compatibility until a response containing the new value failed.

The protocol has no capability negotiation that can prove a particular older
client understands every value a newer server may emit.

## Decision

- Protocol 4.0 acknowledges the closed-enum additions as a breaking change.
- Clients require the server's exact protocol version before consuming typed
  responses. Major/minor version bumps still communicate whether a schema
  change is breaking or additive, but a shared major no longer implies runtime
  forward compatibility.
- Relaxing exact matching requires deliberately forward-compatible wire
  representations or capability negotiation, with compatibility tests and a
  superseding ADR.

## Consequences

- Version-skewed clients fail immediately with an actionable compatibility
  error instead of failing later while decoding an otherwise valid response.
- Server and generated clients must be deployed or refreshed together after
  every protocol version bump, including additive minor bumps.
- Existing 3.x clients correctly reject the 4.0 server at their major-version
  gate.

## Alternatives rejected

- A 4.0 bump while retaining newer-same-major acceptance would recreate the
  defect at 4.1.
- Converting every closed union to an unknown-value wrapper now would weaken
  typed exhaustive handling without defining useful semantics for unknown
  events and projection states.
