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
- Confirm the dashboard shows the authenticated operator identity and access level when validating the remote path.
- Confirm `viewer`, `operator`, and `trade_operator` sessions gate controls the expected way.
- Confirm status, readiness, events, journal, history, health, settings, and strategy workflows all load from the local control plane.
- If validating the remote dashboard path, confirm the dashboard loads through the intended ingress layer and both `/events` and `/chart/stream` work end-to-end.

## Remote Access Validation

- Confirm the standard remote deployment keeps `control_api` binds on localhost only.
- Confirm runtime ports `8080` and `8081` are not publicly reachable.
- Confirm Postgres is not publicly reachable.
- Confirm the remote dashboard loads through the intended private access path, currently Tailscale plus Caddy for the recommended V1 flow.
- Confirm the trusted ingress path supplies the expected authenticated operator headers and roles on the Aurora-side host.
- Confirm privileged routes fail closed if required authenticated operator identity is missing.
- Confirm `viewer` and `operator` accounts cannot bypass backend authorization for trade-capable actions.
- Confirm the break-glass SSH and CLI workflow is documented and works on the candidate build.
- Run the Aurora-side validation flow in [aurora_remote_setup_and_test.md](/C:/repos/TV_bot_core/docs/ops/aurora_remote_setup_and_test.md) for the actual exchange-near deployment candidate.

## Packaging And Delivery

- Build and archive the intended runtime and dashboard deliverables for the target release with:
  - Windows: `.\scripts\package_release.ps1`
  - Linux/macOS: `./scripts/package_release.sh`
- On Windows, stop any running local runtime or dashboard dev server before packaging so locked binaries or native Node modules do not interfere with the build.
- Verify the packaged config defaults do not imply live trading or silent fallback behavior.
- Verify the packaged deployment examples and remote ops runbooks match the candidate build if you intend to support the Linux remote deployment path in that release.
- Record the exact runtime config, strategy file, and candidate commit used for the validation session.

## Sign-Off Rule

Do not mark the release complete until the validation notes, CI results, and operator runbooks all point to the same candidate commit.
