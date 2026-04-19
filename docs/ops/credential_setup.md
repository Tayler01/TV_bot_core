# Credential Setup

This document is the source of truth for runtime credentials and non-secret local configuration.

## Principles

- Keep secrets in environment variables.
- Keep non-secret defaults in `config/runtime.local.toml`.
- Prefer one runtime process per Tradovate environment.
- Restart the runtime after changing credentials or broker/data environment variables.

## Preferred Environment Variables

### Databento

- Preferred: `DATABENTO_API_KEY`
- Compatibility alias: `TV_BOT__MARKET_DATA__API_KEY`

Important:

- If both are set, the config loader currently prefers `TV_BOT__MARKET_DATA__API_KEY`.
- For clean operator setup, prefer setting only `DATABENTO_API_KEY` unless you explicitly need the legacy alias.
- The Windows Databento observation helper now falls back to the persisted Windows user environment if the current process was started before the key was added.

### Tradovate

- `TV_BOT__BROKER__USERNAME`
- `TV_BOT__BROKER__PASSWORD`
- `TV_BOT__BROKER__CID`
- `TV_BOT__BROKER__SEC`

Optional but recommended:

- `TV_BOT__BROKER__APP_ID`
- `TV_BOT__BROKER__APP_VERSION`
- `TV_BOT__BROKER__DEVICE_ID`
- `TV_BOT__BROKER__PAPER_ACCOUNT_NAME`
- `TV_BOT__BROKER__LIVE_ACCOUNT_NAME`
- `TV_BOT__BROKER__ENVIRONMENT`
- `TV_BOT__BROKER__HTTP_BASE_URL`
- `TV_BOT__BROKER__WEBSOCKET_URL`

### Persistence

- `TV_BOT__PERSISTENCE__PRIMARY_URL`

## Non-Secret Runtime Config

Put non-secret defaults in `config/runtime.local.toml`, for example:

- `runtime.startup_mode`
- `runtime.default_strategy_path`
- `market_data.dataset`
- `broker.environment`
- `broker.http_base_url`
- `broker.websocket_url`
- `broker.app_id`
- `broker.app_version`
- `broker.device_id`
- `broker.paper_account_name`
- `broker.live_account_name`

Do not commit live secrets to this file.

## Tradovate Environment Model

Tradovate uses different demo and live domains. In practice, treat them as different runtime profiles.

### Paper Or Demo

Use:

- `broker.environment = "demo"`
- `https://demo.tradovateapi.com/v1`
- `wss://demo.tradovateapi.com/v1/websocket`
- `TV_BOT__BROKER__PAPER_ACCOUNT_NAME`

### Live

Use:

- `broker.environment = "live"`
- `https://live.tradovateapi.com/v1`
- `wss://live.tradovateapi.com/v1/websocket`
- `TV_BOT__BROKER__LIVE_ACCOUNT_NAME`

Important:

- One runtime process should point at one Tradovate environment only.
- Do not expect one runtime instance to manage both demo and live sessions at the same time.
- The runtime already guards against demo/live routing mismatches.

## Minimal Setup Profiles

### Databento-Only Observation

Required:

- `DATABENTO_API_KEY`

Recommended local config:

- `runtime.startup_mode = "observation"`
- `runtime.default_strategy_path = "strategies/examples/micro_silver_elephant_tradovate_v1.md"`

### Paper Trading

Required:

- `DATABENTO_API_KEY`
- Tradovate username/password/cid/sec
- demo environment URLs
- paper account name

### Live Trading

Required:

- `DATABENTO_API_KEY`
- Tradovate username/password/cid/sec
- live environment URLs
- live account name

## Windows PowerShell Example

```powershell
$env:DATABENTO_API_KEY = "db-..."
$env:TV_BOT__BROKER__USERNAME = "your.tradovate.login"
$env:TV_BOT__BROKER__PASSWORD = "your-password"
$env:TV_BOT__BROKER__CID = "your-cid"
$env:TV_BOT__BROKER__SEC = "your-secret"
```

Then start the runtime:

```powershell
.\target\release\tv-bot-runtime.exe .\config\runtime.local.toml
```

## Windows Persistent User Env

If you want new terminals to inherit the Databento key automatically:

```powershell
[Environment]::SetEnvironmentVariable("DATABENTO_API_KEY", "db-...", "User")
```

Important:

- Windows user env changes only affect new processes.
- Already-running terminals, editors, and parent launcher apps will not see the new value until restarted.
- If `echo $env:DATABENTO_API_KEY` works in a new PowerShell window but the bot still shows sample candles, restart the stack from that new shell or use `scripts/dev/start_databento_observation.ps1`, which now checks the Windows user env as a fallback.

## macOS Or Linux Example

```bash
export DATABENTO_API_KEY="db-..."
export TV_BOT__BROKER__USERNAME="your.tradovate.login"
export TV_BOT__BROKER__PASSWORD="your-password"
export TV_BOT__BROKER__CID="your-cid"
export TV_BOT__BROKER__SEC="your-secret"
./target/release/tv-bot-runtime ./config/runtime.local.toml
```

## Verification Checklist

After startup, check:

- `http://127.0.0.1:8080/status`
- `http://127.0.0.1:8080/readiness`

What to confirm:

- the expected strategy is loaded or loadable
- market data is healthy
- the expected paper or live account is selected
- storage and journal state are visible
- dispatch is unavailable only for real, understandable reasons

## Common Mistakes

- Setting both Databento key variables and forgetting which one currently wins
- Updating Windows user environment variables without restarting the runtime or the parent process that launches it
- Using a demo Tradovate config with live routing expectations
- Forgetting to set `paper_account_name` or `live_account_name`
- Storing live secrets in Git-tracked TOML files
