# Current Status Review

This review compares the current repository state against `AGENTS.md`, `codex_futures_bot_plan.md`, `STRATEGY_SPEC.md`, and `V1_ACCEPTANCE_CRITERIA.md`.

## Summary

- Phase 0 foundations are complete.
- Phase 1 foundations are complete.
- Phase 2 foundations are substantially in place.
- Phase 3 foundations are substantially in place.
- Phase 4 foundations are substantially in place.
- Phase 5 foundations are substantially in place, with the runtime host/control-plane server and CLI control surface implemented, while the dashboard remains incomplete.
- Phase 6 foundations are now in place, including durable trading-history record storage, live runtime/broker write-path wiring, and queryable history projection through the control plane, but health supervision and runtime metrics collection remain incomplete.
- Phase 7 is mostly open.

V1 is not release-ready yet because persistence, metrics, the dashboard, and several acceptance flows are still unfinished.

## Implemented Through The Current Pass

- Strict strategy parsing, validation, and compilation
- Runtime mode, readiness, warmup, arming, and audited command orchestration
- Front-month resolution and symbol mapping
- Databento session, reconnect, warmup, and aggregation foundations
- Tradovate auth, sync, account routing, and execution primitives
- Risk evaluation and execution dispatch boundaries
- Strategy evaluation compilation and built-in rule execution
- Local control API command normalization, HTTP-style handler mapping, and WebSocket-style event publication
- Runtime-host HTTP and WebSocket server binding around the audited local control API
- CLI launch and lifecycle control flow for runtime mode, strategy load, warmup, arm/disarm, flatten, status, and readiness inspection
- Runtime-host market-data projection that refreshes the Databento service in the background and syncs warmup state back into readiness/status responses
- Phase 6 persistence runtime selection with real Postgres and SQLite durable adapters, explicit fallback activation reporting, and shared event/health/latency storage contracts
- Durable journal wiring through a persistence-backed projecting journal that keeps an event-sourced state store in sync with appended records
- Durable trading-history record stores for strategy runs, orders, fills, positions, PnL snapshots, and trade summaries across in-memory, SQLite, and Postgres backends
- Queryable trading-history projection state in `crates/state_store` for active runs, working orders, open positions, open trades, and aggregate gross/net PnL plus fees, commissions, and slippage
- Runtime-host history wiring that continuously syncs broker snapshots into the durable trading-history stores and exposes `/history` plus CLI history inspection for operator tooling and the future dashboard
- Phase 6 latency metric calculations for the trade path with deterministic ordering/error checks in `crates/metrics`

## Must Finish Before Advancing Deeper Into Phase 5 And Phase 6

1. Extend the new metrics foundation into runtime-collected latency/health pipelines and add health supervision in `crates/metrics` and `crates/health`.
2. Add the missing safety-critical integration and paper-mode tests from `V1_ACCEPTANCE_CRITERIA.md`.
3. Build the dashboard against the now-real host surfaces for status, readiness, commands, events, and history.

## Remaining Work By Phase

### Phase 5

- Dashboard v1 in `apps/dashboard`
- Manual operator flows end to end through the live runtime host
- Grouped pre-arm readiness screen surfaced through the control plane with live-backed dependency status

### Phase 6

- Dashboard-facing history and journal views on top of the durable Postgres/SQLite backends
- Runtime-collected latency metrics across the trade path
- Reconnect and open-position recovery flows wired end to end

### Phase 7

- Cross-platform packaging
- Hardening pass
- Paper-trading test campaigns
- Operational docs and runbooks

## Acceptance Gaps Still Open

- Strategy-system acceptance is substantially met for V1 built-in rule/runtime behavior, but upload/library UX still needs to be wired through the host surfaces.
- Runtime-host acceptance is substantially met at the transport layer, and broker plus market-data plus active storage/journal backend state are now surfaced through status/readiness while the richer trading-history projection is available through `/history` and the CLI.
- Dashboard acceptance is not met yet.
- CLI acceptance is substantially met for local operator control flow, with broker account/sync projection, live market-data status, shared storage/journal policy status, and trading-history inspection now surfaced through the runtime host.
- Persistence acceptance is partially met: durable Postgres/SQLite adapters, fallback reporting, trading-history stores, live runtime/broker record ingestion, and queryable history projection are now in place, but full health/reporting surfaces are still incomplete.
- Metrics acceptance is not met yet, but the latency-calculation foundation is now in place.
- Full paper-trading acceptance is not met yet.
- Final release gate items for reconnect/open-position recovery and Postgres-first storage are not met yet.
