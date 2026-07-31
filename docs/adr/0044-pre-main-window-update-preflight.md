# ADR 0023: Pre-main-window update preflight

Status: Accepted (2026-07)

## Context

ADR 0022 made desktop updates automatic at startup, but the original
implementation opened the full application while checking and then restarted
it without any visible update state outside Settings. That made slow downloads
look like an unexplained restart and allowed the embedded server and session
controller to initialize in a process that might immediately be replaced.

## Decision

When automatic desktop updates are enabled, a dedicated startup window owns
the update preflight before the main window appears. It reports release checks,
archive byte progress, checksum verification, extraction, installation, and
restart. The main window, embedded server, session restoration, and controller
remain unstarted until the installed executable is current.

An update failure leaves the existing executable untouched and presents
**Retry** and **Open trouve** actions, so update availability never makes the
application unusable. A successful replacement restarts with a one-shot
version marker; the replacement process consumes that marker and opens the
main window without repeating the preflight.

The startup window is part of `trouve-app` and uses the same Slint theme and
assets. Disabling automatic updates in Settings or with
`TROUVE_DISABLE_AUTO_UPDATE` skips the preflight entirely. Manual checks remain
inside Settings → General.

## Consequences

- Users can distinguish checking, downloading, verifying, installing, and
  restarting from a hung or unexpectedly closing application.
- No server or session work starts in an executable that is about to restart.
- Offline and failed-update starts require one explicit **Open trouve** action
  or a retry when automatic updates are enabled.
- The one-shot restart marker is process-local coordination, not durable user
  state or protocol state.
