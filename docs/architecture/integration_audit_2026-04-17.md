# Integration Audit 2026-04-17

This audit compares the current Databento and Tradovate integration code against current official provider documentation, then rolls the result up into project-level setup, debugging, CLI, and strategy-authoring findings.

## Scope

- Databento authentication, session, replay, and slow-reader behavior
- Tradovate authentication, renewal, environment routing, WebSocket sync, heartbeats, and order payload requirements
- Credential-management ergonomics
- Standalone CLI operator flow
- Debug/observability/operator documentation
- Strategy-authoring documentation and canonical sample guidance

## Official Sources Used

### Databento

- [Live API basics and intraday replay](https://databento.com/docs/api-reference-live/basics/intraday-replay)
- [Slow reader behavior](https://databento.com/docs/api-reference-live/basics/slow-reader-behavior)
- [API keys guide](https://databento.com/docs/portal/api-keys)

### Tradovate

- [Access Token Request](https://partner.tradovate.com/api/rest-api-endpoints/authentication/access-token-request)
- [Auth Overview](https://partner.tradovate.com/overview/quick-setup/auth-overview)
- [Connection Overview](https://partner.tradovate.com/overview/core-concepts/web-sockets/connection-overview)
- [Best Practices](https://partner.tradovate.com/resources/reference/best-practices)
- [API Cheat Sheet](https://partner.tradovate.com/resources/reference/api-cheat-sheet)
- [Environments](https://partner.tradovate.com/resources/reference/environments)
- [Place Order](https://partner.tradovate.com/api/rest-api-endpoints/orders/place-order)
- [Place OSO](https://partner.tradovate.com/api/rest-api-endpoints/orders/place-oso)

## Verified Good

### Databento

- The runtime supports Databento API-key auth and now documents `DATABENTO_API_KEY` as the preferred operator-facing variable.
- The live transport is dataset-scoped, which matches Databento’s one-session-per-dataset model.
- The live transport supports replay-style startup via `start(...)` on subscriptions before session start.
- The transport maps the configured slow-reader policy to the Databento client’s `warn` behavior, which matches the documented supported modes.
- The runtime already treats Databento system records as first-class state transitions, including replay completion and degraded-feed signaling.

### Tradovate

- The auth flow uses `POST /auth/accesstokenrequest` and bearer auth for subsequent HTTP calls.
- Access-token renewal uses `GET /auth/renewaccesstoken` instead of creating a new session every time.
- The WebSocket auth message uses the documented `authorize\n<requestId>\n\n<token>` format.
- `user/syncrequest` is sent over the user-data WebSocket, not over HTTP.
- Heartbeat handling responds to the server heartbeat frame with `[]`, matching Tradovate’s WebSocket guidance.
- Environment routing is already guarded so demo can only back paper routing and live can only back live routing.
- Automated entry paths already set `isAutomated: true` on `placeorder` and `placeoso`.

## Findings

### Fixed In This Pass

#### 1. Automated cancel requests were not marked automated

Tradovate’s best-practices and order docs are explicit that automated order activity must carry `isAutomated: true`. The entry paths already did this, but the session-manager convenience path for order cancellation was still emitting `isAutomated: false`.

Status:

- fixed in `crates/broker_tradovate/src/lib.rs`
- covered with assertion updates in runtime-host and execution-engine tests

#### 2. Credential-management docs were fragmented

The repo already had working config/env support, but the setup story was split between `README.md`, `runtime.example.toml`, and dev-script notes. There was no single source of truth for:

- preferred Databento key naming
- one-runtime-per-Tradovate-environment guidance
- paper vs live account-name routing
- restart requirements after env-var changes
- keeping secrets out of Git-tracked TOML

Status:

- fixed with `docs/ops/credential_setup.md`

#### 3. Standalone CLI usage was under-documented

The CLI was real and fairly complete, but there was no operator-facing guide describing the standalone lifecycle flow, confirmations, or example commands.

Status:

- fixed with `docs/ops/cli_standalone.md`

#### 4. Debug workflow documentation was too thin

The runtime already exposes strong debugging surfaces, but there was no compact guide tying together logs, `/status`, `/readiness`, `/health`, `/history`, chart state, and common local failure modes.

Status:

- fixed with `docs/ops/debugging_guide.md`

#### 5. Strategy authoring docs were too thin

`STRATEGY_SPEC.md` already defines the strict contract, but `strategies/docs/README.md` was only a placeholder. There was no practical authoring guide pointing operators toward the canonical example or summarizing the authoring/validation flow.

Status:

- fixed with `strategies/docs/authoring.md`
- `strategies/docs/README.md` now points to the canonical sample and authoring guide

### Open Findings And Recommended Follow-Ups

#### 6. Tradovate renewal margin is tighter than the docs recommend

Tradovate’s partner docs advise renewing roughly 15 minutes before expiry, and the auth overview examples normalize that into an 85-minute refresh rhythm for 90-minute tokens. Our session manager currently renews 5 minutes before expiry.

Current status:

- functional, but tighter than the published guidance

Recommended follow-up:

- move the default renewal margin from 5 minutes to 15 minutes
- keep the timing configurable for tests and special environments
- add a focused session-manager test for renewal timing against a near-expiry token

#### 7. Databento reconnect recovery should be explicitly regression-tested against replay guidance

Databento’s reconnect guidance is clear: clients should resubscribe and recover via replay using stored `ts_event` plus duplicate filtering strategy where needed. The runtime already uses replay-aware warmup and reconnect-capable session management, but the repo should have a dedicated operator-grade regression around disconnect -> replay catch-up -> resumed readiness.

Recommended follow-up:

- add an integration-style market-data recovery test that simulates disconnect and replay resume
- document the exact expected operator/runtime behavior when replay catch-up is in progress

#### 8. Legacy GC example cleanup is still incomplete

The micro-silver strategy is already the default runtime example in `config/runtime.example.toml`, and it should be treated as the canonical layout example going forward. However, the repository still contains the older GC example and several internal test references to it.

Recommended follow-up:

- migrate the remaining user-facing placeholders and examples to the micro-silver strategy
- either remove `strategies/examples/gc_momentum_fade_v1.md` entirely or reclassify it as a legacy fixture used only by tests

## Recommended Fix Order

1. Land the documentation set from this audit and standardize operator setup around it.
2. Keep the automated-cancel fix.
3. Widen the default Tradovate renewal margin to align with published guidance.
4. Add a dedicated Databento reconnect/replay recovery regression.
5. Finish the legacy GC example cleanup and standardize on the micro-silver strategy as the canonical sample.
6. Re-run the paper/demo runbook once Tradovate credentials are available.

## Practical Operator Guidance

- Prefer `DATABENTO_API_KEY` for Databento. Keep `TV_BOT__MARKET_DATA__API_KEY` only as a compatibility alias.
- Treat one runtime process as one Tradovate environment. Use demo URLs plus `paper_account_name` for paper/demo work, and live URLs plus `live_account_name` for live work.
- Restart the runtime after changing broker or market-data environment variables.
- Use the standalone CLI and the local control-plane endpoints together during debug sessions instead of relying on the dashboard alone.
