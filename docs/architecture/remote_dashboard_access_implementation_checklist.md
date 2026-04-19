# Remote Dashboard Access Implementation Checklist

## Purpose

Turn the remote-access architecture in [remote_dashboard_access_plan.md](/C:/repos/TV_bot_core/docs/architecture/remote_dashboard_access_plan.md) into an execution-ready checklist for this repository.

This document is the working implementation map for:

- repo code changes
- deployment and ops work
- documentation work
- validation and release gating

Current implementation status:

- phases 1 and 2 docs and deployment assets are already in the repo
- phase 3 trusted identity contract is implemented
- phase 5 backend authorization is implemented
- phase 6 dashboard identity and authorization UX is implemented
- the remaining major work is Aurora-side ingress wiring and end-to-end remote paper validation

## Execution Rules

These rules stay fixed while implementing this checklist:

- do not expose the runtime host directly to the internet
- do not move broker or strategy logic into the dashboard, proxy, or auth layer
- do not make the dashboard the source of truth for authz, arming, readiness, orders, or positions
- do not trust remote identity headers unless they come from an explicitly trusted local upstream
- do not treat remote dashboard work as complete without tests and paper-mode verification

## Phase Order

Recommended execution order:

1. Production dashboard serving and same-origin proxying
2. Linux deployment assets and host firewall posture
3. Trusted operator identity contracts in backend and wire models
4. Backend authorization for privileged actions
5. Dashboard identity and authorization UX
6. Break-glass runbooks and release validation
7. Optional Cloudflare path

## Phase 1: Production Dashboard Serving

Goal:

- serve the built dashboard from the production server through one origin
- proxy both HTTP and WebSocket control-plane traffic to the localhost runtime

### Checklist

- [ ] Decide whether production dashboard assets will be built in CI, in packaging scripts, or on-host during deployment.
- [ ] Add a production serving subsection to [apps/dashboard/README.md](/C:/repos/TV_bot_core/apps/dashboard/README.md).
- [ ] Ensure the dashboard can operate with a same-origin production base URL and production WebSocket origin.
- [ ] Verify `VITE_CONTROL_API_BASE_URL` and `VITE_CONTROL_API_EVENTS_URL` behavior in [apps/dashboard/src/lib/api.ts](/C:/repos/TV_bot_core/apps/dashboard/src/lib/api.ts).
- [ ] Confirm no dashboard route assumes the Vite dev proxy in production.
- [ ] Add or update dashboard tests for production URL resolution behavior.
- [ ] Add a deployment artifact location for built dashboard assets.

### Likely Repo Touch Points

- [apps/dashboard/src/lib/api.ts](/C:/repos/TV_bot_core/apps/dashboard/src/lib/api.ts)
- [apps/dashboard/vite.config.ts](/C:/repos/TV_bot_core/apps/dashboard/vite.config.ts)
- [apps/dashboard/README.md](/C:/repos/TV_bot_core/apps/dashboard/README.md)
- [scripts/package_release.ps1](/C:/repos/TV_bot_core/scripts/package_release.ps1)
- [scripts/package_release.sh](/C:/repos/TV_bot_core/scripts/package_release.sh)

### Exit Criteria

- remote browser loads dashboard successfully through the production origin
- all dashboard HTTP calls resolve correctly through the same origin
- `/events` and `/chart/stream` work through the production ingress path

## Phase 2: Linux Deployment Assets And Host Posture

Goal:

- make the exchange-near Linux deployment reproducible and safe

### Checklist

- [ ] Create a Linux deployment runbook under `docs/ops`.
- [ ] Add example `systemd` units for:
  - `tv-bot-runtime`
  - `caddy`
  - optional `postgresql` if self-hosted in the same deployment guide
- [ ] Add a production Caddy config example for:
  - static dashboard serving
  - proxying runtime HTTP routes to `127.0.0.1:8080`
  - proxying WebSocket routes to `127.0.0.1:8081`
- [ ] Add a firewall checklist that explicitly blocks public access to runtime and Postgres ports.
- [ ] Document required Tailscale installation and enrollment steps for the server.
- [ ] Document required Tailscale installation and enrollment steps for the operator PC.
- [ ] Add packaging or deployment notes describing where built assets and config files should live on Linux.

### Likely Repo Touch Points

- new `docs/ops/remote_deployment.md`
- new `docs/ops/remote_operator_access.md`
- new `deploy/` examples or `scripts/deploy/` assets if you want checked-in service files
- [README.md](/C:/repos/TV_bot_core/README.md)

### Exit Criteria

- deployment can be repeated from docs without hand-wavy steps
- runtime host remains on localhost-only binds
- runtime and database ports are not public

## Phase 3: Trusted Operator Identity In Backend Contracts

Goal:

- carry authenticated operator identity from the trusted ingress layer into privileged backend command handling and journaling

### Checklist

- [ ] Define a normalized operator identity model in shared Rust types.
- [ ] Decide whether operator identity belongs in `core_types`, `control_api`, or both:
  - `core_types` for persisted and journaled identity
  - `control_api` for transport request metadata
- [ ] Extend control-plane request models to carry authenticated operator context for privileged requests.
- [ ] Extend runtime journal models to persist authenticated operator identity.
- [ ] Extend command-result and event models where useful for operator-facing audit views.
- [ ] Add a trusted-upstream extraction path in the runtime host that reads configured identity headers only from trusted local proxy sources.
- [ ] Fail closed for privileged commands when `require_authenticated_identity_for_privileged_commands` is enabled and identity is absent.
- [ ] Ensure direct local requests cannot spoof remote operator identity when not coming through a trusted upstream path.

### Likely Repo Touch Points

- [crates/core_types/src/lib.rs](/C:/repos/TV_bot_core/crates/core_types/src/lib.rs)
- [crates/control_api/src/lib.rs](/C:/repos/TV_bot_core/crates/control_api/src/lib.rs)
- [crates/journal/src/lib.rs](/C:/repos/TV_bot_core/crates/journal/src/lib.rs)
- [apps/runtime/src/host.rs](/C:/repos/TV_bot_core/apps/runtime/src/host.rs)

### Suggested Deliverables

- `AuthenticatedOperator`
- `AuthenticatedSession`
- trusted proxy config model
- journal fields for operator id and access provider

### Exit Criteria

- authenticated operator identity survives from ingress to journal for privileged actions
- privileged commands are rejected when required identity is missing

## Phase 4: Config Surface For Trusted Proxy Mode

Goal:

- make trusted-identity behavior explicit and configurable

### Checklist

- [ ] Add config fields for remote access mode and trusted proxy behavior.
- [ ] Keep safe defaults:
  - localhost runtime binds
  - trusted identity disabled by default
  - privileged command identity requirement disabled unless explicitly enabled for remote deployment
- [ ] Add config validation for contradictory or unsafe combinations.
- [ ] Expose relevant remote-access settings through status or settings views if appropriate.
- [ ] Decide whether these settings are editable at runtime or startup-only.

### Likely Repo Touch Points

- [crates/config/src/lib.rs](/C:/repos/TV_bot_core/crates/config/src/lib.rs)
- [config/runtime.example.toml](/C:/repos/TV_bot_core/config/runtime.example.toml)
- [apps/runtime/src/host.rs](/C:/repos/TV_bot_core/apps/runtime/src/host.rs)
- [apps/dashboard/src/types/controlApi.ts](/C:/repos/TV_bot_core/apps/dashboard/src/types/controlApi.ts)
- [apps/dashboard/src/lib/api.ts](/C:/repos/TV_bot_core/apps/dashboard/src/lib/api.ts)

### Exit Criteria

- remote-auth behavior is explicit in config
- unsafe config combinations are rejected or loudly surfaced

## Phase 5: Backend Authorization For Privileged Commands

Goal:

- enforce privileged command authorization in the backend, not just the UI

### Checklist

- [ ] Define role or capability model for:
  - `viewer`
  - `operator`
  - `trade_operator`
- [ ] Map runtime command types to required roles or capabilities.
- [ ] Apply authorization checks in runtime host command entrypoints before privileged dispatch.
- [ ] Keep existing safety gates intact after authorization succeeds.
- [ ] Return operator-meaningful authorization failures.
- [ ] Journal rejected authorization attempts where appropriate.

### Likely Repo Touch Points

- [apps/runtime/src/host.rs](/C:/repos/TV_bot_core/apps/runtime/src/host.rs)
- [crates/control_api/src/http.rs](/C:/repos/TV_bot_core/crates/control_api/src/http.rs)
- [crates/control_api/src/lib.rs](/C:/repos/TV_bot_core/crates/control_api/src/lib.rs)
- [crates/runtime_kernel/src/lib.rs](/C:/repos/TV_bot_core/crates/runtime_kernel/src/lib.rs)

### Suggested Command Grouping

- read-only:
  - status
  - readiness
  - health
  - chart
  - history
  - journal
  - event streams
- operator:
  - strategy validate
  - strategy load
  - settings update
  - warmup
  - mode switch
  - pause and resume
- trade operator:
  - arm and disarm
  - manual entry
  - flatten
  - close position
  - cancel orders
  - reconnect and shutdown review decisions

### Exit Criteria

- UI tampering cannot bypass backend authorization
- unauthorized users cannot perform privileged actions even with valid transport access

## Phase 6: Dashboard Identity And Authorization UX

Goal:

- make remote operator identity visible and keep authorization behavior understandable

### Checklist

- [ ] Add authenticated operator display in the dashboard header or operator rail.
- [ ] Add access-provider display such as `Tailscale` or `Cloudflare Access`.
- [ ] Add clear warning state if authenticated operator identity is unavailable.
- [ ] Hide or disable privileged controls for unauthorized users.
- [ ] Preserve backend-first safety model:
  - disabled UI is convenience only
  - backend remains authoritative
- [ ] Add tests for identity display and privileged-control gating.

### Likely Repo Touch Points

- [apps/dashboard/src/App.tsx](/C:/repos/TV_bot_core/apps/dashboard/src/App.tsx)
- [apps/dashboard/src/dashboardModels.ts](/C:/repos/TV_bot_core/apps/dashboard/src/dashboardModels.ts)
- [apps/dashboard/src/lib/dashboardProjection.ts](/C:/repos/TV_bot_core/apps/dashboard/src/lib/dashboardProjection.ts)
- [apps/dashboard/src/components/dashboardControlPanels.tsx](/C:/repos/TV_bot_core/apps/dashboard/src/components/dashboardControlPanels.tsx)
- [apps/dashboard/src/components/dashboardMonitoring.tsx](/C:/repos/TV_bot_core/apps/dashboard/src/components/dashboardMonitoring.tsx)
- [apps/dashboard/src/types/controlApi.ts](/C:/repos/TV_bot_core/apps/dashboard/src/types/controlApi.ts)

### Exit Criteria

- operator can see who is authenticated
- unauthorized controls are clearly unavailable
- paper and live remain visually impossible to confuse

## Phase 7: Break-Glass CLI And SSH Workflows

Goal:

- ensure the operator can still act safely if the web dashboard path is unavailable

### Checklist

- [ ] Add a remote break-glass section to CLI and ops docs.
- [ ] Document Tailscale SSH workflow for the server.
- [ ] Document CLI commands for:
  - status
  - readiness
  - warmup
  - mode change
  - arm and disarm
  - flatten
  - reconnect review
  - shutdown review
- [ ] Verify the CLI remains usable through the same local control plane on the server.
- [ ] Add release-checklist items covering break-glass validation.

### Likely Repo Touch Points

- [docs/ops/cli_standalone.md](/C:/repos/TV_bot_core/docs/ops/cli_standalone.md)
- [docs/ops/debugging_guide.md](/C:/repos/TV_bot_core/docs/ops/debugging_guide.md)
- [docs/ops/release_checklist.md](/C:/repos/TV_bot_core/docs/ops/release_checklist.md)

### Exit Criteria

- operator can recover without depending on the browser dashboard

## Phase 8: Tests And Acceptance Coverage

Goal:

- meet the repo's safety-first testing bar for remote access

### Backend Tests

- [ ] Config tests for new remote-access and trusted-proxy settings.
- [ ] Runtime host tests for trusted identity extraction.
- [ ] Runtime host tests for rejection of spoofed or missing identity.
- [ ] Runtime host tests for backend authorization per command class.
- [ ] Journal tests for authenticated operator persistence.
- [ ] Control API tests for updated error or status mapping if authz failures are added.

### Frontend Tests

- [ ] Dashboard tests for identity display.
- [ ] Dashboard tests for privileged-control gating.
- [ ] Dashboard tests for same-origin production API behavior.

### Ops Validation

- [ ] Remote dashboard loads through Tailscale.
- [ ] WebSockets stream through the ingress path without regressions.
- [ ] Runtime ports are unreachable from public network paths.
- [ ] Break-glass SSH and CLI workflow succeeds.

### Paper-Mode Remote Validation

- [ ] remote warmup
- [ ] remote mode switch
- [ ] remote arm and disarm
- [ ] remote manual entry with broker-side protections
- [ ] remote flatten
- [ ] remote degraded no-new-entry behavior
- [ ] remote reconnect review handling

## Phase 9: Optional Cloudflare Public-Browser Path

Goal:

- add a browser-only public entrypoint without exposing the origin IP or runtime ports

### Checklist

- [ ] Create a Cloudflare Tunnel deployment guide.
- [ ] Create a Cloudflare Access application guide.
- [ ] Define application hostname and session duration policy.
- [ ] Decide on identity provider versus One-Time PIN for allowed users.
- [ ] Ensure the origin still remains non-public.
- [ ] Validate WebSockets through Cloudflare.
- [ ] If using Cloudflare Access token validation at origin, document and implement the chosen validation approach clearly.

### Likely Repo Touch Points

- new `docs/ops/cloudflare_remote_access.md`
- new deployment examples if checked into repo
- optional runtime config docs if origin-side validation is implemented

### Exit Criteria

- public browser access works
- origin remains private
- application remains protected by Access policy

## Suggested Task Slices

If you want to implement this in small safe PRs, this is the clean split:

1. Docs and deployment assets only
- new runbooks
- Caddy examples
- Linux service examples

2. Dashboard production-origin cleanup
- frontend production URL behavior
- packaging updates

3. Backend identity contract
- shared types
- trusted proxy extraction
- journal persistence

4. Backend authorization
- role model
- command gating
- tests

5. Dashboard identity UX
- operator display
- control gating
- tests

6. Release validation and paper-mode remote pass

## Definition Of Done

This implementation track is not done unless:

- runtime remains localhost-only in the standard remote deployment
- production dashboard is served through the chosen ingress layer
- authenticated operator identity reaches journaled privileged actions
- backend authorization protects privileged commands
- break-glass CLI and SSH paths are documented and verified
- remote paper-mode verification passes
- release docs and runbooks are updated

## Immediate Next Task Recommendation

The best first implementation slice is:

1. production dashboard serving cleanup
2. Linux Caddy plus `systemd` deployment assets
3. Tailscale operator runbook

That gives you a safe private remote shell around the existing local control plane before changing backend identity and authz contracts.
