# Release Checklist

Use this checklist before calling V1 release-ready.

## CI And Build Validation

- Confirm `.github/workflows/ci.yml` is green on the candidate commit.
- Confirm Rust workspace tests passed on:
  - Windows
  - Linux
  - macOS
- Confirm dashboard tests and production build passed.

## Paper/Demo Validation

- Run the `paper_demo_verification.md` flow on the release candidate.
- Confirm explicit arm-before-trade behavior.
- Confirm degraded-feed and operator no-new-entry gating.
- Confirm startup/reconnect review handling for:
  - `close_position`
  - `leave_broker_protected`
  - `reattach_bot_management`

## Storage And Audit Validation

- Confirm Postgres-primary operation on the candidate build.
- Confirm the fallback override warning flow still behaves correctly if Postgres is intentionally unavailable.
- Review journal and history surfaces for the paper validation session and verify the records are complete.

## Dashboard Validation

- Confirm mode visibility clearly separates paper from live.
- Confirm dangerous actions still require confirmation.
- Confirm status, readiness, events, journal, history, health, settings, and strategy workflows all load from the local control plane.

## Packaging And Delivery

- Build and archive the intended runtime and dashboard deliverables for the target release with:
  - Windows: `.\scripts\package_release.ps1`
  - Linux/macOS: `./scripts/package_release.sh`
- On Windows, stop any running local runtime or dashboard dev server before packaging so locked binaries or native Node modules do not interfere with the build.
- Verify the packaged config defaults do not imply live trading or silent fallback behavior.
- Record the exact runtime config, strategy file, and candidate commit used for the validation session.

## Sign-Off Rule

Do not mark the release complete until the validation notes, CI results, and operator runbooks all point to the same candidate commit.
