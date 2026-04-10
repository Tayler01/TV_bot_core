# TV Bot Core

Strategy-agnostic futures trading platform foundations for Databento market data, Tradovate execution, strict Markdown strategy specs, and a local control plane.

## Current Status

- Phase 0 through Phase 4 foundations are in place across the Rust workspace.
- Phase 5 is partially started: the runtime command path and control API foundations exist, but the actual local server, dashboard, and usable CLI flows are not finished yet.
- Phase 6 and Phase 7 work are still open: persistence, metrics, reconnect recovery hardening, packaging, and operational docs are not complete.

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
  cli/         Local operator CLI scaffold
  dashboard/   Reserved for the React dashboard
  runtime/     Runtime host scaffold
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
- Front-month symbol resolution in `crates/instrument_resolver`
- Databento transport/session, warmup buffers, replay-aware warmup, and aggregation in `crates/market_data`
- Tradovate auth, sync, and execution primitives in `crates/broker_tradovate`
- Strategy-agnostic risk evaluation in `crates/risk_engine`
- Strategy-agnostic execution planning and broker dispatch in `crates/execution_engine`
- Local control API foundations in `crates/control_api`
- In-memory journal abstraction in `crates/journal`

## Still Required For V1

- Strategy evaluation runtime plus built-in indicators and rule engine
- Runtime host that actually serves local HTTP and WebSocket control endpoints
- CLI command flows beyond startup scaffold
- Dashboard implementation
- Postgres-first persistence with safe SQLite fallback behavior
- Queryable journal persistence, trade history, PnL, fees, commissions, slippage, and latency metrics
- Health/state projection services
- Open-position shutdown and reconnect recovery flows wired end to end
- Paper-mode acceptance campaigns and remaining safety-critical integration coverage

## Local Development

1. Install the Rust toolchain with MSVC support.
2. Copy `config/runtime.example.toml` into a local runtime config if needed.
3. Run targeted serial tests from the workspace root:

```powershell
cargo test -j 1
```

This workspace lives under Windows + OneDrive in the current setup, so a retry with serial tests may be needed if Windows briefly file-locks generated test executables.

## Key Docs

- `AGENTS.md`
- `codex_futures_bot_plan.md`
- `STRATEGY_SPEC.md`
- `V1_ACCEPTANCE_CRITERIA.md`
- `docs/architecture/phase_0_phase_1_foundations.md`
- `docs/architecture/current_status.md`
