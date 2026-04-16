# Current Status Review

This review compares the current repository state against `AGENTS.md`, `codex_futures_bot_plan.md`, `STRATEGY_SPEC.md`, and `V1_ACCEPTANCE_CRITERIA.md`.

## Summary

- Phase 0 foundations are complete.
- Phase 1 foundations are complete.
- Phase 2 foundations are substantially in place.
- Phase 3 foundations are substantially in place.
- Phase 4 foundations are substantially in place.
- Phase 5 foundations are substantially in place, with the runtime host/control-plane server and CLI control surface implemented and the dashboard now partially wired for operator overview, lifecycle control, explicit no-new-entry gating, host-backed runtime settings editing, strategy-library upload/validation, event streaming, reconnect/shutdown safety actions, manual entry, persisted journal visibility, close/cancel plus open-order/fill and trade-ledger views, deeper PnL, latency, and host-health drill-downs, and a real live contract-chart surface backed only by the runtime host chart control plane; the dark-first shell/status rail, grouped control center, lower monitoring/audit redesign, denser operator-control ergonomics pass, operator-workflow plus runtime-host/strategy/settings controller extraction, tighter form-grid hardening, and a browser-verified responsive sweep across `390px`, `768px`, `1024px`, and `1440px` with no page-level horizontal overflow are now in place, while final dashboard sign-off is still incomplete and tracked in `docs/architecture/dashboard_live_chart_plan.md` plus `docs/architecture/dashboard_production_ui_plan.md`.
- Phase 6 foundations are now in place, including durable trading-history record storage, live runtime/broker write-path wiring, queryable history projection through the control plane, runtime-collected latency persistence, host-level health supervision with sampled runtime-resource telemetry, and safety-critical shutdown/reconnect review control flows with explicit acceptance coverage for paper startup and reconnect review gating across position-only, working-orders-only, and mixed-exposure scenarios plus `close_position`, `leave_broker_protected`, and `reattach_bot_management`, as well as signal-time shutdown blocking, explicit paper arm-before-trade enforcement, `/readiness` release-gate coverage for broker/account/data/storage surfacing plus fallback override warnings, and a broader paper release-sweep regression that combines repeated paper-session gating with startup-review resolution plus cancel/close operator actions through the runtime host.
- Phase 7 is now partially in place with a checked-in GitHub Actions cross-platform CI matrix, concrete V1 operator runbooks, and cross-platform release-bundle packaging scripts, but final hands-on release verification is still open and the current local sign-off remains blocked by unavailable Tradovate demo credentials for the paper/demo runbook.

V1 is not release-ready yet because final dashboard sign-off, the remaining paper/demo release verification pass, and the last release-checklist walkthrough are still unfinished. The current candidate review for those open items lives in `docs/ops/release_readiness_review.md`.

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
- Targeted acceptance coverage for execution-engine no-new-entry blocking, paper-account scale-in dispatch, startup/reconnect review detection that blocks paper arming/new entry across position-only, working-orders-only, and mixed-exposure scenarios until operator resolution, the paper startup/reconnect review decision trio through the runtime host, and signal-time shutdown review blocking
- Dashboard overview and control-center slices in `apps/dashboard`, including host-backed lifecycle commands, explicit dashboard-driven no-new-entry gating, config/env-backed runtime settings editing through `/settings`, strategy-library upload/browsing and strict strategy validation through `/strategies`, `/strategies/upload`, and `/strategies/validate`, a local `/events` operator feed, dashboard-driven reconnect/shutdown review actions, manual entry, direct flatten/current-position close and cancel-working-order controls, richer `/history` plus `/journal` trade/order/fill/operator projections, an explicit real-time PnL chart plus per-trade PnL cards and trade-ledger views, deeper latency/health audit drill-downs, and a live contract chart that consumes `/chart/config`, `/chart/snapshot`, `/chart/history`, and `/chart/stream` through dedicated frontend chart/controller modules
- Runtime-host chart control-plane foundation in `apps/runtime`, including `/chart/config`, `/chart/snapshot`, `/chart/history`, and `/chart/stream`, runtime-backed timeframe negotiation from the loaded strategy, in-memory candle pagination from market-data buffers, and active position plus working-order plus recent-fill overlay projection for the currently loaded contract
- Broader host-level paper regression coverage proving explicit arm-before-trade enforcement, repeated paper manual-entry dispatch, operator entry gating, degraded-feed no-new-entry blocking, recovery back to healthy repeated testing, and a release-sweep path that combines startup-review resolution with cancel/close operator actions through the same runtime-host path
- Host-level readiness-route coverage proving the operator-facing `/readiness` surface reports broker account selection, healthy market-data and broker-sync state, and primary-storage fallback override warnings together for paper mode

## Must Finish Before Advancing Deeper Into Phase 5 And Phase 6

1. Finish the remaining dashboard chart polish on top of the live contract-chart renderer tracked in `docs/architecture/dashboard_live_chart_plan.md`.
2. Build out the remaining dashboard operator workflow polish on top of the now-real host surfaces for status, readiness, commands, events, history, and health.
3. Expand the last dashboard-facing operational views and deeper operator drill-downs on top of the now-sampled health and metrics pipeline.
4. Run the remaining final release verification passes on top of the new CI matrix, packaging scripts, and operator runbooks.

## Remaining Work By Phase

### Phase 5

- Dashboard v1 in `apps/dashboard`
- Final live contract-chart polish in `apps/dashboard`, including fit/live-follow ergonomics and richer working-order overlay fidelity once the host exposes precise working prices
- Manual operator flows end to end through the live runtime host
- Final dashboard control-center polish, responsive hardening, and operator ergonomics

### Phase 6

- Final dashboard-facing performance and audit polish on top of the durable Postgres/SQLite backends

### Phase 7

- Final hands-on paper-trading verification passes against the release candidate
- Final release verification passes and sign-off

## Acceptance Gaps Still Open

- Strategy-system acceptance is substantially met for V1 built-in rule/runtime behavior, and host-backed strategy library upload, validation, and load workflows are now exposed to the dashboard.
- Runtime-host acceptance is substantially met at the transport layer, and broker plus market-data plus active storage/journal backend state are now surfaced through status/readiness while the richer trading-history projection is available through `/history` and the CLI, with direct host coverage now asserting the `/readiness` release-gate view for account/data/storage health and override warnings.
- Runtime-host observability acceptance is substantially met for persisted latency/health snapshots, host `/health` and `/status` projection, and operator-facing conflict/precondition mapping of safety-blocked execution paths.
- Restart/reconnect and shutdown-with-open-position acceptance are now substantially met at the host and execution layers, including startup/reconnect review-required detection that blocks paper arming/new entry across position-only, working-orders-only, and mixed-exposure scenarios until operator resolution, explicit paper startup/reconnect `close_position`, `leave_broker_protected`, and `reattach_bot_management` routing, and signal-time shutdown blocking coverage.
- Dashboard acceptance is not met yet, but the local overview, lifecycle controls, explicit no-new-entry gating, config/env-backed runtime settings editing, strategy-library upload/validation surface, event feed, reconnect/shutdown safety controls, manual entry, persisted journal visibility, close/cancel plus open-order/fill, explicit real-time PnL/per-trade surfaces, richer latency/health drill-downs, the real monitoring plus operator-workflow component-decomposition passes, and the runtime-host-backed live contract chart are now wired through the host, with the polling/event/command controller split into dedicated runtime-host, strategy-workflow, settings-workflow, and chart hooks, the runtime-view-model shaping moved into dedicated projection/adapter modules, and the latest browser QA pass clearing page-level overflow at `390px`, `768px`, `1024px`, and `1440px`, while final chart/operator polish, final production sign-off, and release verification are still tracked in `docs/architecture/dashboard_live_chart_plan.md` and `docs/architecture/dashboard_production_ui_plan.md`.
- CLI acceptance is substantially met for local operator control flow, with broker account/sync projection, live market-data status, shared storage/journal policy status, reconnect/shutdown review controls, and trading-history inspection now surfaced through the runtime host.
- Persistence acceptance is substantially met for durable Postgres/SQLite adapters, fallback reporting, trading-history stores, live runtime/broker record ingestion, queryable history projection, and persisted latency/health snapshots.
- Metrics acceptance is substantially met for V1 host and CLI surfaces: runtime-collected trade-path latency, persisted health snapshots, sampled CPU/memory runtime-resource telemetry, dashboard event streaming, and open-order/fill, trade-ledger, journal, PnL, and latency/health operator views are now surfaced, while final dashboard polish is still incomplete.
- Full paper-trading acceptance is substantially met at the host/operator layer, the broader host-level regression/release sweep is now in place, and the repository now has release runbooks plus a cross-platform CI matrix and packaging scripts, but final hands-on release verification is not complete yet.
- Final release gate items for the dashboard and full paper-mode acceptance are not met yet.
