use std::collections::BTreeMap;

use chrono::{DateTime, Duration, TimeZone, Utc};
use rust_decimal::Decimal;
use tv_bot_core_types::{
    BrokerPositionSnapshot, BrokerSyncState, CompiledStrategy, ExecutionIntent, SignalDirection,
    Timeframe, TradeSide, WarmupStatus,
};
use tv_bot_indicators::BarInput;
use tv_bot_strategy_loader::StrictStrategyCompiler;
use tv_bot_strategy_runtime::{
    StrategyMarketSnapshot, StrategyRuntimeCompiler, StrategyRuntimeEngine, StrategyRuntimeState,
};

fn base_strategy_markdown() -> String {
    r#"
# Strategy: GC Runtime

## Metadata
```yaml
schema_version: 1
strategy_id: gc_runtime_v1
name: GC Runtime
version: 1.0.0
author: tests
description: runtime tests
```

## Market
```yaml
market: gold
selection:
  contract_mode: front_month_auto
```

## Session
```yaml
mode: always
timezone: America/New_York
```

## Data Requirements
```yaml
feeds:
  - type: ohlcv_1m
timeframes:
  - 1m
multi_timeframe: false
requires:
  volume: true
```

## Warmup
```yaml
bars_required:
  "1m": 10
ready_requires_all: true
```

## Signal Confirmation
```yaml
mode: all
primary_conditions:
  - trend_filter(fast=3,slow=5)
  - rejection(wick_ratio=1.5)
  - volume_gate(period=3,min_ratio=1.5)
```

## Entry Rules
```yaml
long_enabled: true
short_enabled: true
entry_order_type: market
entry_conditions:
  long:
    - close_above_sma(period=5)
  short:
    - close_below_sma(period=5)
```

## Exit Rules
```yaml
exit_on_opposite_signal: true
flatten_on_session_end: true
exit_conditions:
  - fail_structure(fast=3,slow=5)
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
  allow_scale_out: false
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
default_overlay: entries_exits
```
"#
    .to_owned()
}

fn compile_strategy(markdown: &str) -> CompiledStrategy {
    StrictStrategyCompiler
        .compile_markdown(markdown)
        .expect("strategy markdown should compile")
        .compiled
}

fn bullish_bars() -> Vec<BarInput> {
    let base = Utc::now();
    let mut bars = Vec::new();
    for (index, close) in [100, 101, 102, 103, 104, 105, 106, 107, 108]
        .iter()
        .enumerate()
    {
        bars.push(BarInput {
            symbol: "GCM2026".to_owned(),
            timeframe: Timeframe::OneMinute,
            open: Decimal::from(close - 1),
            high: Decimal::from(close + 1),
            low: Decimal::from(close - 2),
            close: Decimal::from(*close),
            volume: 100,
            closed_at: base + Duration::minutes(index as i64),
        });
    }
    bars.push(BarInput {
        symbol: "GCM2026".to_owned(),
        timeframe: Timeframe::OneMinute,
        open: Decimal::from(107),
        high: Decimal::from(109),
        low: Decimal::from(101),
        close: Decimal::from(108),
        volume: 250,
        closed_at: base + Duration::minutes(10),
    });
    bars
}

fn bearish_bars() -> Vec<BarInput> {
    let base = Utc::now();
    let mut bars = Vec::new();
    for (index, close) in [110, 109, 108, 107, 106, 105, 104, 103, 102]
        .iter()
        .enumerate()
    {
        bars.push(BarInput {
            symbol: "GCM2026".to_owned(),
            timeframe: Timeframe::OneMinute,
            open: Decimal::from(close + 1),
            high: Decimal::from(close + 2),
            low: Decimal::from(close - 1),
            close: Decimal::from(*close),
            volume: 100,
            closed_at: base + Duration::minutes(index as i64),
        });
    }
    bars.push(BarInput {
        symbol: "GCM2026".to_owned(),
        timeframe: Timeframe::OneMinute,
        open: Decimal::from(103),
        high: Decimal::from(109),
        low: Decimal::from(101),
        close: Decimal::from(102),
        volume: 250,
        closed_at: base + Duration::minutes(10),
    });
    bars
}

fn snapshot(now: DateTime<Utc>, bars: Vec<BarInput>) -> StrategyMarketSnapshot {
    StrategyMarketSnapshot {
        now,
        warmup_status: WarmupStatus::Ready,
        bars_by_timeframe: BTreeMap::from([(Timeframe::OneMinute, bars)]),
        position: None,
        market_data_degraded: false,
        broker_sync_state: BrokerSyncState::Synchronized,
        reconnect_review_required: false,
    }
}

#[test]
fn compiler_parses_entry_conditions_and_timezone() {
    let compiled = compile_strategy(&base_strategy_markdown());
    let runtime = StrategyRuntimeCompiler::compile(&compiled).expect("runtime compile");

    assert_eq!(runtime.timezone, chrono_tz::America::New_York);
    assert_eq!(runtime.entry_conditions.long.len(), 1);
    assert_eq!(runtime.signal_plan.primary.len(), 3);
}

#[test]
fn emits_long_entry_when_flat_and_signal_edges_on() {
    let compiled = compile_strategy(&base_strategy_markdown());
    let runtime = StrategyRuntimeCompiler::compile(&compiled).expect("runtime compile");
    let now = Utc::now();
    let mut state = StrategyRuntimeState::default();

    let first =
        StrategyRuntimeEngine::evaluate(&runtime, &mut state, &snapshot(now, bullish_bars()))
            .expect("first evaluation");
    assert_eq!(first.signal.direction, SignalDirection::Long);
    match first.intent.expect("entry intent should exist") {
        ExecutionIntent::Enter {
            side,
            quantity,
            protective_brackets_expected,
            ..
        } => {
            assert_eq!(side, TradeSide::Buy);
            assert_eq!(quantity, 1);
            assert!(protective_brackets_expected);
        }
        other => panic!("unexpected intent: {other:?}"),
    }

    let second =
        StrategyRuntimeEngine::evaluate(&runtime, &mut state, &snapshot(now, bullish_bars()))
            .expect("second evaluation");
    assert!(second.intent.is_none());
}

#[test]
fn exits_on_opposite_signal_when_position_is_open() {
    let compiled = compile_strategy(&base_strategy_markdown());
    let runtime = StrategyRuntimeCompiler::compile(&compiled).expect("runtime compile");
    let now = Utc::now();
    let mut state = StrategyRuntimeState::default();
    let mut market_snapshot = snapshot(now, bearish_bars());
    market_snapshot.position = Some(BrokerPositionSnapshot {
        symbol: "GCM2026".to_owned(),
        quantity: 1,
        average_price: Some(Decimal::from(2100)),
        realized_pnl: None,
        unrealized_pnl: None,
        protective_orders_present: true,
        captured_at: now,
    });

    let evaluation =
        StrategyRuntimeEngine::evaluate(&runtime, &mut state, &market_snapshot).expect("exit");

    assert_eq!(evaluation.signal.direction, SignalDirection::Short);
    assert!(matches!(
        evaluation.intent,
        Some(ExecutionIntent::Exit { .. })
    ));
}

#[test]
fn blocks_entries_outside_trade_window() {
    let strategy = base_strategy_markdown().replace(
        "mode: always\ntimezone: America/New_York",
        "mode: fixed_window\ntimezone: America/New_York\ntrade_window:\n  start: \"08:30:00\"\n  end: \"11:30:00\"",
    );
    let compiled = compile_strategy(&strategy);
    let runtime = StrategyRuntimeCompiler::compile(&compiled).expect("runtime compile");
    let mut state = StrategyRuntimeState::default();
    let now = chrono_tz::America::New_York
        .with_ymd_and_hms(2026, 4, 10, 7, 0, 0)
        .single()
        .expect("valid timestamp")
        .with_timezone(&Utc);

    let evaluation =
        StrategyRuntimeEngine::evaluate(&runtime, &mut state, &snapshot(now, bullish_bars()))
            .expect("session evaluation");

    assert_eq!(evaluation.signal.direction, SignalDirection::Long);
    assert!(evaluation.intent.is_none());
    assert!(evaluation
        .signal
        .rationale
        .iter()
        .any(|item| item.contains("outside the configured trade window")));
}

#[test]
fn pauses_when_reconnect_review_is_required() {
    let compiled = compile_strategy(&base_strategy_markdown());
    let runtime = StrategyRuntimeCompiler::compile(&compiled).expect("runtime compile");
    let now = Utc::now();
    let mut state = StrategyRuntimeState::default();
    let mut market_snapshot = snapshot(now, bullish_bars());
    market_snapshot.reconnect_review_required = true;

    let evaluation =
        StrategyRuntimeEngine::evaluate(&runtime, &mut state, &market_snapshot).expect("pause");

    assert_eq!(evaluation.signal.direction, SignalDirection::Flat);
    assert!(matches!(
        evaluation.intent,
        Some(ExecutionIntent::PauseStrategy { .. })
    ));
}

#[test]
fn flattens_after_session_end_when_position_is_open() {
    let strategy = base_strategy_markdown().replace(
        "mode: always\ntimezone: America/New_York",
        "mode: fixed_window\ntimezone: America/New_York\ntrade_window:\n  start: \"08:30:00\"\n  end: \"11:30:00\"\nflatten_rule:\n  mode: by_time\n  time: \"13:00:00\"",
    );
    let compiled = compile_strategy(&strategy);
    let runtime = StrategyRuntimeCompiler::compile(&compiled).expect("runtime compile");
    let mut state = StrategyRuntimeState::default();
    let now = chrono_tz::America::New_York
        .with_ymd_and_hms(2026, 4, 10, 13, 5, 0)
        .single()
        .expect("valid timestamp")
        .with_timezone(&Utc);
    let mut market_snapshot = snapshot(now, bullish_bars());
    market_snapshot.position = Some(BrokerPositionSnapshot {
        symbol: "GCM2026".to_owned(),
        quantity: 1,
        average_price: Some(Decimal::from(2100)),
        realized_pnl: None,
        unrealized_pnl: None,
        protective_orders_present: true,
        captured_at: now,
    });

    let evaluation =
        StrategyRuntimeEngine::evaluate(&runtime, &mut state, &market_snapshot).expect("flatten");

    assert!(matches!(
        evaluation.intent,
        Some(ExecutionIntent::Flatten { .. })
    ));
}
