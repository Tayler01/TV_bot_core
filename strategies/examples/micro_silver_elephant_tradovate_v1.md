# Strategy: Micro Silver Elephant Trend

## Metadata
```yaml
schema_version: 1
strategy_id: micro_silver_elephant_tradovate_v1
name: Micro Silver Elephant Trend
version: 1.0.0
author: codex
description: Closest V1 translation of the SIL Tradovate strategy extracted from the pulled research bot.
tags:
  - micro_silver
  - sil
  - breakout
  - trend_mode
source: strat_research/bot/production/metals_sil_tradovate.py@e9884c4
notes: >
  Extracted from SIL_CONFIG and InstrumentTracker in the pulled source bot. This file preserves the SIL contract family,
  one-second data dependency, market-entry plus broker-side stop workflow, and the major trade-management thresholds. The
  current V1 runtime cannot yet execute the exact 15-bar trend-score mode switch, elephant or consecutive candle detector,
  16:31-17:59 ET entry blackout, or 30/45-minute bar-lock logic, so those details are captured in entry_filters,
  post_entry_rules, and time_based_adjustments while the runnable entry logic uses one-minute breakout proxies and a
  deliberately distant take-profit placeholder.
```

## Market
```yaml
market: micro_silver
selection:
  contract_mode: front_month_auto
```

## Session
```yaml
mode: always
timezone: America/New_York
flatten_rule:
  mode: by_time
  time: "16:40:00"
```

## Data Requirements
```yaml
feeds:
  - type: ohlcv_1s
timeframes:
  - 1s
  - 1m
multi_timeframe: true
requires:
  volume: true
```

## Warmup
```yaml
bars_required:
  "1s": 900
  "1m": 20
ready_requires_all: true
```

## Signal Confirmation
```yaml
mode: all
primary_conditions:
  - volume_gate(period=1,min_ratio=0,timeframe=1m)
```

## Entry Rules
```yaml
long_enabled: true
short_enabled: true
entry_order_type: market
entry_conditions:
  long:
    - breakout_up(lookback=1,timeframe=1m)
  short:
    - breakout_down(lookback=1,timeframe=1m)
allow_reentry_same_bar: false
entry_filters:
  source_pattern:
    primary_signal:
      types:
        - elephant_bar
        - cumulative_consecutive_candle_push
      body_ticks_threshold: 5
      consecutive_min_count: 2
      consecutive_cumulative_move_ticks: 5
    trend_mode:
      lookback_bars: 15
      threshold_percent: 30
      score_formula: abs(close - open_n_bars_ago) / (highest_high - lowest_low) * 100
      modes:
        score_up_above_threshold: long_only
        score_down_above_threshold: short_only
        score_at_or_below_threshold: both
    source_runtime_gap:
      executable_in_v1: false
      runtime_proxy: one_minute_breakout_up_down
```

## Exit Rules
```yaml
exit_on_opposite_signal: false
flatten_on_session_end: false
exit_conditions: []
emergency_exit_rules:
  on_disconnect: close_immediately
  on_reconnect_mismatch: review_required
```

## Position Sizing
```yaml
mode: fixed
contracts: 1
```

## Execution
```yaml
reversal_mode: flatten_first
scaling:
  allow_scale_in: false
  allow_scale_out: false
  max_legs: 1
broker_preferences:
  stop_loss: broker_required
  take_profit: bot_allowed
  trailing_stop: broker_preferred
```

## Trade Management
```yaml
initial_stop_ticks: 40
take_profit_ticks: 1000
break_even:
  enabled: true
  activate_at_ticks: 10
trailing:
  enabled: true
  activate_at_ticks: 15
  trail_ticks: 10
post_entry_rules:
  source_management:
    direction_specific_initial_trail_ticks:
      long: 40
      short: 40
    bar_lock:
      small_body_minutes: 30
      large_body_minutes: 45
      large_body_threshold_ticks: 8
      override_ticks: 0
    break_even_stop_offset_ticks: 3
    profit_lock_ticks: 15
    stop_check_frequency: 1s
    note: V1 runtime approximates this logic with initial_stop_ticks, break_even, and trailing.
time_based_adjustments:
  no_new_entries_session_break:
    start: "16:31:00"
    end: "17:59:59"
  forced_flatten:
    time: "16:40:00"
    reason: eod_margin_switch
  entries_resume:
    time: "18:00:00"
```

## Risk
```yaml
daily_loss:
  broker_side_required: false
  local_backup_enabled: true
max_trades_per_day: 100
max_consecutive_losses: 20
max_open_positions: 1
```

## Failsafes
```yaml
no_new_entries_on_data_degrade: true
pause_on_broker_sync_mismatch: true
pause_on_reconnect_until_reviewed: true
kill_on_repeated_order_rejects: true
clock_sanity_required: true
storage_health_required: true
```

## State Behavior
```yaml
cooldown_after_loss_s: 0
max_reentries_per_side: 999
memory_reset_rules:
  consecutive_candle_streaks:
    reset_on_entry: true
    reset_on_doji: true
    continue_through_session_break: true
  daily_flags:
    reset_at: "18:00:00"
reentry_logic:
  source_behavior:
    explicit_cap_in_source: null
    open_position_cap: 1
    note: Source bot permits reentry after a trade closes; this high cap is the closest V1 translation.
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
  - positions
default_overlay: entries_exits
debug_panels:
  - signal_state
  - sizing
  - risk_preview
  - latency
custom_labels:
  extracted_from: SIL Tradovate bot
  runtime_translation: V1 approximation
preferred_chart_timeframe: 1m
```
