# TV Bot Core

Strategy-agnostic futures trading platform foundations for Databento market data, Tradovate execution, strict Markdown strategy specs, and a local control plane.

## Current Status

- Phase 0 through Phase 4 foundations are in place across the Rust workspace.
- Phase 5 is substantially in place: the runtime host serves the local control plane, status/readiness project live broker and market-data state plus shared storage/journal policy status, and the CLI plus dashboard now drive the main operator flows for strategy load/validation, warmup, mode, arm/disarm, manual entry, close/cancel, flatten, events, history, journal, settings, and health; the dashboard redesign is now dark-first with stronger control/monitoring hierarchy, extracted monitoring and operator-workflow components, a dedicated runtime-host/strategy/settings controller split, tighter operator-form layout rules, and a browser-verified responsive pass across `390px`, `768px`, `1024px`, and `1440px` with no page-level horizontal overflow, while final release acceptance is still incomplete.
- Phase 6 now has real Postgres/SQLite persistence adapters, durable journal wiring, event-sourced runtime projection, live runtime/broker trading-history ingestion, runtime-collected trade-latency metrics, host-level health supervision, sampled CPU/memory runtime-resource projection, queryable history surfaces through the host/CLI/dashboard, and broad host-level paper acceptance coverage for entry, scale-in, flatten, operator/degraded no-new-entry gating, and startup/reconnect review safety flows.
- Phase 7 now has a checked-in GitHub Actions cross-platform CI matrix, operator runbooks for paper verification, storage fallback override handling, reconnect/shutdown safety review handling, and release verification, plus cross-platform packaging scripts, while final hands-on release validation is still incomplete.

The current implementation status review lives in `docs/architecture/current_status.md`.

## Safety Model

- No trading without explicit arming
- Runtime mode is always explicit
- Strategy Markdown is authoring input only; trading uses compiled internal strategy objects
- Broker-side protections are preferred and broker-required protections surface override gates
- Important actions and state transitions must be journaled

## Workspace Layout

```text
apps/
  cli/         Local operator CLI
  dashboard/   React operator dashboard
  runtime/     Runtime host
crates/
  broker_tradovate/
  config/
  control_api/
  core_types/
  execution_engine/
  health/
  indicators/
  instrument_resolver/
  journal/
  market_data/
  metrics/
  persistence/
  risk_engine/
  rule_engine/
  runtime_kernel/
  state_store/
  strategy_loader/
  strategy_runtime/
config/
  runtime.example.toml
docs/
  api/
  architecture/
  ops/
strategies/
  docs/
  examples/
  schemas/
tests/
  integration/
  paper/
  unit/
```

## Implemented Foundations

- Shared domain contracts in `crates/core_types`
- Environment + TOML config loading in `crates/config`
- Runtime mode, warmup, readiness, arming, and audited command orchestration in `crates/runtime_kernel`
- Strict strategy parsing, validation, and compilation in `crates/strategy_loader`
- Front-month symbol resolution plus provider-specific Databento raw-symbol and Tradovate routing-symbol mapping in `crates/instrument_resolver`
- Databento transport/session, warmup buffers, replay-aware warmup, and aggregation in `crates/market_data`
- Tradovate auth, sync, and execution primitives in `crates/broker_tradovate`
- Strategy-agnostic risk evaluation in `crates/risk_engine`
- Strategy-agnostic execution planning and broker dispatch in `crates/execution_engine`
- Local control API foundations in `crates/control_api`
- In-memory journal abstraction in `crates/journal`
- Postgres-first persistence planning plus durable event/health/latency/trading-history storage adapters in `crates/persistence`
- Durable Postgres/SQLite journal + storage backend selection in `crates/persistence` and `crates/journal`
- Event-sourced runtime state projection plus queryable trading-history state in `crates/state_store`
- Runtime-collected trade-path latency persistence and snapshots in `crates/metrics`
- Runtime health supervision, durable health snapshots, sampled CPU/memory runtime-resource telemetry, and health-event publication in `crates/health`
- Runtime host lifecycle/state handling in `apps/runtime`
- Runtime-host `/history` projection and background broker-history sync in `apps/runtime`
- Runtime-host system-health and latency projection through `/health`, `/status`, and WebSocket events in `apps/runtime`
- Runtime-host `/readiness` release-gate coverage for broker/account/data/storage surfacing and fallback override warnings in `apps/runtime`
- Dashboard operator surfaces for lifecycle control, strategy upload/validation, settings, manual entry, close/cancel, explicit real-time PnL and per-trade drill-downs, journal/history, event streaming, and health in `apps/dashboard`
- Host-level paper acceptance coverage for explicit arm enforcement, repeated manual entries, operator/degraded no-new-entry safety gates, scale-in, flatten, startup/reconnect review decisions, and a broader paper release-sweep regression in `apps/runtime`
- CLI launch, lifecycle, reconnect-review, shutdown-review, and history commands in `apps/cli`

## Still Required For V1

- Final dashboard production sign-off, remaining component/layout cleanup, and operator ergonomics
- Final cross-platform paper/demo verification passes and remaining safety-critical integration hardening
- Cross-platform packaging and final release hardening

## Local Development

1. Install the Rust toolchain with MSVC support.
2. Copy `config/runtime.example.toml` into a local runtime config if needed.
3. Run targeted serial tests from the workspace root:

```powershell
cargo test -j 1
```

This workspace is now the primary local checkout, but Windows may still briefly file-lock generated test executables. If that happens, retrying targeted serial tests is usually enough.

For a quick local smoke test on Windows:

- start the runtime host with `.\target\release\tv-bot-runtime.exe .\config\runtime.local.toml`
- start the dashboard dev server from `apps/dashboard` with `npm run dev`
- open `http://127.0.0.1:4173`
- use `http://127.0.0.1:8080/status` or the root API landing page at `http://127.0.0.1:8080/` instead of expecting a UI on the runtime port

For a Databento-only observation smoke test on Windows, set `TV_BOT__MARKET_DATA__API_KEY` in the current PowerShell session and run:

```powershell
.\scripts\dev\start_databento_observation.ps1 -StartDashboard
```

That observation path now starts warmup with a strategy-driven Databento historical replay window instead of waiting only on live bars, so multi-timeframe warmup should catch up much faster when historical data is available.

For the release-hardening path, the repository now includes:

- `.github/workflows/ci.yml` for Rust workspace tests on Windows, Linux, and macOS plus dashboard build/test checks
- `scripts/package_release.ps1` and `scripts/package_release.sh` for cross-platform release bundle creation
- `docs/ops/paper_demo_verification.md`
- `docs/ops/storage_fallback_override.md`
- `docs/ops/reconnect_and_shutdown_review.md`
- `docs/ops/release_checklist.md`

## Key Docs

- `AGENTS.md`
- `codex_futures_bot_plan.md`
- `STRATEGY_SPEC.md`
- `V1_ACCEPTANCE_CRITERIA.md`
- `docs/architecture/phase_0_phase_1_foundations.md`
- `docs/architecture/current_status.md`
- `docs/architecture/dashboard_production_ui_plan.md`
- `docs/ops/README.md`
