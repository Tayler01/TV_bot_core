# TV Bot Core

Strategy-agnostic futures trading platform foundations for Databento market data, Tradovate execution, strict Markdown strategy specs, and a local control plane.

## Current Status

- Phase 0 through Phase 4 foundations are in place across the Rust workspace.
- Phase 5 is substantially in place: the runtime host serves the local control plane, status/readiness project live broker and market-data state plus shared storage/journal policy status, and the CLI plus dashboard now drive the main operator flows for strategy load/validation, warmup, mode, arm/disarm, manual entry, close/cancel, flatten, events, history, journal, settings, and health; the dashboard redesign is now dark-first with stronger control/monitoring hierarchy, extracted monitoring and operator-workflow components, a dedicated runtime-host/strategy/settings controller split, tighter operator-form layout rules, and a browser-verified responsive pass across `390px`, `768px`, `1024px`, and `1440px` with no page-level horizontal overflow, and the dashboard now includes a real live contract chart backed only by the runtime host chart control plane with timeframe switching, buffered history paging, fit/live-follow controls, active-position context, exact working-order price overlays, chart-side runtime alert banners, operator readout strips, viewport-aware chart-history bootstrapping, a tighter chart toolbar/readout/utility strip, and a chart-dominant workspace shell where the header now behaves like a compact utility strip, the chart stage holds more width, the left rail now carries the lower-frequency mode and entry-gate tools, the right rail stays focused on posture/ticket/exit actions, strategy/setup workflows move into a flatter lower detail dock, the latest polish pass trims alert copy, toolbar labels, and utility-header wording further so the workspace scans faster above the fold, and the newest refinement trims duplicated chart context out of the left rail and flattens the lower dock again so the chart reads more cleanly as the primary surface.
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
- Runtime-host chart control-plane endpoints and dedicated chart WebSocket stream in `apps/runtime`
- Runtime-host system-health and latency projection through `/health`, `/status`, and WebSocket events in `apps/runtime`
- Runtime-host `/readiness` release-gate coverage for broker/account/data/storage surfacing and fallback override warnings in `apps/runtime`
- Dashboard operator surfaces for lifecycle control, strategy upload/validation, settings, manual entry, close/cancel, explicit real-time PnL and per-trade drill-downs, journal/history, event streaming, health, and a runtime-host-backed live contract chart in `apps/dashboard`
- Host-level paper acceptance coverage for explicit arm enforcement, repeated manual entries, operator/degraded no-new-entry safety gates, scale-in, flatten, startup/reconnect review decisions, and a broader paper release-sweep regression in `apps/runtime`
- CLI launch, lifecycle, reconnect-review, shutdown-review, and history commands in `apps/cli`

## Still Required For V1

- Final chart-first dashboard/operator redesign and production sign-off around the now-real live chart module, now that the shell reset, utility-header pass, denser toolbar rails, fuller first-open chart, tighter chart toolbar/readout/utility strip, thinner rail chrome, flatter lower dock, and more minimal chart frame are in place and the remaining work is final product polish and sign-off
- Final cross-platform paper/demo verification passes and remaining safety-critical integration hardening
- Final release checklist walkthrough and sign-off

## Local Development

1. Install the Rust toolchain with MSVC support.
2. Copy `config/runtime.example.toml` into a local runtime config if needed.
3. Run targeted serial tests from the workspace root:

```powershell
cargo test -j 1
```

## API Key And Credential Setup

The runtime is designed to read secrets from environment variables instead of storing them in Git-tracked config files.
At minimum, you usually need:

- `DATABENTO_API_KEY` or `TV_BOT__MARKET_DATA__API_KEY` for Databento
- `TV_BOT__BROKER__USERNAME` for Tradovate
- `TV_BOT__BROKER__PASSWORD` for Tradovate
- `TV_BOT__BROKER__CID` for the Tradovate app/client id
- `TV_BOT__BROKER__SEC` for the Tradovate app secret

Common optional values:

- `TV_BOT__BROKER__PAPER_ACCOUNT_NAME` to force a specific Tradovate paper account
- `TV_BOT__BROKER__LIVE_ACCOUNT_NAME` to force a specific Tradovate live account
- `TV_BOT__PERSISTENCE__PRIMARY_URL` if you want Postgres instead of SQLite fallback
- `TV_BOT__MARKET_DATA__GATEWAY` only if you need a non-default Databento gateway

Example values used below:

```text
DATABENTO_API_KEY=db-live-abc123example
TV_BOT__MARKET_DATA__API_KEY=db-live-abc123example
TV_BOT__BROKER__USERNAME=your.tradovate.login
TV_BOT__BROKER__PASSWORD=correct-horse-battery-staple
TV_BOT__BROKER__CID=your-tradovate-app-cid
TV_BOT__BROKER__SEC=your-tradovate-app-secret
TV_BOT__BROKER__PAPER_ACCOUNT_NAME=SIM123456
TV_BOT__PERSISTENCE__PRIMARY_URL=postgres://postgres:postgres@localhost:5432/tv_bot_core
```

### Windows PowerShell

Use these commands if you want the variables only for the current terminal session:

```powershell
$env:DATABENTO_API_KEY = "db-live-abc123example"
$env:TV_BOT__MARKET_DATA__API_KEY = "db-live-abc123example"
$env:TV_BOT__BROKER__USERNAME = "your.tradovate.login"
$env:TV_BOT__BROKER__PASSWORD = "correct-horse-battery-staple"
$env:TV_BOT__BROKER__CID = "your-tradovate-app-cid"
$env:TV_BOT__BROKER__SEC = "your-tradovate-app-secret"
$env:TV_BOT__BROKER__PAPER_ACCOUNT_NAME = "SIM123456"
$env:TV_BOT__PERSISTENCE__PRIMARY_URL = "postgres://postgres:postgres@localhost:5432/tv_bot_core"
```

Verify they are present in the current PowerShell session:

```powershell
Get-ChildItem Env:TV_BOT__*
```

If you want them to persist for future terminals, use `setx`. `setx` does not update the current shell, so open a new terminal after running it:

```powershell
setx DATABENTO_API_KEY "db-live-abc123example"
setx TV_BOT__MARKET_DATA__API_KEY "db-live-abc123example"
setx TV_BOT__BROKER__USERNAME "your.tradovate.login"
setx TV_BOT__BROKER__PASSWORD "correct-horse-battery-staple"
setx TV_BOT__BROKER__CID "your-tradovate-app-cid"
setx TV_BOT__BROKER__SEC "your-tradovate-app-secret"
setx TV_BOT__BROKER__PAPER_ACCOUNT_NAME "SIM123456"
setx TV_BOT__PERSISTENCE__PRIMARY_URL "postgres://postgres:postgres@localhost:5432/tv_bot_core"
```

If you prefer classic Command Prompt for the current session:

```cmd
set DATABENTO_API_KEY=db-live-abc123example
set TV_BOT__MARKET_DATA__API_KEY=db-live-abc123example
set TV_BOT__BROKER__USERNAME=your.tradovate.login
set TV_BOT__BROKER__PASSWORD=correct-horse-battery-staple
set TV_BOT__BROKER__CID=your-tradovate-app-cid
set TV_BOT__BROKER__SEC=your-tradovate-app-secret
set TV_BOT__BROKER__PAPER_ACCOUNT_NAME=SIM123456
set TV_BOT__PERSISTENCE__PRIMARY_URL=postgres://postgres:postgres@localhost:5432/tv_bot_core
```

### macOS

For a temporary session in `zsh` or `bash`:

```bash
export DATABENTO_API_KEY="db-live-abc123example"
export TV_BOT__MARKET_DATA__API_KEY="db-live-abc123example"
export TV_BOT__BROKER__USERNAME="your.tradovate.login"
export TV_BOT__BROKER__PASSWORD="correct-horse-battery-staple"
export TV_BOT__BROKER__CID="your-tradovate-app-cid"
export TV_BOT__BROKER__SEC="your-tradovate-app-secret"
export TV_BOT__BROKER__PAPER_ACCOUNT_NAME="SIM123456"
export TV_BOT__PERSISTENCE__PRIMARY_URL="postgres://postgres:postgres@localhost:5432/tv_bot_core"
```

To make them persistent for future shells on macOS:

For `zsh`:

```bash
cat <<'EOF' >> ~/.zshrc
export DATABENTO_API_KEY="db-live-abc123example"
export TV_BOT__MARKET_DATA__API_KEY="db-live-abc123example"
export TV_BOT__BROKER__USERNAME="your.tradovate.login"
export TV_BOT__BROKER__PASSWORD="correct-horse-battery-staple"
export TV_BOT__BROKER__CID="your-tradovate-app-cid"
export TV_BOT__BROKER__SEC="your-tradovate-app-secret"
export TV_BOT__BROKER__PAPER_ACCOUNT_NAME="SIM123456"
export TV_BOT__PERSISTENCE__PRIMARY_URL="postgres://postgres:postgres@localhost:5432/tv_bot_core"
EOF
source ~/.zshrc
```

For `bash`:

```bash
cat <<'EOF' >> ~/.bashrc
export DATABENTO_API_KEY="db-live-abc123example"
export TV_BOT__MARKET_DATA__API_KEY="db-live-abc123example"
export TV_BOT__BROKER__USERNAME="your.tradovate.login"
export TV_BOT__BROKER__PASSWORD="correct-horse-battery-staple"
export TV_BOT__BROKER__CID="your-tradovate-app-cid"
export TV_BOT__BROKER__SEC="your-tradovate-app-secret"
export TV_BOT__BROKER__PAPER_ACCOUNT_NAME="SIM123456"
export TV_BOT__PERSISTENCE__PRIMARY_URL="postgres://postgres:postgres@localhost:5432/tv_bot_core"
EOF
source ~/.bashrc
```

Verify:

```bash
env | grep '^TV_BOT__'
```

### Linux

For a temporary session:

```bash
export DATABENTO_API_KEY="db-live-abc123example"
export TV_BOT__MARKET_DATA__API_KEY="db-live-abc123example"
export TV_BOT__BROKER__USERNAME="your.tradovate.login"
export TV_BOT__BROKER__PASSWORD="correct-horse-battery-staple"
export TV_BOT__BROKER__CID="your-tradovate-app-cid"
export TV_BOT__BROKER__SEC="your-tradovate-app-secret"
export TV_BOT__BROKER__PAPER_ACCOUNT_NAME="SIM123456"
export TV_BOT__PERSISTENCE__PRIMARY_URL="postgres://postgres:postgres@localhost:5432/tv_bot_core"
```

To make them persistent on Linux, add them to your shell startup file and reload it. For `bash`:

```bash
cat <<'EOF' >> ~/.bashrc
export DATABENTO_API_KEY="db-live-abc123example"
export TV_BOT__MARKET_DATA__API_KEY="db-live-abc123example"
export TV_BOT__BROKER__USERNAME="your.tradovate.login"
export TV_BOT__BROKER__PASSWORD="correct-horse-battery-staple"
export TV_BOT__BROKER__CID="your-tradovate-app-cid"
export TV_BOT__BROKER__SEC="your-tradovate-app-secret"
export TV_BOT__BROKER__PAPER_ACCOUNT_NAME="SIM123456"
export TV_BOT__PERSISTENCE__PRIMARY_URL="postgres://postgres:postgres@localhost:5432/tv_bot_core"
EOF
source ~/.bashrc
```

For `zsh` on Linux, use the same pattern with `~/.zshrc` instead.

Verify:

```bash
env | grep '^TV_BOT__'
```

### Recommended Minimal Setup

If you only want enough to run observation mode with live Databento data, set just:

```text
DATABENTO_API_KEY or TV_BOT__MARKET_DATA__API_KEY
```

If you want paper or live Tradovate execution flows to work, also set:

```text
TV_BOT__BROKER__USERNAME
TV_BOT__BROKER__PASSWORD
TV_BOT__BROKER__CID
TV_BOT__BROKER__SEC
```

If you want the bot to target a specific account automatically, also set one of:

```text
TV_BOT__BROKER__PAPER_ACCOUNT_NAME
TV_BOT__BROKER__LIVE_ACCOUNT_NAME
```

### Example Full Startup Flow

After your environment variables are set, a typical Windows local run looks like:

```powershell
.\target\release\tv-bot-runtime.exe .\config\runtime.local.toml
```

On macOS or Linux:

```bash
./target/release/tv-bot-runtime ./config/runtime.local.toml
```

If you want to confirm the runtime is seeing your configuration, check the status endpoint after startup:

```text
http://127.0.0.1:8080/status
```

### Notes

- Keep secrets out of Git-tracked TOML files. Environment variables are the intended path for credentials.
- `config/runtime.local.toml` is the right place for non-secret local defaults such as startup mode or default strategy path.
- On Windows, `setx` affects future terminals only. Use `$env:...` as well if you need the values immediately in the current shell.
- If you rotate Databento or Tradovate credentials, restart the runtime after updating the environment.

This workspace is now the primary local checkout, but Windows may still briefly file-lock generated test executables. If that happens, retrying targeted serial tests is usually enough.

For a quick local smoke test on Windows:

- start the runtime host with `.\target\release\tv-bot-runtime.exe .\config\runtime.local.toml`
- start the dashboard dev server from `apps/dashboard` with `npm run dev`
- open `http://127.0.0.1:4173`
- use `http://127.0.0.1:8080/status` or the root API landing page at `http://127.0.0.1:8080/` instead of expecting a UI on the runtime port

For a Databento-only observation smoke test on Windows, set `DATABENTO_API_KEY` or `TV_BOT__MARKET_DATA__API_KEY` in the current PowerShell session and run:

```powershell
.\scripts\dev\start_databento_observation.ps1 -StartDashboard
```

That observation path now rebuilds the release runtime and CLI before launch, uses `runtime.default_strategy_path` from the config when you do not pass `-StrategyPath`, and promotes `DATABENTO_API_KEY` into the runtime env for that launch so an older saved `TV_BOT__MARKET_DATA__API_KEY` does not accidentally win. It also starts warmup with a strategy-driven Databento historical replay window instead of waiting only on live bars, so local smoke tests stay aligned with current source and multi-timeframe warmup should catch up much faster when historical data is available. If the runtime still has no usable Databento market-data session, including missing or rejected credentials, the chart now renders clearly-labeled illustrative sample candles so the dashboard layout stays readable instead of looking broken.

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
- `docs/architecture/dashboard_live_chart_plan.md`
- `docs/ops/README.md`
- `docs/ops/release_readiness_review.md`
