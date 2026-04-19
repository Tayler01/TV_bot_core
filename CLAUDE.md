# CLAUDE.md

This file helps Claude Code work effectively in `TV_bot_core`.

`AGENTS.md` is the primary repository contract. If this file and `AGENTS.md` ever differ, follow `AGENTS.md`.

## What This Repo Is

`TV_bot_core` is a strategy-agnostic futures trading platform with:

- Databento for market data
- Tradovate for execution, sync, fills, and positions
- Postgres as primary persistence
- SQLite as fallback-only storage
- A local HTTP + WebSocket control plane
- A React dashboard and a terminal CLI
- Strict Markdown strategy files that compile into validated internal runtime specs

The system is intentionally built around explicit safety, auditability, and clear module boundaries.

## Non-Negotiable Rules

- Never add strategy-specific logic to broker, execution, risk, persistence, dashboard, or control-plane modules.
- Never trade without explicit arming.
- Never make runtime mode ambiguous.
- Prefer broker-side protections when supported.
- Do not let the frontend become the source of truth.
- Important actions and state transitions must be journaled or logged.
- If a change is safety-critical, add or update tests.

## Architecture Map

Use these boundaries strictly:

- `crates/strategy_loader`: parses, validates, and compiles strict strategy Markdown
- `crates/strategy_runtime`: evaluates compiled strategy logic and emits intents
- `crates/risk_engine`: accepts/rejects intents and determines safe sizing
- `crates/execution_engine`: translates approved intents into broker-native execution flows
- `crates/broker_tradovate`: Tradovate auth, REST, websocket sync, reconciliation, execution transport
- `crates/market_data`: Databento transport, replay, warmup, aggregation, health
- `crates/control_api`: local HTTP/WebSocket contracts used by dashboard/CLI
- `crates/persistence`, `crates/journal`, `crates/state_store`: durable storage, event recording, projections
- `apps/runtime`: runtime host that wires everything together
- `apps/dashboard`: React UI that talks only to the local control API
- `apps/cli`: terminal operator console

## Files Worth Reading First

- `AGENTS.md`
- `README.md`
- `STRATEGY_SPEC.md`
- `docs/architecture/current_status.md`
- `Cargo.toml`

If the task is in a specific area, read the crate/app boundary code before editing across modules.

## Safe Working Habits

- Keep the execution core strategy-agnostic.
- Be explicit about reconnect, degraded-state, and failure behavior.
- Avoid hidden coupling between runtime, broker, market data, and UI.
- Treat Postgres outage and SQLite fallback as operator-visible safety states, not silent implementation details.
- Preserve audit data in journal payloads when changing commands, lifecycle events, or auth context.

## Commands

Rust workspace:

```powershell
cargo test --workspace --target-dir target_verify_claude
```

Focused crates:

```powershell
cargo test -p tv-bot-broker-tradovate
cargo test -p tv-bot-market-data
cargo test -p tv-bot-strategy-runtime
cargo test -p tv-bot-execution-engine
cargo test -p tv-bot-runtime --bin tv-bot-runtime
cargo test -p tv-bot-cli
```

Frontend:

```powershell
npm test -- --run
npm run build
```

Formatting:

```powershell
cargo fmt
```

## Testing Expectations

Do not treat a change as done without tests where they matter.

Minimum expectations:

- Unit tests for parsing, validation, readiness, sizing, risk, mode transitions, and symbol resolution
- Integration coverage for load -> warmup -> ready, arm/disarm, manual control flow, reconnect, degraded storage, and review/override paths
- Paper-mode coverage for entry, scaling, flatten, degraded no-new-entry gating, and reconnect/startup review

If you touch:

- broker sync or transport behavior: add broker transport/session tests
- market data or replay behavior: add Databento transport/session tests
- runtime host/control API contracts: add host tests
- dashboard or CLI state behavior: add UI/console tests

## Known Repo Conventions

- The repo is Windows-friendly and cross-platform; avoid shell-specific assumptions in product code.
- Verification target directories like `target_verify_*` are local artifacts and should not be committed.
- The dashboard must consume the runtime host only, never provider APIs directly.
- Strategies are authoring inputs only; runtime trading uses compiled internal objects.
- The platform supports one loaded strategy at a time, but that strategy may permit multiple legs/scaling.

## Good Task Strategy

1. Identify the boundary you are working inside.
2. Check existing tests in that crate/app first.
3. Make the smallest change that preserves safety and auditability.
4. Add regression coverage for the exact failure mode.
5. Run focused tests first, then a broader verification pass if the change crosses module contracts.

## When Unsure

Prefer this order:

1. Safety
2. Auditability
3. Clear boundaries
4. Explicit configuration
5. Broker-side protection
6. Smaller, testable changes
