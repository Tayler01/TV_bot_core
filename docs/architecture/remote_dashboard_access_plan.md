# Remote Dashboard Access Plan

## Purpose

Add a secure remote-access architecture for the existing web dashboard so the bot can run on a low-latency exchange-near server while the operator uses the dashboard from a separate PC, without violating the boundaries in [AGENTS.md](/C:/repos/TV_bot_core/AGENTS.md).

The remote-access design must:

- preserve the strategy-agnostic execution core
- keep the runtime host as the only source of truth
- avoid direct public exposure of unauthenticated runtime control endpoints
- support both HTTP and WebSocket dashboard traffic cleanly
- preserve explicit arming, mode visibility, and existing backend safety gates
- improve operator identity, auditability, and operational recovery paths
- work cross-platform at the product level even if the first production deployment target is Linux

This document is the source of truth for the remote dashboard hosting, access-control, security, login, rollout, and acceptance plan.

## Planning Status

Current state as of 2026-04-19:

- The runtime host already exposes a local HTTP and WebSocket control plane through [apps/runtime/src/host.rs](/C:/repos/TV_bot_core/apps/runtime/src/host.rs).
- The control plane is intentionally local-first today, with default binds of `127.0.0.1:8080` and `127.0.0.1:8081` in [config/runtime.example.toml](/C:/repos/TV_bot_core/config/runtime.example.toml) and [crates/config/src/lib.rs](/C:/repos/TV_bot_core/crates/config/src/lib.rs).
- The dashboard already consumes only that control plane through [apps/dashboard/src/lib/api.ts](/C:/repos/TV_bot_core/apps/dashboard/src/lib/api.ts) and [apps/dashboard/vite.config.ts](/C:/repos/TV_bot_core/apps/dashboard/vite.config.ts).
- The runtime host already serves sensitive read and write routes including status, readiness, settings, strategy upload and validation, runtime commands, journal, history, event streaming, and chart streaming.
- The current runtime host now supports trusted authenticated operator identity and role propagation for privileged remote use.
- The current command and journal model now preserves authenticated operator metadata for privileged actions, including user id, display name, session id, device id, provider, and roles.

Implementation status update:

- trusted operator identity propagation is now implemented in the current branch
- backend role-based authorization for privileged runtime routes is now implemented in the current branch
- dashboard operator identity and capability gating is now implemented in the current branch
- see [remote_dashboard_access_status.md](/C:/repos/TV_bot_core/docs/architecture/remote_dashboard_access_status.md) for the concrete shipped contract

## Current Repo Reality

The current host routes include:

- `GET /health`
- `GET /status`
- `GET /readiness`
- `GET /chart/config`
- `GET /chart/snapshot`
- `GET /chart/history`
- `GET /history`
- `GET /journal`
- `GET /settings`
- `POST /settings`
- `GET /strategies`
- `POST /strategies/upload`
- `POST /strategies/validate`
- `POST /runtime/commands`
- `POST /commands`
- `WS /events`
- `WS /chart/stream`

These routes are built directly in [apps/runtime/src/host.rs](/C:/repos/TV_bot_core/apps/runtime/src/host.rs).

This is correct for the local-control-plane architecture, but it means the current host must **not** simply be rebound to `0.0.0.0` and published to the internet as-is.

## Core Decision

The recommended V1 production architecture is:

- run the bot, runtime host, Postgres, and dashboard assets on a Linux server close to the exchange
- keep the runtime host bound to localhost only
- add a single edge layer on the same server that serves the dashboard and proxies the runtime host
- use private remote access first, not public internet access
- use Tailscale as the first access-control and operator-auth layer
- add application-aware operator identity propagation and audit logging before treating the remote dashboard as production-ready for live use

Recommended V1 stack:

- `tv-bot-runtime`
- local or same-network Postgres
- Caddy as reverse proxy and static-file server
- Tailscale for operator connectivity and identity

Recommended V2 optional stack:

- Cloudflare Tunnel plus Cloudflare Access in front of the same Caddy origin when browser-only access from unmanaged devices becomes necessary

## Questions And Answers

This section turns the main planning questions into explicit decisions.

### 1. Should the runtime host itself be directly internet-exposed?

Answer: No.

Reason:

- the current runtime host exposes privileged routes
- the current runtime host has no first-class remote login/session layer
- exposing these routes directly would create an avoidable attack surface around trading controls

### 2. Should the dashboard be hosted separately from the runtime host?

Answer: No for V1.

Reason:

- same-server hosting reduces moving parts
- same-origin serving simplifies HTTP and WebSocket proxying
- it avoids extra CORS and cookie complexity
- the trading-critical latency path remains entirely server-local

### 3. Should remote login be implemented inside the dashboard first?

Answer: No.

Reason:

- edge authentication is simpler to ship and safer to harden first
- it avoids exposing unauthenticated control-plane routes while the application still lacks a mature session layer
- it lets the runtime remain focused on trading truth rather than web auth mechanics in the first pass

### 4. What is the best V1 remote access model?

Answer: Tailscale.

Reason:

- ideal for a small number of trusted operators
- avoids public origin exposure
- gives strong device and user identity
- adds MFA through the chosen identity provider
- minimizes external attack surface and network complexity

### 5. What is the best V2 remote access model if browser-only public access is needed?

Answer: Cloudflare Tunnel plus Cloudflare Access.

Reason:

- outbound-only tunnel keeps origin IP private
- Access adds policy-based login, session duration, and identity-provider integration
- WebSockets are supported through Cloudflare
- this can later support guest or secondary operator access

### 6. Should there be separate operator roles?

Answer: Yes.

Minimum target roles:

- `viewer`
- `operator`
- `trade_operator`

V1 may initially enforce these through coarse policy and UI gating, but the runtime must eventually enforce privileged command authorization on the backend.

### 7. Should manual actions record the authenticated operator identity?

Answer: Yes, required.

This is a release-gate requirement for remote operations.

The journal and command-result path should evolve from:

- `dashboard`

to something operator-aware, for example:

- transport source: `dashboard`
- authenticated operator: email or subject id
- session id
- device id or device label when available
- access mechanism such as `tailscale` or `cloudflare_access`

### 8. What is the break-glass path if the web dashboard is unavailable?

Answer: Tailnet SSH plus CLI.

The remote operating model must not depend exclusively on the browser dashboard.

### 9. Should Postgres be remote from the bot server?

Answer: Not for the first production pass.

Keep Postgres on the same server or on a tightly controlled private same-region host to avoid adding unnecessary latency, dependency sprawl, and network failure modes to the trading runtime.

### 10. Should paper and live use the same remote dashboard?

Answer: Yes, but with stronger production posture controls.

Required differences:

- unmistakable mode presentation
- optional distinct hostnames later
- stronger confirmation requirements for live-only dangerous actions
- explicit operator identity in the UI and journal

## Option Review

| Option | Strengths | Weaknesses | Repo fit |
| --- | --- | --- | --- |
| Rebind runtime host directly to public network | Fastest to hack together | Unsafe, no mature auth boundary, high-risk control-plane exposure | Not acceptable |
| Public reverse proxy plus basic auth only | Simple | Weak identity model, poor device trust, weak operator auditability | Not enough |
| Tailscale plus local reverse proxy | Private-by-default, MFA-capable, low operational complexity, excellent small-team fit | Requires client install on operator devices | Best V1 fit |
| Cloudflare Tunnel plus Access | Public hostname without public origin IP, good SSO policies, good browser-only path | More moving parts and public-internet entrypoint semantics | Best V2 fit |
| Custom in-app auth first | Full application control | More work, more risk, distracts from core remote-access hardening | Later enhancement |

## Recommendation

Use **Tailscale plus Caddy** for the first production remote-dashboard rollout.

Keep the runtime host private on localhost and treat the reverse proxy as the only browser-facing origin.

When public browser-only access becomes necessary, layer **Cloudflare Tunnel plus Cloudflare Access** in front of the same local Caddy origin instead of exposing the runtime directly.

## Target Architecture

### V1 Topology

```text
Operator PC
  -> Tailscale
  -> Caddy on exchange-near server
     -> static dashboard assets
     -> reverse proxy /status, /readiness, /history, /journal, /settings, /strategies, /runtime/commands
     -> reverse proxy /events and /chart/stream WebSockets
     -> tv-bot-runtime on 127.0.0.1:8080 and 127.0.0.1:8081
     -> Postgres on localhost or private same-region network
```

### V2 Optional Topology

```text
Operator browser
  -> Cloudflare Access login
  -> Cloudflare Tunnel
  -> Caddy on exchange-near server
  -> tv-bot-runtime on localhost
```

## Architecture Guardrails

These rules are fixed:

- the dashboard must continue to consume only the local control plane
- the runtime host must remain the source of truth for status, readiness, chart data, orders, fills, and command execution
- the reverse proxy and access layer must not become a second source of truth for trading state
- broker-specific logic must not move into the dashboard or proxy layer
- all privileged actions must continue to flow through the audited runtime command path
- remote access must not weaken explicit arming, explicit mode, broker-side safety preference, or journal requirements
- direct public access to runtime host ports is not acceptable

## Threat Model

Primary risks this plan is addressing:

- unauthenticated internet access to privileged runtime routes
- stolen password or weak single-factor login on a public dashboard
- header spoofing if identity metadata is trusted from untrusted network paths
- operator actions that are not attributable to a real authenticated user
- inability to recover when the browser path fails
- accidental confusion between paper and live while operating remotely
- reverse-proxy or access misconfiguration that bypasses expected authentication

## Authentication And Identity Plan

### V1 Authentication

Use Tailscale as the remote-access gate.

Requirements:

- operator account protected by MFA
- server joined to the tailnet
- operator PC joined to the tailnet
- tailnet policy limiting access to the dashboard host
- no non-tailnet public exposure

### V1 Identity Propagation

The runtime should evolve to accept trusted upstream identity metadata from the reverse-proxy layer only when:

- the upstream is local and trusted
- the runtime is configured to trust that upstream explicitly
- direct non-proxy access remains impossible

Suggested forwarded identity model:

- `X-Authenticated-User`
- `X-Authenticated-Session`
- `X-Authenticated-Device`
- `X-Authenticated-Provider`

These names are illustrative. Final names should be chosen deliberately and documented in code and ops docs.

Important:

- the runtime must not trust these headers from arbitrary network clients
- if identity headers are missing in a remote-auth-required deployment mode, privileged commands should fail closed

### V2 Authentication

If a public browser path is needed, use Cloudflare Access in front of the origin.

Capabilities to use:

- IdP login or One-Time PIN for approved email addresses
- short session duration for operator applications
- explicit allow policies
- optional app launcher integration later

## Authorization Plan

Authentication alone is not enough.

The runtime should eventually apply backend authorization for manual and lifecycle actions.

Suggested command authorization policy:

- `viewer`
  - may read status, readiness, history, journal, health, chart, and events
- `operator`
  - viewer permissions
  - may load strategies, validate strategies, update settings, warmup, mode switch, pause, resume
- `trade_operator`
  - operator permissions
  - may arm, disarm, manual entry, flatten, close position, cancel working orders, and acknowledge safety reviews

The first rollout may temporarily rely on edge policy plus UI hiding for part of this, but backend enforcement is the desired end state.

## Hosting Plan

### Production Host

Recommended initial deployment target:

- Linux VPS or dedicated server near the exchange
- systemd-managed services
- local NVMe storage for runtime logs and database files where relevant

Suggested processes:

- `tv-bot-runtime`
- `postgresql`
- `caddy`
- `tailscaled`

### Runtime Binding Policy

Keep:

- `control_api.http_bind = "127.0.0.1:8080"`
- `control_api.websocket_bind = "127.0.0.1:8081"`

Do not change the runtime bind to `0.0.0.0` for the standard remote-dashboard deployment.

### Dashboard Hosting Policy

Use a production build of `apps/dashboard` served by Caddy from the same server.

Production dashboard behavior should be:

- one browser origin
- same-origin HTTP API calls
- same-origin WebSocket upgrade paths
- no dependency on the Vite dev server

## Reverse Proxy Plan

### Reverse Proxy Responsibilities

Caddy should:

- terminate HTTPS for the operator-facing origin
- serve static dashboard assets
- proxy runtime HTTP endpoints to `127.0.0.1:8080`
- proxy runtime WebSocket endpoints to `127.0.0.1:8081`
- optionally inject trusted operator identity metadata from the access layer
- enforce basic response hardening and request-size limits where appropriate

### Suggested V1 Caddyfile Sketch

```caddyfile
bot.internal.example {
    encode zstd gzip

    root * /opt/tv-bot/dashboard
    file_server

    @api path /health /status /readiness /history /journal /settings /strategies /strategies/upload /strategies/validate /runtime/commands /commands /chart/config /chart/snapshot /chart/history
    reverse_proxy @api 127.0.0.1:8080

    @events path /events /chart/stream
    reverse_proxy @events 127.0.0.1:8081
}
```

This is intentionally minimal and not the final hardened production config.

### Suggested V1 Tailscale-Only TLS Policy

Use either:

- Caddy with a tailnet DNS name and local certificate handling, or
- Tailscale Serve in front of Caddy if that operating model proves cleaner

Preferred V1 approach remains Caddy plus tailnet access because it keeps the public and future Cloudflare paths conceptually aligned.

## Network And Firewall Plan

Required host firewall posture:

- allow Tailscale interface traffic as needed
- allow SSH only via tailnet policy or explicitly approved admin path
- deny public inbound access to runtime ports `8080` and `8081`
- deny public inbound access to Postgres
- if using public HTTPS later, expose only the reverse-proxy ingress, never the runtime directly

Required network rule:

- no browser should ever connect directly to the runtime host ports from outside the server

## Secrets And Config Plan

Keep secrets in environment variables, consistent with [docs/ops/credential_setup.md](/C:/repos/TV_bot_core/docs/ops/credential_setup.md).

Production secret categories:

- Databento API key
- Tradovate credentials
- Postgres credentials if externalized
- optional Cloudflare tunnel or API credentials in V2

Non-secret runtime config remains in the runtime TOML file.

Suggested production config additions later:

- remote access mode
- trusted upstream identity mode
- allowed proxy source list
- auth-required-for-privileged-commands flag

## Auditability And Journaling Plan

This is a major required enhancement for remote operations.

### Current Gap

The existing action-source model distinguishes:

- `dashboard`
- `cli`
- `system`

This is useful but insufficient for remote production operation because it does not answer:

- which human operator performed the action
- from which authenticated session
- from which trusted device

### Required Enhancement

Extend manual and lifecycle command journaling so records can include:

- transport source
- authenticated operator id
- session id
- access provider
- optional device label or device id
- request correlation id

Important actions that should carry operator identity:

- strategy load
- strategy validation request
- settings update
- warmup start
- mode change
- arm or disarm
- manual entry
- cancel orders
- flatten
- reconnect review decisions
- shutdown review decisions

### UI Follow-Up

The dashboard should display:

- current authenticated operator
- current access method
- clear warning if authenticated operator identity is unavailable

## Operational Recovery Plan

### Break-Glass Access

Required operational fallback:

- Tailscale SSH access to the server
- standalone CLI access through the existing control plane

This fallback path should be documented and verified before live use.

### Failure Modes To Design For

- dashboard build unavailable
- reverse proxy misconfiguration
- access layer outage
- WebSocket reconnect churn
- runtime still healthy but browser path unavailable
- database degraded state
- reconnect review required during remote session

## Delivery Phases

### Phase 1: Infrastructure And Private Ingress

Deliver:

- Linux production host baseline
- systemd service model for runtime, Postgres, Caddy, and Tailscale
- runtime remains localhost-only
- production dashboard build served by Caddy
- same-origin proxying for HTTP and WebSocket routes
- firewall lock-down of runtime and database ports

Acceptance:

- dashboard loads from the remote PC through the private access path
- `/events` and `/chart/stream` work through the proxy
- runtime ports are unreachable from outside the server

### Phase 2: Trusted Operator Identity

Deliver:

- trusted-upstream identity contract
- runtime support for attaching operator identity to privileged command processing
- journal and command-result model updates
- dashboard operator identity display

Acceptance:

- privileged actions record authenticated operator identity
- privileged actions fail closed when remote-auth-required mode is enabled and identity is missing
- direct local runtime access without trusted headers cannot spoof operator identity

### Phase 3: Authorization And Dangerous-Action Hardening

Deliver:

- role mapping for read-only versus operational versus trade actions
- backend authorization checks for privileged commands
- stronger live-mode confirmation behavior in the dashboard

Acceptance:

- unauthorized operators cannot arm, trade, flatten, or acknowledge reviews
- viewer sessions remain read-only even if the UI is tampered with

### Phase 4: Operational Hardening

Deliver:

- structured runbook for remote deployment
- backup and restore procedure for Postgres and config
- break-glass SSH and CLI verification
- monitoring and restart policy review

Acceptance:

- operator can recover from web-path failure without unsafe manual hacks
- restore steps are documented and validated

### Phase 5: Optional Public Browser Access

Deliver when needed:

- Cloudflare Tunnel
- Cloudflare Access application
- policy-based login
- optional guest or secondary-operator policy

Acceptance:

- origin remains non-public
- Cloudflare Access protects the application
- token validation or equivalent trusted-origin protection is enforced

## Suggested Implementation Order

1. Production dashboard build plus same-origin reverse proxy
2. Linux service and firewall deployment
3. Tailscale private operator access
4. Trusted operator identity contract in runtime and journal
5. Backend authorization for privileged commands
6. Dashboard identity and authorization UX
7. Break-glass CLI and SSH runbooks
8. Optional Cloudflare path if browser-only remote access becomes necessary

## Config Sketches

### Runtime Config

Keep the current control-plane bind posture:

```toml
[control_api]
http_bind = "127.0.0.1:8080"
websocket_bind = "127.0.0.1:8081"
```

Future additive settings could look like:

```toml
[remote_access]
mode = "private_tailnet"
require_authenticated_identity_for_privileged_commands = true
trusted_proxy_sources = ["127.0.0.1/32"]
trusted_identity_header_user = "X-Authenticated-User"
trusted_identity_header_session = "X-Authenticated-Session"
trusted_identity_header_device = "X-Authenticated-Device"
trusted_identity_header_provider = "X-Authenticated-Provider"
```

These settings do not exist yet. They are planning targets.

### systemd Sketch

Suggested service units:

- `tv-bot-runtime.service`
- `caddy.service`
- `postgresql.service`
- `tailscaled.service`

The runtime service should:

- run under a dedicated service account
- load secrets from environment files or a secret manager
- restart on failure
- write structured logs to journalctl and file output if desired

## Acceptance Tests

The remote dashboard work should not be considered complete without targeted verification.

### Minimum Required Acceptance Coverage

1. Remote private-access connectivity
- dashboard loads from the operator PC through Tailscale
- proxied HTTP routes succeed
- proxied WebSocket routes reconnect cleanly

2. Security posture
- runtime host ports are not publicly reachable
- Postgres is not publicly reachable
- direct requests to runtime ports from non-local sources are denied

3. Operator identity
- authenticated operator identity appears in the dashboard
- authenticated operator identity is persisted in journal records for privileged actions
- commands fail if identity is required but absent

4. Authorization
- read-only operator cannot execute privileged commands
- trade operator can execute allowed commands
- unsafe actions still require existing confirmations and backend safety gates

5. Auditability
- strategy load, arm, disarm, mode changes, settings edits, manual trade actions, and review decisions all retain authenticated operator identity and timestamps

6. Break-glass recovery
- operator can SSH through Tailscale
- operator can use CLI commands when the web dashboard is unavailable

7. Paper-mode remote verification
- remote warmup
- remote arm and disarm
- remote manual entry with broker-side protections
- remote flatten
- remote no-new-entry behavior during degraded states
- remote reconnect review handling

### Suggested Test Matrix

- private access over Tailscale on the operator PC
- paper mode
- observation mode
- live-mode UI gating without actually placing live orders during initial validation

## Documentation Follow-Ups

After implementation starts, update:

- [README.md](/C:/repos/TV_bot_core/README.md)
- [docs/ops/release_checklist.md](/C:/repos/TV_bot_core/docs/ops/release_checklist.md)
- [docs/ops/debugging_guide.md](/C:/repos/TV_bot_core/docs/ops/debugging_guide.md)
- [docs/ops/credential_setup.md](/C:/repos/TV_bot_core/docs/ops/credential_setup.md)
- create a dedicated remote deployment and remote operator runbook in `docs/ops`

## Open Decisions

These are the remaining real decisions after the recommended defaults in this document:

- exact Linux distribution and server provider
- whether Postgres stays on the same server or a private same-region host
- whether a second operator will exist in V1
- whether V1 needs role separation immediately or can stage role enforcement after identity propagation lands
- whether to add Cloudflare public-browser access in the first release or defer it until private-tailnet operations are stable

## Definition Of Done

The remote dashboard access project is not done unless:

- the runtime remains localhost-only in the standard deployment
- the dashboard is remotely reachable through the intended secure ingress path
- WebSockets work through the chosen ingress layer
- authenticated operator identity is preserved for privileged actions
- backend authorization exists for privileged commands or an explicitly temporary limitation is documented and contained
- break-glass CLI and SSH workflows are documented and tested
- remote paper-mode verification passes
- live-mode rollout is held behind explicit final review

## Research Notes

Relevant official references reviewed during this planning pass:

- Tailscale Serve and identity headers: [tailscale.com/docs/features/tailscale-serve](https://tailscale.com/docs/features/tailscale-serve)
- Tailscale access control: [tailscale.com/docs/features/access-control](https://tailscale.com/docs/features/access-control)
- Cloudflare Tunnel overview: [developers.cloudflare.com/tunnel/](https://developers.cloudflare.com/tunnel/)
- Cloudflare Access self-hosted application flow: [developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/self-hosted-public-app/](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/self-hosted-public-app/)
- Cloudflare One-Time PIN: [developers.cloudflare.com/cloudflare-one/integrations/identity-providers/one-time-pin/](https://developers.cloudflare.com/cloudflare-one/integrations/identity-providers/one-time-pin/)
- Cloudflare WebSockets support: [developers.cloudflare.com/network/websockets/](https://developers.cloudflare.com/network/websockets/)
- Caddy automatic HTTPS and reverse proxy basics: [caddyserver.com/docs/quick-starts/https](https://caddyserver.com/docs/quick-starts/https)
