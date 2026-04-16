# Dev Scripts

This directory holds local developer automation helpers.

## Available Helpers

- `start_databento_observation.ps1`
  Starts the runtime in safe observation mode against `config/runtime.local.toml`, waits for the local control plane, and loads the built-in sample strategy.
  Warmup uses the strategy's historical Databento replay window before falling through to live updates.
  The helper rebuilds the release runtime and CLI before launch so local smoke tests do not run stale binaries after host or dashboard changes.
  It requires `TV_BOT__MARKET_DATA__API_KEY` in the current PowerShell session before launch.
