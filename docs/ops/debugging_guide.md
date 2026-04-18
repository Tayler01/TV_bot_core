# Debugging Guide

Use this guide when the runtime, CLI, or dashboard does not behave as expected.

## First Checks

Open these local surfaces first:

- `http://127.0.0.1:8080/`
- `http://127.0.0.1:8080/status`
- `http://127.0.0.1:8080/readiness`
- `http://127.0.0.1:8080/health`
- `http://127.0.0.1:8080/history`

If the dashboard is running, also check:

- `http://127.0.0.1:4173`

## CLI Checks

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml status
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml readiness
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml history
```

These commands should be your first pass before assuming the dashboard is wrong.

## Logs

If you launch through the Databento helper, logs are written to:

- `logs/runtime-host.out.log`
- `logs/runtime-host.err.log`
- `logs/dashboard.out.log`
- `logs/dashboard.err.log`

## Logging Configuration

Runtime logging is controlled by:

- `logging.level`
- `logging.json`

Example in `config/runtime.local.toml`:

```toml
[logging]
level = "debug"
json = false
```

Set `json = true` if you want machine-friendly structured output.

## Common Failures

### 1. Databento key missing or invalid

Symptoms:

- market data unavailable or failed in `/status`
- warmup does not advance
- chart falls back to sample candles

Checks:

- set `DATABENTO_API_KEY`
- restart the runtime after changing env vars
- confirm `/status` no longer reports auth failure

### 2. Databento chart is visible but sample-backed

This is expected when market data is missing or rejected.

Checks:

- inspect `/status`
- inspect `logs/runtime-host.err.log`
- confirm `sample_data_active` in chart config if you are debugging the chart surface

### 3. Tradovate dispatch unavailable

Symptoms:

- status says dispatch unavailable
- paper/live commands stay blocked

Checks:

- broker username/password/cid/sec present
- correct `broker.environment`
- correct `http_base_url` and `websocket_url`
- `paper_account_name` or `live_account_name` matches the selected mode/environment

### 4. Tradovate paper/live mismatch

The runtime expects:

- demo environment for paper routing
- live environment for live routing

If those do not match, readiness and dispatch should block.

### 5. Env vars changed but runtime still uses old values

The runtime reads env vars at process start.

Fix:

- restart the runtime
- if you used Windows `setx`, open a new terminal first

### 6. Postgres unavailable

Check `/readiness` and `/status`.

If SQLite fallback was activated unexpectedly, the runtime should surface a warning and may require a temporary override depending on state.

See:

- `docs/ops/storage_fallback_override.md`

### 7. Reconnect or shutdown review blocks trading

This is expected safety behavior.

Use:

- dashboard review actions, or
- CLI `reconnect-review` / `shutdown`

See:

- `docs/ops/reconnect_and_shutdown_review.md`

## Provider-Specific Checks

### Databento

- confirm the configured dataset is correct
- confirm the strategy warmup window is reasonable
- confirm live replay is catching up after a restart or disconnect

### Tradovate

- confirm token acquisition succeeds
- confirm token renewal is not failing
- confirm user-sync WebSocket is connected
- confirm the selected account matches the expected demo or live routing target

## Chart-Specific Checks

If the chart looks wrong:

1. check `/chart/config`
2. check `/chart/snapshot`
3. check `/status`
4. hard-refresh the dashboard

If bars are present in `/chart/snapshot` but not visible, the issue is frontend rendering.
If bars are absent and `sample_data_active` is false, the issue is upstream market-data state.

## Recommended Debug Sequence

1. `status`
2. `readiness`
3. runtime logs
4. dashboard
5. chart endpoints if the issue is chart-specific
6. provider credentials and environment selection
