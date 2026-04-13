# Current Status Review

This review compares the current repository state against `AGENTS.md`, `codex_futures_bot_plan.md`, `STRATEGY_SPEC.md`, and `V1_ACCEPTANCE_CRITERIA.md`.

## Summary

- Phase 0 foundations are complete.
- Phase 1 foundations are complete.
- Phase 2 foundations are substantially in place.
- Phase 3 foundations are substantially in place.
- Phase 4 foundations are substantially in place.
- Phase 5 foundations are substantially in place, with the runtime host/control-plane server and CLI control surface implemented and the dashboard now partially wired for operator overview, lifecycle control, strategy-library upload/validation, event streaming, reconnect/shutdown safety actions, manual entry, persisted journal visibility, and close/cancel plus open-order/fill and recent-trade views, while the remaining dashboard polish is still incomplete.
- Phase 6 foundations are now in place, including durable trading-history record storage, live runtime/broker write-path wiring, queryable history projection through the control plane, runtime-collected latency persistence, host-level health supervision with sampled runtime-resource telemetry, and safety-critical shutdown/reconnect review control flows with explicit acceptance coverage for paper reconnect `close_position` and `reattach_bot_management` plus signal-time shutdown blocking.
- Phase 7 is mostly open.

V1 is not release-ready yet because the dashboard and the remaining full end-to-end paper-trading acceptance campaign are still unfinished.

## Implemented Through The Current Pass

- Strict strategy parsing, validation, and compilation
- Runtime mode, readiness, warmup, arming, and audited command orchestration
- Front-month resolution and symbol mapping, now backed by built-in supported-market contract chains during strategy load
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
- Targeted acceptance coverage for execution-engine no-new-entry blocking, paper-account scale-in dispatch, paper reconnect-review `close_position` and `reattach_bot_management` flows through the runtime host, and signal-time shutdown review blocking
- Dashboard overview and control-center slices in `apps/dashboard`, including host-backed lifecycle commands, strategy-library upload/browsing and strict strategy validation through `/strategies`, `/strategies/upload`, and `/strategies/validate`, a local `/events` operator feed, dashboard-driven reconnect/shutdown review actions, manual entry, close-position/cancel-working-order controls, and richer `/history` plus `/journal` trade/order/fill/operator projections

## Must Finish Before Advancing Deeper Into Phase 5 And Phase 6

1. Finish the remaining full end-to-end paper-mode acceptance campaign from `V1_ACCEPTANCE_CRITERIA.md`, especially the operator and dashboard-visible flows that still need host-surface validation together rather than crate-level targeting.
2. Build out the remaining dashboard operator workflows on top of the now-real host surfaces for status, readiness, commands, events, history, and health.
3. Expand the last dashboard-facing operational views and deeper operator drill-downs on top of the now-sampled health and metrics pipeline.

## Remaining Work By Phase

### Phase 5

- Dashboard v1 in `apps/dashboard`
- Manual operator flows end to end through the live runtime host
- Final dashboard control-center polish and operator ergonomics

### Phase 6

- Deeper dashboard-facing history and journal drill-downs on top of the durable Postgres/SQLite backends

### Phase 7

- Cross-platform packaging
- Hardening pass
- Final reconnect hardening sweep inside the remaining paper-trading campaign, especially the paper `leave_broker_protected` operator path and broader restart/reconnect regression coverage beyond the current host-level acceptance set
- Paper-trading test campaigns
- Operational docs and runbooks

## Acceptance Gaps Still Open

- Strategy-system acceptance is substantially met for V1 built-in rule/runtime behavior, and host-backed strategy library upload, validation, and load workflows are now exposed to the dashboard.
- Runtime-host acceptance is substantially met at the transport layer, and broker plus market-data plus active storage/journal backend state are now surfaced through status/readiness while the richer trading-history projection is available through `/history` and the CLI.
- Runtime-host observability acceptance is substantially met for persisted latency/health snapshots, host `/health` and `/status` projection, and operator-facing conflict/precondition mapping of safety-blocked execution paths.
- Restart/reconnect and shutdown-with-open-position acceptance are now substantially met at the host and execution layers, including explicit paper reconnect `close_position` and `reattach_bot_management` routing plus signal-time shutdown blocking coverage.
- Dashboard acceptance is not met yet, but the local overview, lifecycle controls, strategy-library upload/validation surface, event feed, reconnect/shutdown safety controls, manual entry, persisted journal visibility, and close/cancel plus open-order/fill and recent-trade surfaces are now wired through the host.
- CLI acceptance is substantially met for local operator control flow, with broker account/sync projection, live market-data status, shared storage/journal policy status, reconnect/shutdown review controls, and trading-history inspection now surfaced through the runtime host.
- Persistence acceptance is substantially met for durable Postgres/SQLite adapters, fallback reporting, trading-history stores, live runtime/broker record ingestion, queryable history projection, and persisted latency/health snapshots.
- Metrics acceptance is substantially met for V1 host and CLI surfaces: runtime-collected trade-path latency, persisted health snapshots, sampled CPU/memory runtime-resource telemetry, dashboard event streaming, and open-order/fill plus recent-trade/journal operator views are now surfaced, while deeper drill-down polish is still incomplete.
- Full paper-trading acceptance is not met yet.
- Final release gate items for the dashboard and full paper-mode acceptance are not met yet.
