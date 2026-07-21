# GitHub App-backed code review service

Status: Accepted (2026-07)

## Context

The desktop GitHub integration is deliberately account-centric and OAuth-only
(ADR 0010). Automated code review has different requirements: it must act as a
stable bot, be restricted to explicitly installed repositories, run while no
desktop client is connected, and avoid consuming the user's OAuth rate-limit
budget. The web client anticipated by ADR 0005 also needs to be independently
deployable beside a headless server.

## Decision

- Keep the OAuth account feed unchanged for desktop clients.
- Add a distinct GitHub App installation boundary for automated reviews.
  Installation tokens perform discovery, authenticated Git fetches, and review
  publication, so GitHub attributes activity to the App bot and meters it per
  installation.
- Repository review policy is independent of discovery and has three states:
  off, manual reviewer requests only, or automatic review of each new head SHA.
- The server owns webhook verification, a durable review queue, reconciliation
  polling, exact-SHA session creation, and review publication. Reviews run
  through the normal read-only agent/session path and remain visible through
  the protocol event log.
- The review web UI is a separate TypeScript SPA/container. It speaks only the
  versioned HTTP/SSE protocol; deployments normally expose it and the API under
  one reverse-proxied HTTPS origin.

## Consequences

The desktop feed and its user quota are unaffected by review automation. A
GitHub App private key and webhook secret become server-side deployment
secrets, while installation selection provides repository-level access
control. Webhooks give low-latency triggers and periodic polling repairs missed
deliveries. The independently deployed UI adds one container but lets the
server and static frontend be upgraded and secured separately.

## Alternatives rejected

Posting through the existing OAuth token would impersonate the user and share
their rate limit. A machine-user account would require separately managed
credentials and may consume an organization seat. Webhook-only processing was
rejected because a durable reconciliation path is still needed after downtime
or failed deliveries.
