# Dev Scripts

This directory holds local developer automation helpers.

## Available Helpers

- `start_databento_observation.ps1`
  Starts the runtime in safe observation mode against `config/runtime.local.toml`, waits for the local control plane, and loads the requested strategy.
  If `-StrategyPath` is omitted, it uses `runtime.default_strategy_path` from the config file.
  Warmup uses the strategy's historical Databento replay window before falling through to live updates.
  The helper rebuilds the release runtime and CLI before launch so local smoke tests do not run stale binaries after host or dashboard changes.
  It accepts either `DATABENTO_API_KEY` or `TV_BOT__MARKET_DATA__API_KEY`, with `DATABENTO_API_KEY` as the preferred operator-facing variable.
  If the current PowerShell process does not have the key but the Windows user environment does, the helper falls back to the user-level value before launch.
  If both are present, the helper promotes `DATABENTO_API_KEY` into the runtime env for that launch so local observation runs use the newer Databento key consistently.
  Restart the runtime after changing market-data environment variables; the running process will not pick up updated keys automatically.
