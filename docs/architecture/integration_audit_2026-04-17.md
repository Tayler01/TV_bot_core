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
- The live transport is dataset-scoped, which matches Databento's one-session-per-dataset model.
- The live transport supports replay-style startup via `start(...)` on subscriptions before session start.
- The transport maps the configured slow-reader policy to the Databento client's `warn` behavior, which matches the documented supported modes.
- The runtime already treats Databento system records as first-class state transitions, including replay completion and degraded-feed signaling.

### Tradovate

- The auth flow uses `POST /auth/accesstokenrequest` and bearer auth for subsequent HTTP calls.
- Access-token renewal uses `GET /auth/renewaccesstoken` instead of creating a new session every time.
- The WebSocket auth message uses the documented `authorize\n<requestId>\n\n<token>` format.
- `user/syncrequest` is sent over the user-data WebSocket, not over HTTP.
- Heartbeat handling responds to the server heartbeat frame with `[]`, matching Tradovate's WebSocket guidance.
- Environment routing is already guarded so demo can only back paper routing and live can only back live routing.
- Automated entry paths already set `isAutomated: true` on `placeorder` and `placeoso`.

## Findings

### Fixed In This Pass

#### 1. Automated cancel requests were not marked automated

Tradovate's best-practices and order docs are explicit that automated order activity must carry `isAutomated: true`. The entry paths already did this, but the session-manager convenience path for order cancellation was still emitting `isAutomated: false`.

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

#### 6. Tradovate renewal margin is now aligned with published guidance

Tradovate's partner docs advise renewing about 15 minutes before a 90-minute token expires, with the evaluation-partner overview normalizing that into an 85-minute refresh rhythm. The session manager was previously using a 5-minute default margin.

Status:

- fixed in `crates/broker_tradovate/src/lib.rs`
- default renewal margin is now 15 minutes
- covered with focused session-manager tests for default timing, no-renewal outside the window, explicit near-expiry renewal, and renewal on the order-submission path

#### 7. Databento reconnect and replay recovery are now regression-tested

Databento's live replay guidance says reconnect recovery should resubscribe, replay from the stored timestamp boundary, and only consider the stream caught up once replay completion is observed. The market-data service already carried most of that shape, but it did not explicitly clear `replay_caught_up` on replay-backed reconnects, which could let `trade_ready` remain true across a replay catch-up gap.

Status:

- fixed in `crates/market_data/src/service.rs`
- covered with a `MarketDataService` regression proving replay-backed reconnect keeps `trade_ready` false until a fresh `ReplayCompleted` arrives
- covered with transport-level request assertions showing replay subscriptions continue to use the stored `replay_from` timestamp

### Open Findings And Recommended Follow-Ups

#### 8. Legacy GC example cleanup is now complete

The micro-silver strategy is now the only operator-facing example in `strategies/examples`, and the older GC sample has been reclassified as an internal test fixture.

Status:

- fixed by removing `strategies/examples/gc_momentum_fade_v1.md`
- GC now lives only in `tests/fixtures/strategies/gc_momentum_fade_v1.md`
- user-facing path examples now point at `strategies/examples/micro_silver_elephant_tradovate_v1.md`

## Recommended Fix Order

1. Land the documentation set from this audit and standardize operator setup around it.
2. Keep the automated-cancel fix.
3. Re-run the paper/demo runbook once Tradovate credentials are available.

## Practical Operator Guidance

- Prefer `DATABENTO_API_KEY` for Databento. Keep `TV_BOT__MARKET_DATA__API_KEY` only as a compatibility alias.
- Treat one runtime process as one Tradovate environment. Use demo URLs plus `paper_account_name` for paper/demo work, and live URLs plus `live_account_name` for live work.
- Restart the runtime after changing broker or market-data environment variables.
- Use the standalone CLI and the local control-plane endpoints together during debug sessions instead of relying on the dashboard alone.
