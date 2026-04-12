# Current Status Review

This review compares the current repository state against `AGENTS.md`, `codex_futures_bot_plan.md`, `STRATEGY_SPEC.md`, and `V1_ACCEPTANCE_CRITERIA.md`.

## Summary

- Phase 0 foundations are complete.
- Phase 1 foundations are complete.
- Phase 2 foundations are substantially in place.
- Phase 3 foundations are substantially in place.
- Phase 4 foundations are substantially in place.
- Phase 5 foundations are substantially in place, with the runtime host/control-plane server and CLI control surface implemented, while the dashboard remains incomplete.
- Phase 6 foundations are now in place, including durable trading-history record storage, live runtime/broker write-path wiring, queryable history projection through the control plane, runtime-collected latency persistence, host-level health supervision with sampled runtime-resource telemetry, and safety-critical shutdown/reconnect review control flows with explicit acceptance coverage for reconnect-close and signal-time shutdown blocking.
- Phase 7 is mostly open.

V1 is not release-ready yet because the dashboard and the remaining full end-to-end paper-trading acceptance campaign are still unfinished.

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
- Phase 6 runtime-collected trade-path latency persistence with deterministic ordering/error checks in `crates/metrics`
- Phase 6 health supervision in `crates/health`, including durable health snapshots, runtime error counting, DB-write latency tracking, and WebSocket health publication through the runtime host
- Runtime-host `/health` and `/status` projection of latest system-health and trade-latency snapshots, with server-side degraded-entry blocking now enforced before command dispatch
- Cross-platform sampled CPU and memory telemetry now flows through the persisted health pipeline, `/health`, `/status`, and the CLI operator view
- Safety-critical HTTP mapping for execution-planning failures so blocked/manual entry paths return operator-facing conflicts or precondition responses instead of internal-server-error responses
- Shutdown-with-open-position safety flow through the runtime host, including signal-time blocking, explicit flatten-first or leave-broker-protected decisions, and status projection of pending shutdown review state
- Reconnect/open-position recovery review flow through the runtime host and CLI, including explicit leave-broker-protected or reattach acknowledgement and close-position dispatch through the existing audited flatten path
- Targeted acceptance coverage for execution-engine no-new-entry blocking, paper-account scale-in dispatch, reconnect-review close-position flatten dispatch, and signal-time shutdown review blocking

## Must Finish Before Advancing Deeper Into Phase 5 And Phase 6

1. Finish the remaining full end-to-end paper-mode acceptance campaign from `V1_ACCEPTANCE_CRITERIA.md`, especially the operator and dashboard-visible flows that still need host-surface validation together rather than crate-level targeting.
2. Build the dashboard against the now-real host surfaces for status, readiness, commands, events, history, and health.
3. Expand dashboard-facing operational views on top of the now-sampled health and metrics pipeline.

## Remaining Work By Phase

### Phase 5

- Dashboard v1 in `apps/dashboard`
- Manual operator flows end to end through the live runtime host
- Grouped pre-arm readiness screen surfaced through the control plane with live-backed dependency status

### Phase 6

- Dashboard-facing history and journal views on top of the durable Postgres/SQLite backends

### Phase 7

- Cross-platform packaging
- Hardening pass
- Paper-trading test campaigns
- Operational docs and runbooks

## Acceptance Gaps Still Open

- Strategy-system acceptance is substantially met for V1 built-in rule/runtime behavior, but upload/library UX still needs to be wired through the host surfaces.
- Runtime-host acceptance is substantially met at the transport layer, and broker plus market-data plus active storage/journal backend state are now surfaced through status/readiness while the richer trading-history projection is available through `/history` and the CLI.
- Runtime-host observability acceptance is substantially met for persisted latency/health snapshots, host `/health` and `/status` projection, and operator-facing conflict/precondition mapping of safety-blocked execution paths.
- Restart/reconnect and shutdown-with-open-position acceptance are now substantially met at the host and execution layers, including explicit close-position review routing and signal-time shutdown blocking coverage.
- Dashboard acceptance is not met yet.
- CLI acceptance is substantially met for local operator control flow, with broker account/sync projection, live market-data status, shared storage/journal policy status, reconnect/shutdown review controls, and trading-history inspection now surfaced through the runtime host.
- Persistence acceptance is substantially met for durable Postgres/SQLite adapters, fallback reporting, trading-history stores, live runtime/broker record ingestion, queryable history projection, and persisted latency/health snapshots.
- Metrics acceptance is substantially met for V1 host and CLI surfaces: runtime-collected trade-path latency, persisted health snapshots, and sampled CPU/memory runtime-resource telemetry are now persisted and surfaced, while the richer dashboard-facing operator views are still incomplete.
- Full paper-trading acceptance is not met yet.
- Final release gate items for the dashboard and full paper-mode acceptance are not met yet.
