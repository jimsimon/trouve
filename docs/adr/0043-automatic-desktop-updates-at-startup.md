# ADR 0043: Automatic desktop updates at startup

Status: Accepted (2026-08)

## Context

ADR 0042 gave direct binary installations a checksummed update path, but made
desktop installation an explicit follow-up action after a background check.
That still requires users to notice the result and leaves many installations
behind the release train. The desktop already has a client-local General
settings store suitable for preferences that do not belong to server state.
The Wry desktop and Lit frontend share these preferences through the native
host bridge, so startup can read the same value before the web app exists.

## Decision

The desktop checks the stable release channel at startup and, when a newer
eligible release exists, verifies, installs, and restarts into it
automatically. Automatic desktop updates default to enabled and can be
disabled with a persisted checkbox in Settings → General. Disabling them
prevents startup network checks and installations but leaves manual check and
install actions available.

`TROUVE_DISABLE_AUTO_UPDATE` remains a deployment-level override. Development
builds still cannot replace themselves. Standalone server and search restart
policy is unchanged from ADR 0042.

The updater remains a library linked into each release binary rather than an
independently running executable. Updating any component therefore also
updates the updater code that component will use on its next check.

## Consequences

- Direct desktop installations normally converge to the latest stable release
  without an extra user action.
- An eligible update can cause one automatic restart shortly after launch.
- Users who prefer package-manager or manually controlled updates can opt out
  in the UI, while managed deployments can enforce the environment override.
- The preference remains local to the desktop frontend and requires no
  protocol event, endpoint, or durable server state.
