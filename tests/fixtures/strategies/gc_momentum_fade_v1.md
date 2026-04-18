# Strategy: GC Momentum Fade

## Metadata
```yaml
schema_version: 1
strategy_id: gc_momentum_fade_v1
name: GC Momentum Fade
version: 1.0.0
author: internal
description: Fade setup for front-month gold futures
```

## Market
```yaml
market: gold
selection:
  contract_mode: front_month_auto
```

## Session
```yaml
mode: fixed_window
timezone: America/New_York
trade_window:
  start: "08:30:00"
  end: "11:30:00"
flatten_rule:
  mode: by_time
  time: "13:00:00"
```

## Data Requirements
```yaml
feeds:
  - type: trades
  - type: ohlcv_1s
timeframes:
  - 1s
  - 1m
  - 5m
multi_timeframe: true
requires:
  volume: true
```

## Warmup
```yaml
bars_required:
  "1s": 600
  "1m": 100
  "5m": 50
ready_requires_all: true
```

## Signal Confirmation
```yaml
mode: all
primary_conditions:
  - trend_filter
  - rejection
  - volume_gate
```

## Entry Rules
```yaml
long_enabled: true
short_enabled: true
entry_order_type: market
```

## Exit Rules
```yaml
exit_on_opposite_signal: false
flatten_on_session_end: true
```

## Position Sizing
```yaml
mode: risk_based
max_risk_usd: 250
fallback_fixed_contracts: 1
```

## Execution
```yaml
reversal_mode: flatten_first
scaling:
  allow_scale_in: true
  allow_scale_out: true
  max_legs: 3
broker_preferences:
  stop_loss: broker_required
  take_profit: broker_required
  trailing_stop: broker_preferred
```

## Trade Management
```yaml
initial_stop_ticks: 40
take_profit_ticks: 80
break_even:
  enabled: true
  activate_at_ticks: 30
trailing:
  enabled: true
  activate_at_ticks: 50
  trail_ticks: 20
```

## Risk
```yaml
daily_loss:
  broker_side_required: true
  local_backup_enabled: true
max_trades_per_day: 6
max_consecutive_losses: 3
max_open_positions: 1
```

## Failsafes
```yaml
no_new_entries_on_data_degrade: true
pause_on_broker_sync_mismatch: true
pause_on_reconnect_until_reviewed: true
```

## State Behavior
```yaml
cooldown_after_loss_s: 300
max_reentries_per_side: 2
```

## Dashboard Display
```yaml
show:
  - pnl
  - net_pnl
  - fills
  - active_brackets
  - latency
  - health
default_overlay: entries_exits
debug_panels:
  - signal_state
  - sizing
  - risk_preview
```
