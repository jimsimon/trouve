---
name: protocol-change
description: Make a change to the trouve harness protocol (trouve-protocol types, endpoints, event taxonomy, or the OpenAPI schema). Use whenever a request/response body, event type, or route changes.
---

# Changing the protocol

The protocol is versioned and snapshot-tested; changes must be deliberate.

1. Edit the wire types in `crates/trouve-protocol` (never define wire shapes
   in other crates). Events follow the taxonomy rules in
   `docs/design/event-log.md`: new UI-visible state = new event type;
   existing types are never repurposed.
2. Additive change (new optional field, new event type, new endpoint): keep
   `PROTOCOL_VERSION`'s major, bump the minor. Breaking change (removing or
   changing meaning): bump the major — and question whether it's really
   necessary; clients must ignore unknown event types, so additive paths
   usually exist.
3. Update handlers/`ApiDoc` in `crates/trouve-server/src/lib.rs` if routes
   or bodies changed.
4. Regenerate the schema snapshot:
   `TROUVE_UPDATE_OPENAPI=1 cargo test -p trouve-server openapi`
   and commit the updated `crates/trouve-server/tests/snapshots/openapi.json`
   together with the code change.
5. Run `cargo test -p trouve-server` — the e2e tests exercise the protocol
   end to end with a scripted provider.
