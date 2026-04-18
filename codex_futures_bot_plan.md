# Codex Implementation Plan: Modular Futures Trading Bot

## 1. Product goal

Build a **cross-platform futures trading platform** for Windows, Linux, and macOS with these properties:

- one strategy loaded at a time
- strategy authored as a **strict Markdown spec**
- same core for **paper** and **live**
- **manual arm required** before any trading, including manual dashboard orders
- **broker-side protections preferred/required** wherever possible
- dashboard as an **interactive control center**
- full audit trail, latency tracking, health metrics, and debug logging
- backtesting/replay intentionally deferred into a separate sister project

The architecture should be built around a **strategy-agnostic execution core** that uses **Databento for market data**, **Tradovate for execution/account state**, and a **local control plane** for the dashboard. Tradovate’s API supports REST order/account operations, WebSocket-based user sync, demo account flows, and advanced order patterns like OCO, OSO, and multi-bracket order strategies. Databento’s live API supports one session per dataset, multiple subscriptions per session, the same schemas/record structures across live and replay-style use, and intraday replay within the live API. fileciteturn2file0 fileciteturn2file1 [Databento Live API docs](https://databento.com/docs/api-reference-live/basics/schemas-and-conventions)

## 2. Core principles

1. **Execution core stays strategy-agnostic.**  
   No strategy logic in the broker adapter, risk engine, or dashboard.

2. **Markdown is authoring format, not runtime truth.**  
   The runtime only acts on a validated, compiled internal strategy spec.

3. **Paper must mirror live.**  
   Paper trading should use the same execution path and Tradovate paper/demo account routing, not a fake side simulator. Tradovate exposes demo account APIs like `openDemoAccount` and `resetDemoAccountState`. fileciteturn2file0

4. **Broker-side protection first.**  
   Stops, TPs, brackets, and account protections should be placed broker-side whenever supported. Tradovate documents order placement, OCO, OSO, liquidation operations, and account risk-related objects/endpoints. fileciteturn2file3 fileciteturn2file4

5. **Arming is mandatory.**  
   No live or paper order placement without explicit arming.

6. **No ambiguous mode.**  
   Runtime mode must always be explicit: paper, live, observation, paused.

## 3. Recommended stack

Use a **hybrid stack**.

### Runtime core: Rust
Rust should own:
- market data ingestion
- event bus
- strategy runtime
- broker adapter
- execution engine
- risk engine
- local HTTP/WebSocket control plane
- state store
- journal/logging/metrics

Reason: low latency, deterministic concurrency, better safety for always-on trading services.

### Tooling/support: Python
Python should own:
- strategy MD compiler/validator tools if convenient
- dev utilities
- report generation
- migration scripts
- later sister projects like backtest/replay helpers and strategy-builder tooling

### Dashboard
- React frontend
- local backend served by the Rust control plane

### Storage
- **Postgres primary**
- **SQLite fallback**
- fallback should warn and require a **temporary per-session hard override** before trading starts

## 4. External systems design

### Databento
Use Databento as the **single market data source**. Databento’s live API uses a socket/session model, supports multiple subscriptions in a single session, supports parent and continuous symbology, and supports intraday replay in the live service. [Databento Live API docs](https://databento.com/docs/api-reference-live/basics/schemas-and-conventions)

Implications:
- one Databento session per dataset for the runtime
- symbol resolver must map strategy intent into Databento subscription symbols
- local rolling buffers must support warmup and indicator construction
- no new entries if data stream health is degraded

### Tradovate
Use Tradovate for:
- authentication
- account list/account selection
- orders
- fills
- positions
- user/account sync
- paper/demo routing
- broker-side bracket and risk features where possible

Tradovate access tokens expire after **90 minutes** and the docs warn about a **2 concurrent session** limit, recommending renewal rather than repeated token requests. That means the bot needs a central token/session manager. fileciteturn2file1 fileciteturn2file2

## 5. Major runtime modules

### A. runtime kernel
Responsibilities:
- process lifecycle
- config boot
- dependency wiring
- mode management
- service start/stop
- health supervision

### B. strategy loader/compiler
Responsibilities:
- load MD from file upload or strategy library
- parse structured sections
- validate schema
- compile into internal `CompiledStrategy`
- return readable validation errors/warnings

### C. instrument resolver
Responsibilities:
- resolve strategy market intent into front-month contract
- maintain global front-month rollover behavior
- map to Databento symbology and Tradovate execution symbol
- show resolved mapping in pre-arm summary

V1 default: **front-month auto**.

### D. market data adapter
Responsibilities:
- Databento session management
- subscription management
- tick/bar aggregation
- multi-timeframe construction
- heartbeat/reconnect handling
- rolling buffer/warmup cache
- market data health status

### E. indicator/rule engine
Responsibilities:
- built-in indicator library
- parameterized rule evaluation
- multi-timeframe condition evaluation
- state-aware confirmation logic

V1 should support **built-in indicators/rules only**, not arbitrary custom indicator code from the MD file. Long-term roadmap can introduce a DSL/plugin layer.

### F. strategy runtime
Responsibilities:
- consume normalized market and broker state
- maintain strategy-local state
- emit **intents**, not broker calls

Examples:
- `EnterLong`
- `EnterShort`
- `Exit`
- `Flatten`
- `CancelWorkingOrders`
- `PauseStrategy`
- `ReducePosition`

### G. risk engine
Responsibilities:
- local bot-side risk checks
- broker-side protection validation
- sizing calculation
- kill/failsafe enforcement
- pre-arm validation summary generation

### H. execution engine
Responsibilities:
- convert intents into broker-native execution structures
- prefer OCO/OSO/bracket/multi-bracket where possible
- support scale-in/scale-out if the strategy allows it
- handle flatten-first or direct-reverse depending on strategy setting

Tradovate documents `placeOrder`, `placeOCO`, `placeOSO`, and `startOrderStrategy`. fileciteturn2file0 fileciteturn2file3

### I. Tradovate broker adapter
Responsibilities:
- auth token acquisition/renewal
- account lookup and confirmation
- order submission/cancel/modify
- position/fill/order sync
- reconnect recovery
- detection of unexpected open positions/orders

### J. control plane
Expose:
- HTTP API for commands and queries
- WebSocket API for live event streaming

Use:
- HTTP for strategy load, config edits, arm/disarm, start/pause, flatten, manual order commands, and historical queries
- WebSocket for live PnL, fills, orders, health, logs, and runtime state

### K. state store
Responsibilities:
- current runtime truth
- account state
- order state
- position state
- strategy readiness state
- dashboard projection state

### L. journal + metrics + logging
Responsibilities:
- append-only event journal
- structured logs
- debug logs
- performance/latency metrics
- health metrics
- audit trails

## 6. Trading modes

### paper
- same logic as live
- routes to Tradovate paper/demo
- still requires arm
- still requires checklist
- must be visually impossible to confuse with live

### live
- real account
- requires explicit account confirmation
- requires arm
- stricter warnings and overrides

### observation
- strategy can load and warm up
- no order placement
- useful for reconnect/recovery and validation

### paused
- no new entries
- can still observe, sync, and warm up

## 7. Arming model

No separate pile of many confirmations. Build **one readable pre-arm readiness screen** with grouped status:

- mode
- strategy loaded/validated
- warmup ready
- account selected
- resolved Databento symbols
- resolved Tradovate symbol
- market data health
- broker sync clean
- DB/journal healthy
- clock sane
- risk summary

Then:
- `Arm` for normal path
- temporary **hard override** for exceptions

Hard overrides are:
- temporary
- per-session only
- always audit logged

## 8. Runtime safety rules

### data/sync failure
If stream/sync health is down:
- **do not open new positions**
- leave existing broker-protected positions alone

### shutdown with open position
If open position exists:
- warn and block by default
- user chooses either:
  - flatten first
  - or confirm shutdown while leaving broker-side protection in place

### reconnect with active position
On reconnect:
- detect existing position/orders
- warn user
- offer:
  - close position
  - leave broker-side protection managing it
  - reattach bot management

### live/manual trading
Any trading action, including manual dashboard trades, requires arming.

### broker-side protection mismatch
If strategy expects broker-side safety that cannot be placed exactly:
- warn clearly
- require temporary hard override to proceed

## 9. Strategy MD system

The MD file must be **strictly structured**, AI-writable, and well documented.

Do not rely on prose interpretation.  
Use Markdown sections with structured blocks, preferably YAML.

### Required sections
Every strategy must declare:

- metadata
- market/instrument
- session/time rules
- data requirements
- warmup
- signal/confirmation
- entry rules
- exit rules
- position sizing
- execution
- trade management
- risk
- failsafes/kill conditions
- state behavior
- dashboard/display

### Required but simple-friendly
Simple strategies should still be valid with defaults.

Examples:
- session section can declare `always`
- execution can be minimal and inherit defaults
- state behavior can be minimal
- display section can be minimal

### Recommended top-level fields
Inside metadata:
- `schema_version`
- `strategy_id`
- `name`
- `version`
- `author`
- `description`

### Important v1 design choice
The MD file can configure:
- flatten-first vs direct-reverse
- fixed contracts vs risk-based sizing
- one position vs multiple legs
- scale-in/scale-out rules
- broker-required vs broker-preferred vs bot-allowed behavior inside execution/risk/failsafe fields

But V1 should use a **fixed built-in indicator/rule library** with parameters, not arbitrary custom strategy code.

## 10. Example strategy shape

```md
# Strategy: Micro Silver Elephant Trend

## Metadata
```yaml
schema_version: 1
strategy_id: micro_silver_elephant_tradovate_v1
name: Micro Silver Elephant Trend
version: 1.0.0
author: internal
description: Trend strategy for front-month micro silver futures
```

## Market
```yaml
market: silver
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
```

## Failsafes
```yaml
no_new_entries_on_data_degrade: true
pause_on_broker_sync_mismatch: true
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
```
```

## 11. Front-month and symbol handling

Make rollover **global engine behavior**, not mandatory per-strategy complexity.

V1:
- global front-month resolver
- strategy usually specifies market family only
- resolver maps to Databento and Tradovate
- pre-arm screen shows exact resolved symbols before arming

Longer-term:
- allow optional per-strategy override for exact contracts or custom rollover behavior

Databento supports symbology types including `parent` and `continuous`, and notes that identical new continuous subscriptions may remap differently over time while existing live subscriptions are not remapped in-place. [Databento Live API docs](https://databento.com/docs/api-reference-live/basics/schemas-and-conventions)

## 12. Broker-side-first execution model

Execution engine should choose the safest available path:

1. broker-native bracket / OSO / OCO / strategy order if it expresses the strategy cleanly
2. broker-native stop/TP attached around entry if supported
3. bot-managed fallback only if strategy allows it

Tradovate documents:
- single orders
- OCO
- OSO
- multi-bracket strategy start via WebSocket
- liquidation operations
- user sync via WebSocket fileciteturn2file0 fileciteturn2file3

Important note:  
Specific behavior for every advanced order combination, especially trailing logic under all conditions, should be validated during paper integration tests before assuming full broker-native coverage across every strategy shape. The docs show supported order/strategy primitives, but exact execution semantics need test confirmation. fileciteturn2file0 fileciteturn2file3

## 13. Dashboard scope

The dashboard is an **interactive control center**.

### It must support
- mode display
- account display
- strategy library picker
- strategy upload
- validation results
- warmup trigger
- arm/disarm
- start/pause
- flatten
- disable new entries
- manual order entry for the loaded contract only
- manual close/cancel
- health view
- logs
- event stream
- open orders
- fills
- trade history
- gross/net/session PnL
- real-time PnL chart
- per-trade PnL
- latency panel
- config editor for env-backed runtime settings

### It should not become
- a general full broker terminal
- a multi-strategy control center in v1

### UX rules
- live vs paper visually impossible to confuse
- dangerous actions require extra confirmation
- risk summary readable, not raw YAML

## 14. Persistence design

### Postgres primary
Store:
- strategies
- runs/sessions
- event journal
- orders
- fills
- positions
- PnL snapshots
- latency metrics
- health metrics
- config change history
- manual actions / overrides

### SQLite fallback
- allowed only when explicitly enabled/overridden
- useful for dev/test/emergency local use
- trading should warn and require temporary hard override if Postgres is down and fallback wasn’t already planned

## 15. Logging, journaling, and metrics

V1 must store everything useful.

### Event journal
Append-only and replay-friendly:
- strategy load
- validation results
- warmup transitions
- arm/disarm
- checklist status
- market data events
- signals
- risk decisions
- order intents
- broker requests
- broker responses
- order state updates
- fills
- position changes
- reconnect events
- overrides
- shutdown actions
- manual controls

### PnL and trade analytics
Track:
- gross PnL
- fees
- commissions
- slippage
- net PnL
- per-trade breakdown
- session totals

Tradovate’s schemas include fill, fill fee, cash balance, and related order/account entities that support building detailed execution/account reporting. fileciteturn0file0

### Latency metrics
Track per trade/action:
- market event ts
- signal ts
- decision ts
- order intent ts
- order send ts
- broker ack ts
- fill ts
- position sync ts

### System health metrics
Track:
- CPU
- memory
- event loop lag / processing lag
- dropped or late market messages
- reconnect counts
- DB write latency
- queue depths
- API error rates

## 16. API/config model

### Env/config system
Secrets and runtime settings belong outside strategy MD.

Examples:
- Tradovate credentials / secrets
- Databento key
- DB URL
- service ports
- mode defaults
- logging levels
- local dashboard bind settings

Dashboard may edit runtime config, but:
- masked where needed
- explicit apply flow
- changes apply only after restarting affected modules/services
- all edits audit logged

## 17. Startup/load/warmup flow

### Startup
- process starts
- mode explicit
- strategy path may be supplied
- but bot does **not** auto-load or auto-warm

### Manual load/warmup
- user loads strategy
- validation/compile runs
- if valid, warmup can be triggered manually
- after warmup, status becomes `ready`
- trading still cannot start until arm

### Hot reload
Allow **hot reload while paused**:
- validate new strategy
- compile
- warm up using current buffers if possible
- remain paused
- no auto-trading after reload

If open position exists:
- allow observation/warmup
- do not auto-resume trading control silently

## 18. Pre-arm grouped checklist

Do not build 20 separate modal traps.  
Build one grouped readiness summary.

### Required checks
- explicit mode
- strategy valid
- warmup ready
- account selected
- market data healthy
- symbol mapping resolved
- no unresolved broker position/order mismatch
- DB/journal healthy
- clock sane
- readable risk summary displayed

### Warning + override cases
- broker-side requirements not fully satisfiable
- DB primary unavailable but fallback usable
- other degraded-but-operable conditions

## 19. Order/position behavior

### Scaling
Support scale-in and scale-out in v1, controlled by strategy.

### Reversal
Support both:
- `flatten_first`
- `direct_reverse`

Default recommendation remains `flatten_first` for safer behavior, but strategy may opt in to direct reverse.

### One strategy at a time
One active strategy per bot instance, but that strategy may manage multiple legs/positions if its spec allows.

## 20. Suggested repository structure

```text
bot/
  apps/
    runtime/
    dashboard/
    cli/
  crates/
    core_types/
    strategy_spec/
    strategy_loader/
    instrument_resolver/
    market_data/
    indicators/
    rule_engine/
    strategy_runtime/
    risk_engine/
    execution_engine/
    broker_tradovate/
    control_api/
    state_store/
    journal/
    metrics/
    health/
    config/
    persistence/
  strategies/
    examples/
    schemas/
    docs/
  docs/
    architecture/
    strategy-spec/
    ops/
    api/
  scripts/
    dev/
    migrations/
  tests/
    unit/
    integration/
    paper/
```

## 21. Interfaces Codex should define first

Codex should start with interfaces/contracts before implementation.

Define these first:
- `CompiledStrategy`
- `MarketEvent`
- `SignalDecision`
- `ExecutionIntent`
- `RiskDecision`
- `BrokerOrderCommand`
- `BrokerOrderUpdate`
- `BrokerPositionSnapshot`
- `RuntimeMode`
- `WarmupStatus`
- `ArmReadinessReport`
- `SystemHealthSnapshot`
- `EventJournalRecord`

## 22. Delivery phases

### Phase 0
- repo skeleton
- core types
- config system
- mode system
- logging/journal scaffolding
- DB schema design

### Phase 1
- strategy MD schema + parser/compiler
- validation engine
- example strategies
- front-month resolver

### Phase 2
- Databento adapter
- warmup buffers
- multi-timeframe aggregator
- health checks

### Phase 3
- Tradovate auth/session manager
- account lookup
- order primitives
- user sync
- paper/live account routing

### Phase 4
- execution engine
- risk engine
- broker-side preference enforcement
- scale/reversal support

### Phase 5
- control plane HTTP/WebSocket
- dashboard v1
- manual controls
- pre-arm readiness screen

### Phase 6
- journaling
- latency metrics
- fee/slippage/net PnL
- trade history
- reconnect recovery flows

### Phase 7
- cross-platform packaging
- hardening
- paper test campaigns
- operational docs

## 23. What Codex should explicitly avoid

- no hidden globals
- no direct strategy logic inside broker/execution core
- no prose-based runtime parsing
- no auto-trading on startup
- no implicit mode
- no live trading without arm
- no silent fallback from degraded safety to bot-managed behavior
- no coupling dashboard directly to broker or Databento
- no assuming every trailing/bracket variant works broker-side without paper validation

## 24. First implementation target

Tell Codex to build this first:

1. strict strategy schema and compiler
2. core runtime state machine
3. Databento market data adapter with rolling buffers
4. Tradovate session manager and basic paper/live account selection
5. execution path for:
   - market entry
   - limit entry
   - broker-side stop
   - broker-side TP
   - OCO/OSO where applicable
6. pre-arm readiness report
7. HTTP/WebSocket control plane
8. dashboard with:
   - mode
   - strategy load
   - warmup
   - arm
   - pause
   - flatten
   - manual test order
   - fills/PnL/live state
9. Postgres persistence + SQLite fallback
10. full event journal and latency metrics

## 25. Final Codex instruction block

Build a cross-platform futures trading runtime with a Rust core, strict Markdown strategy spec, Databento market data, Tradovate execution, Postgres primary storage, SQLite fallback, local HTTP/WebSocket control plane, and React dashboard. The system must be strategy-agnostic, one-strategy-at-a-time, arm-gated, and broker-side-protection-first. Paper mode must mirror live using Tradovate demo/paper accounts. All actions and market/execution decisions must be fully journaled, metrics-rich, and recoverable. Strategy files must be strictly structured, compile-validated, and AI-writable. Start by implementing contracts, strategy compiler, market/broker adapters, risk/execution skeletons, pre-arm readiness flow, and dashboard control center.
