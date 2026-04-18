# STRATEGY_SPEC.md

## Purpose

This document defines the strict Markdown strategy format for the futures trading platform.

Strategy files are authored in Markdown for readability, but the runtime must only act on structured, validated data extracted from the file.

The strategy file must be:
- strict
- deterministic
- AI-writable
- human-readable
- versioned
- compile-validatable

The strategy file is not executable code.

---

## File format rules

1. Strategy files use Markdown headings for sections.
2. Each required section must contain one structured YAML block.
3. Runtime behavior must come only from the structured blocks, not prose.
4. Optional prose may be included for notes, but it must not affect execution.
5. Unknown required sections are invalid.
6. Unknown fields should fail validation unless explicitly marked warning-only by schema rules.
7. Every strategy must declare a `schema_version`.

---

## Required sections

Every strategy file must include all of the following sections:

1. Metadata
2. Market
3. Session
4. Data Requirements
5. Warmup
6. Signal Confirmation
7. Entry Rules
8. Exit Rules
9. Position Sizing
10. Execution
11. Trade Management
12. Risk
13. Failsafes
14. State Behavior
15. Dashboard Display

A file missing any required section must fail validation.

---

## Canonical structure

```md
# Strategy: Human Readable Name

## Metadata
```yaml
...
```

## Market
```yaml
...
```

## Session
```yaml
...
```

## Data Requirements
```yaml
...
```

## Warmup
```yaml
...
```

## Signal Confirmation
```yaml
...
```

## Entry Rules
```yaml
...
```

## Exit Rules
```yaml
...
```

## Position Sizing
```yaml
...
```

## Execution
```yaml
...
```

## Trade Management
```yaml
...
```

## Risk
```yaml
...
```

## Failsafes
```yaml
...
```

## State Behavior
```yaml
...
```

## Dashboard Display
```yaml
...
```
```

---

## Section definitions

## 1. Metadata

Required fields:

```yaml
schema_version: 1
strategy_id: micro_silver_elephant_tradovate_v1
name: Micro Silver Elephant Trend
version: 1.0.0
author: codex
description: Canonical strict Markdown sample for the micro silver strategy layout
```

### Field rules

- `schema_version`: integer, required
- `strategy_id`: string, required, unique identifier-friendly
- `name`: string, required
- `version`: string, required
- `author`: string, required
- `description`: string, required

Optional:
- `tags`: array of strings
- `source`: string
- `notes`: string

---

## 2. Market

Required purpose:
Defines what market family the strategy trades and how instrument resolution should work.

Example:

```yaml
market: gold
selection:
  contract_mode: front_month_auto
```

### Required fields

- `market`: enum/string identifying the target market family
- `selection.contract_mode`: required

### V1 allowed values

`selection.contract_mode`:
- `front_month_auto`

### Notes

V1 uses global front-month handling by default.
Future versions may allow explicit contract overrides.

---

## 3. Session

Required purpose:
Defines when trading is allowed and any session-based flatten/no-entry behavior.

Simple anytime example:

```yaml
mode: always
timezone: America/New_York
```

Fixed window example:

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

### Required fields

- `mode`
- `timezone`

### Allowed `mode` values

- `always`
- `fixed_window`

### Optional fields

- `trade_window.start`
- `trade_window.end`
- `no_new_entries_after`
- `flatten_rule.mode`
- `flatten_rule.time`
- `allowed_days`

### Allowed flatten modes

- `none`
- `by_time`
- `session_end`

---

## 4. Data Requirements

Required purpose:
Declares exactly what market data/timeframes/features are needed.

Example:

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

### Required fields

- `feeds`
- `timeframes`
- `multi_timeframe`

### V1 guidance

The runtime should support:
- 1-second bars
- multi-timeframe strategies
- real-time/tick-driven logic where available

### Suggested feed values

- `trades`
- `ohlcv_1s`
- `ohlcv_1m`
- `ohlcv_5m`
- `mbp`
- `mbo`

Actual supported feeds depend on implementation and market availability.

---

## 5. Warmup

Required purpose:
Defines how much buffered data/state the strategy needs before becoming ready.

Example:

```yaml
bars_required:
  "1s": 600
  "1m": 100
  "5m": 50
ready_requires_all: true
```

### Required fields

- `bars_required`
- `ready_requires_all`

### Rules

- Warmup must be manually triggered by user action
- Warmup completion does not imply arming
- Strategy cannot become trade-ready until warmup requirements are met

---

## 6. Signal Confirmation

Required purpose:
Defines how conditions combine.

Simple example:

```yaml
mode: all
primary_conditions:
  - trend_filter
  - rejection
  - volume_gate
```

Alternative:

```yaml
mode: any
primary_conditions:
  - breakout_up
  - breakout_down
```

### Required fields

- `mode`
- `primary_conditions`

### Allowed `mode` values in V1

- `all`
- `any`
- `n_of_m`
- `weighted_score`

### Optional fields

- `n_required`
- `secondary_conditions`
- `score_threshold`
- `regime_filter`
- `sequence`

Simple strategies should use `all` or `any`.

---

## 7. Entry Rules

Required purpose:
Defines how and when entry is allowed.

Example:

```yaml
long_enabled: true
short_enabled: true
entry_order_type: market
entry_conditions:
  long:
    - trend_up
    - pullback_done
  short:
    - trend_down
    - pullback_done
```

### Required fields

- `long_enabled`
- `short_enabled`
- `entry_order_type`

### Allowed `entry_order_type` values in V1

- `market`
- `limit`
- `stop`
- `stop_limit`

### Optional fields

- `entry_conditions`
- `max_entry_distance_ticks`
- `entry_timeout_seconds`
- `allow_reentry_same_bar`
- `entry_filters`

---

## 8. Exit Rules

Required purpose:
Defines how positions are exited beyond static trade management.

Example:

```yaml
exit_on_opposite_signal: false
flatten_on_session_end: true
exit_conditions:
  - fail_structure
  - regime_invalid
```

### Required fields

- `exit_on_opposite_signal`
- `flatten_on_session_end`

### Optional fields

- `exit_conditions`
- `timeout_exit`
- `max_hold_seconds`
- `emergency_exit_rules`

---

## 9. Position Sizing

Required purpose:
Defines how trade size is computed.

Fixed example:

```yaml
mode: fixed
contracts: 2
```

Risk-based example:

```yaml
mode: risk_based
max_risk_usd: 250
fallback_fixed_contracts: 1
```

Combined-capable example:

```yaml
mode: risk_based
max_risk_usd: 250
min_contracts: 1
max_contracts: 3
fallback_fixed_contracts: 1
```

### Required fields

- `mode`

### Allowed `mode` values

- `fixed`
- `risk_based`

### Optional fields

- `contracts`
- `max_risk_usd`
- `min_contracts`
- `max_contracts`
- `fallback_fixed_contracts`
- `rounding_mode`

### Rules

V1 must support:
- fixed contracts
- risk-based sizing
Both are controlled by the strategy file.

---

## 10. Execution

Required purpose:
Defines how the strategy should place and structure trades.

Minimal example:

```yaml
reversal_mode: flatten_first
scaling:
  allow_scale_in: false
  allow_scale_out: false
  max_legs: 1
broker_preferences:
  stop_loss: broker_required
  take_profit: broker_required
  trailing_stop: broker_preferred
```

### Required fields

- `reversal_mode`
- `scaling`
- `broker_preferences`

### Allowed `reversal_mode`

- `flatten_first`
- `direct_reverse`

### Scaling fields

- `allow_scale_in`: required
- `allow_scale_out`: required
- `max_legs`: required

### Allowed broker preference values

- `broker_required`
- `broker_preferred`
- `bot_allowed`

### Notes

The section is required, but may be minimal.
Simple strategies should rely on safe defaults where documented.

---

## 11. Trade Management

Required purpose:
Defines post-entry trade handling.

Example:

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
partial_take_profit:
  enabled: true
  targets:
    - at_ticks: 40
      percent: 50
```

### Required fields

- `initial_stop_ticks`
- `take_profit_ticks`

### Optional fields

- `break_even`
- `trailing`
- `partial_take_profit`
- `post_entry_rules`
- `time_based_adjustments`

### Rules

Anything that can be broker-native should be placed broker-side where possible.

---

## 12. Risk

Required purpose:
Declares risk controls and required protections.

Example:

```yaml
daily_loss:
  broker_side_required: true
  local_backup_enabled: true
max_trades_per_day: 6
max_consecutive_losses: 3
max_open_positions: 1
```

### Required fields

At minimum:
- `daily_loss`
- `max_trades_per_day`
- `max_consecutive_losses`

### Optional fields

- `max_open_positions`
- `max_unrealized_drawdown_usd`
- `cooldown_after_daily_stop`
- `max_notional_exposure`

### Notes

Some risk controls may be broker-side, some bot-side, and some both.

---

## 13. Failsafes

Required purpose:
Defines strategy-level stop conditions and degraded-mode behavior.

Example:

```yaml
no_new_entries_on_data_degrade: true
pause_on_broker_sync_mismatch: true
pause_on_reconnect_until_reviewed: true
```

### Required fields

At minimum:
- `no_new_entries_on_data_degrade`
- `pause_on_broker_sync_mismatch`

### Optional fields

- `pause_on_reconnect_until_reviewed`
- `kill_on_repeated_order_rejects`
- `abnormal_spread_guard`
- `clock_sanity_required`
- `storage_health_required`

---

## 14. State Behavior

Required purpose:
Defines what the strategy remembers and how it behaves across signals/trades.

Simple example:

```yaml
cooldown_after_loss_s: 300
max_reentries_per_side: 2
```

### Required fields

At minimum:
- `cooldown_after_loss_s`
- `max_reentries_per_side`

### Optional fields

- `regime_mode`
- `memory_reset_rules`
- `post_win_cooldown_s`
- `failed_setup_decay`
- `reentry_logic`

This section may be simple in V1.

---

## 15. Dashboard Display

Required purpose:
Defines strategy-specific display hints.

Example:

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

### Required fields

At minimum:
- `show`
- `default_overlay`

### Optional fields

- `debug_panels`
- `custom_labels`
- `preferred_chart_timeframe`

Defaults should exist for simple strategies.

---

## Defaults policy

The spec is strict, but simple strategies are allowed.
Required sections must exist, but many fields inside those sections may use defaults.

### Example default philosophy

- execution section required, but can be minimal
- session section required, but `mode: always` is valid
- state behavior section required, but may contain only a cooldown and re-entry cap
- dashboard display section required, but may use default panels

The compiler should:
- fill safe defaults where allowed
- surface readable warnings where appropriate
- fail on missing required sections or invalid enums/types

---

## Validation behavior

Validation should fail if:
- required section missing
- required field missing
- unknown enum used
- incompatible sizing/execution rules are declared
- invalid timeframes/feeds are requested
- impossible warmup configuration is declared
- trade management requires invalid order semantics

Validation may warn if:
- optional display hints unknown
- broker-preferred features may degrade to bot-managed
- contract selection assumptions are ambiguous but recoverable

---

## Long-term roadmap notes

V1 uses built-in indicators and rules only.

Long-term extensions may include:
- richer expression/DSL layer
- optional plugin indicators
- regime/state-machine templates
- explicit contract overrides
- advanced cross-market dependency logic

These are roadmap features, not V1 requirements.

---

## Example full strategy

```md
# Strategy: Micro Silver Elephant Trend

## Metadata
```yaml
schema_version: 1
strategy_id: micro_silver_elephant_tradovate_v1
name: Micro Silver Elephant Trend
version: 1.0.0
author: codex
description: Canonical strict Markdown sample for the micro silver strategy layout
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
```

---

## Compiler output expectation

The strategy compiler should transform the Markdown file into a normalized internal object similar to:

- `CompiledStrategy.metadata`
- `CompiledStrategy.market`
- `CompiledStrategy.session`
- `CompiledStrategy.data_requirements`
- `CompiledStrategy.warmup`
- `CompiledStrategy.signal_confirmation`
- `CompiledStrategy.entry_rules`
- `CompiledStrategy.exit_rules`
- `CompiledStrategy.position_sizing`
- `CompiledStrategy.execution`
- `CompiledStrategy.trade_management`
- `CompiledStrategy.risk`
- `CompiledStrategy.failsafes`
- `CompiledStrategy.state_behavior`
- `CompiledStrategy.dashboard_display`

That compiled object is the runtime truth.
