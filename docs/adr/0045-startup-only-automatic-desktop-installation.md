# ADR 0045: Startup-only automatic desktop installation

Status: Accepted (2026-08)

## Context

The pre-main updater from ADR 0044 can safely replace and restart the desktop
before server, session, or user work begins. Reusing that automatic install
behavior for a release discovered after the main window appears would instead
interrupt active agent turns and the user's current work without consent.
Long-running desktop processes should still be able to discover releases
without forcing users to restart just to learn that one exists.

## Decision

Automatic desktop installation and restart are permitted only during the
pre-main-window startup preflight. While the main window is running, automatic
release checks are check-only: a newer release is cached and surfaced as an
**Update** indicator leading to Settings → General.

The runtime installer is reachable only from an explicit user click on
**Install and restart**. Ignoring a detected release leaves the executable and
process untouched; the next natural application launch installs it through the
startup preflight. Manual **Check for updates** follows the same consent rule.

The standalone server and search policies remain unchanged because replacing
their on-disk binaries does not restart their active processes.

## Consequences

- A background release check cannot interrupt active desktop work.
- Users can install immediately with an action whose label makes the restart
  explicit, or defer without dismissing work.
- Runtime checking and startup installation intentionally use separate command
  paths even though both share release selection and verification code.
- Future runtime check triggers must surface availability rather than invoke
  installation directly.
