# V1_ACCEPTANCE_CRITERIA.md

## Purpose

This document defines what must be true for V1 of the futures trading bot to be considered complete enough for controlled paper testing and early live-readiness hardening.

This is not the long-term wishlist.
This is the V1 bar.

---

## 1. Core product outcome

V1 is acceptable when the system can:

- run on Windows, Linux, and macOS
- load exactly one strict Markdown strategy at a time
- validate and compile that strategy into a runtime-safe internal spec
- warm up manually
- paper trade through Tradovate paper/demo using the same execution path as live
- expose an interactive local dashboard and CLI control flow
- require explicit arming before any trading
- prefer broker-side protections whenever possible
- store a full audit trail, metrics, and debug logs
- safely pause, flatten, reconnect, and recover from degraded states

---

## 2. Must-have runtime modes

The system must support all of the following modes:

- `paper`
- `live`
- `observation`
- `paused`

### Acceptance criteria

- mode is always explicit
- mode is shown clearly in dashboard and CLI
- live and paper are visually impossible to confuse
- no order placement is possible if mode is unclear

---

## 3. Strategy system acceptance

The strategy system is acceptable only if:

- strategy files are loaded from either:
  - upload
  - local strategy library
- strategy files use the strict format defined in `STRATEGY_SPEC.md`
- missing required sections fail validation
- invalid enums/types fail validation
- strategies compile into a normalized internal representation
- compiled strategy becomes the only runtime truth
- strategy validation errors are readable to a user
- strategy warnings are distinguishable from blocking errors

### Required supported sections

- Metadata
- Market
- Session
- Data Requirements
- Warmup
- Signal Confirmation
- Entry Rules
- Exit Rules
- Position Sizing
- Execution
- Trade Management
- Risk
- Failsafes
- State Behavior
- Dashboard Display

### V1 strategy capability minimum

V1 strategies must support:
- multi-timeframe logic
- built-in indicator/rule references
- fixed contract sizing
- risk-based sizing
- flatten-first reversal
- direct-reverse setting in spec
- scale-in / scale-out settings
- broker-side protection preferences

---

## 4. Warmup acceptance

Warmup is acceptable only if:

- warmup does not auto-start on launch
- warmup can be manually triggered from CLI and dashboard
- warmup state is visible as:
  - not_loaded
  - loaded
  - warming
  - ready
  - failed
- strategy cannot become trade-ready until warmup passes
- warmup does not arm trading
- hot reload while paused is supported
- hot reload preserves or rebuilds indicator state cleanly
- new entries remain blocked until warmup completes

---

## 5. Market data acceptance

The market data layer is acceptable only if:

- Databento is the sole market data source in V1
- required feed/timeframe availability is checked
- local rolling buffers exist for warmup and display
- 1-second and multi-timeframe support exists
- degraded data health blocks new entries
- existing broker-protected positions are left alone during data degradation
- reconnect behavior is observable and logged
- symbol resolution is visible before arming

### Minimum supported behavior

- subscribe
- reconnect
- health state reporting
- feed readiness checks
- recent buffer access for indicators and dashboard

---

## 6. Tradovate integration acceptance

Tradovate integration is acceptable only if:

- auth token acquisition works
- token renewal works
- centralized session management is used
- account listing and selection work
- paper/demo routing works
- order submission works
- order cancel/modify works where applicable
- open order sync works
- fill sync works
- position sync works
- reconnect detection works

### Execution minimum

The execution layer must support:
- market entry
- limit entry
- broker-side stop placement where supported
- broker-side take-profit placement where supported
- OCO / OSO use where supported by the strategy flow
- flatten
- manual test trade for loaded market
- no trading without arming

---

## 7. Risk and safety acceptance

V1 is acceptable only if:

- arming is required for paper and live
- arming is required for manual dashboard trades
- a grouped pre-arm readiness summary exists
- readiness summary includes:
  - mode
  - strategy loaded
  - warmup ready
  - account selected
  - symbol mapping
  - market data health
  - broker sync health
  - DB/journal health
  - clock sanity
  - readable risk summary
- broker-side requirement mismatches produce warnings
- warnings can require temporary per-session hard override
- hard overrides are always audit logged
- no new entries occur when stream/sync health is degraded

### Open-position safety

- shutdown with open position must warn and block by default
- user must be able to choose:
  - flatten first
  - leave broker-protected position in place
- reconnect with open position must offer:
  - close
  - leave broker-side management
  - reattach bot management

---

## 8. Dashboard acceptance

The dashboard is acceptable only if it supports:

- mode display
- account display
- strategy upload
- strategy library selection
- strategy validation feedback
- warmup trigger
- arm / disarm
- start / pause
- flatten
- disable new entries
- manual order entry for currently loaded market
- manual close / cancel
- open orders view
- fills view
- trade history view
- gross and net PnL
- real-time PnL chart
- per-trade PnL
- latency view
- health view
- log/event stream view
- config/env-backed runtime settings editing

### UX acceptance

- live and paper must be visually distinct
- dangerous actions require confirmation
- dashboard must consume only local control API
- dashboard must not directly speak to Databento or Tradovate

---

## 9. CLI acceptance

The CLI is acceptable only if it supports:

- launch runtime
- specify strategy path
- load strategy
- warmup
- arm / disarm
- start / pause
- flatten
- show readiness status
- show current mode
- show current account
- show current strategy
- view key health state
- handle confirmations for dangerous actions

---

## 10. Persistence acceptance

Persistence is acceptable only if:

- Postgres works as primary storage
- SQLite fallback exists
- fallback from Postgres does not silently occur for trading
- degraded primary DB state produces warning and requires temporary hard override
- all important records are persisted

### Required persisted records

- strategy runs
- event journal
- orders
- fills
- positions
- trade summaries
- gross PnL
- net PnL
- fees
- commissions
- slippage
- latency metrics
- health metrics
- config edits
- manual actions
- overrides

---

## 11. Logging and journaling acceptance

The system is acceptable only if it logs and/or journals:

- strategy load attempts
- validation failures
- warmup state changes
- arm/disarm
- mode changes
- account selection
- symbol resolution
- market data health changes
- broker health changes
- risk pass/fail decisions
- order intents
- broker requests/responses
- fills
- position changes
- PnL updates
- manual actions
- shutdowns
- reconnects
- overrides
- config changes

### Minimum logging requirements

- structured logs available
- debug logging available
- journal records queryable
- timestamps preserved
- source of action preserved where possible:
  - dashboard
  - CLI
  - system/runtime

---

## 12. Metrics acceptance

Metrics are acceptable only if V1 tracks:

### Trade path latency
- market event timestamp
- signal timestamp
- decision timestamp
- order intent timestamp
- order send timestamp
- broker ack timestamp
- fill timestamp
- sync update timestamp

### System health
- CPU
- memory
- reconnect count
- DB write latency
- queue lag or processing lag if applicable
- error counts
- dropped/degraded feed indicators if available

---

## 13. Contract and symbol handling acceptance

Symbol handling is acceptable only if:

- V1 supports front-month auto selection
- symbol mapping from strategy market intent to:
  - Databento symbol(s)
  - Tradovate execution symbol
  is resolved and shown clearly
- symbol mapping is included in pre-arm summary
- rollover logic is global engine behavior, not required strategy complexity

---

## 14. Paper-trading acceptance

Paper mode is acceptable only if:

- it uses Tradovate paper/demo routing
- it uses the same execution core as live
- it still requires arm
- it supports manual test trades
- it records the same orders/fills/PnL/journal structure as live
- it supports controlled repeated testing without code-path divergence from live mode

---

## 15. Cross-platform acceptance

Cross-platform support is acceptable only if:

- runtime launches successfully on Windows, Linux, and macOS
- dashboard/control plane works on all three
- config and path handling are platform-safe
- no Linux-only assumptions are baked into core behavior

---

## 16. Test acceptance

V1 is not acceptable without tests.

### Required minimum
- unit tests for parsing, validation, sizing, risk, mode transitions, readiness checks, symbol resolution
- integration tests for load/warmup/arm/start/pause/flatten/reconnect flows
- paper-trading tests for entry, stop/TP protection path, scaling behavior if enabled, no-new-entry on degraded stream

### Safety-critical test coverage
At minimum, tests must exist for:
- no order placement without arm
- no new entries on degraded data
- shutdown warning with open position
- reconnect detection of existing position
- DB primary degraded warning + override requirement
- strategy validation failure on missing required section

---

## 17. What is explicitly out of scope for V1

Not required for V1 acceptance:

- full backtesting engine
- replay engine
- strategy builder system
- arbitrary user-defined indicator code in strategy files
- multi-strategy runtime in one instance
- full broker-terminal feature parity
- remote multi-user permission system
- advanced distributed deployment/orchestration

These may come later, but they are not part of V1 acceptance.

---

## 18. Final release gate

V1 is acceptable only if all of the following are true:

1. strict strategy files load and validate correctly
2. manual warmup works
3. paper mode works end-to-end through Tradovate demo/paper
4. arming is enforced for all trading
5. broker/account/data/storage health are surfaced in readiness view
6. dashboard supports core control-center functions
7. full event journal and structured debug logging exist
8. PnL includes fees/commission/slippage tracking
9. latency metrics exist
10. critical reconnect/open-position safety flows work
11. primary Postgres persistence works and fallback behavior is safe
12. tests cover the most important safety-critical flows

If any of these are missing, V1 is not done.
