# AGENTS.md

## Purpose

This repository contains a futures trading platform with a strategy-agnostic execution core.

The system uses:
- Databento for market data
- Tradovate for execution, fills, positions, account sync, and paper/live routing
- Postgres as primary storage
- SQLite as fallback
- A local HTTP + WebSocket control plane
- A React dashboard
- Strict Markdown strategy files that compile into validated runtime strategy specs

This file defines how Codex must work in this repository.

---

## Core operating rules

1. The execution core must remain strategy-agnostic.
2. Never embed strategy-specific logic inside broker, execution, risk, persistence, dashboard, or control-plane code.
3. Strategy Markdown files are authoring inputs only. Runtime trading must operate on compiled, validated internal strategy objects.
4. No live or paper order placement without an explicit arm state.
5. No ambiguous runtime mode. Mode must always be explicit and visible.
6. Prefer broker-side protections over bot-side protections whenever possible.
7. All important actions and state transitions must be journaled and logged.
8. Cross-platform support is required for Windows, Linux, and macOS.
9. The bot supports one loaded strategy at a time, but that strategy may allow multiple legs/scale behavior if specified.
10. Paper mode must mirror live execution as closely as possible, using Tradovate paper/demo routing.

---

## Architecture rules

### Separation of concerns

Keep these boundaries strict:

- `strategy_loader` parses and validates strategy Markdown
- `strategy_runtime` evaluates strategy logic and emits intents
- `risk_engine` decides whether intents are allowed and how they should be sized
- `execution_engine` converts intents into broker-native execution flows
- `broker_tradovate` handles Tradovate-specific communication and reconciliation
- `market_data` handles Databento-specific communication and normalization
- `control_api` exposes local HTTP/WebSocket interfaces
- `dashboard` consumes only the local control API, never broker or market data providers directly
- `journal` and `persistence` handle storage and event records
- `config` handles env/runtime configuration
- `instrument_resolver` handles front-month and symbol mapping behavior

### Do not violate these boundaries

Do not:
- place Tradovate REST/WebSocket calls from dashboard code
- parse strategy Markdown from execution code
- compute strategy signals inside broker code
- directly mutate persistent models from unrelated modules
- bypass risk checks during normal execution flow
- hide side effects in convenience helpers

---

## Development style

### General coding standards

- Prefer explicit types and interfaces
- Keep modules small and composable
- Favor deterministic behavior over cleverness
- Avoid hidden global state
- Avoid magic defaults that are not documented
- Use structured errors
- Make state transitions explicit
- Design for auditability and debuggability first
- Write readable code over compact code
- Document unsafe assumptions clearly

### Rust expectations

- Use clear domain types instead of raw strings where practical
- Prefer enums over stringly-typed mode/command handling
- Avoid panics in runtime/service paths
- Keep async boundaries explicit
- Avoid long blocking work on async runtimes
- Make reconnection and retry behavior testable
- Use traits/interfaces for adapters and major service contracts

### Frontend expectations

- Keep live/paper modes impossible to confuse visually
- Dangerous actions must require confirmation
- Do not allow the frontend to become the source of truth
- Frontend state should derive from backend state/events
- Dashboard actions must be audit logged by the backend

---

## Testing requirements

Codex must not treat implementation as complete without tests.

### Minimum required tests

1. Unit tests for:
   - strategy parsing
   - strategy validation
   - sizing logic
   - risk decisions
   - mode transitions
   - readiness checks
   - symbol resolution
   - latency metric calculations

2. Integration tests for:
   - strategy load -> warmup -> ready flow
   - arm/disarm flow
   - manual command flow
   - broker sync mismatch handling
   - reconnect with existing open position
   - DB degraded state handling
   - warning + temporary hard override behavior

3. Paper-mode execution tests for:
   - entry with broker-side stop/TP
   - scaling if enabled
   - flatten behavior
   - pause/no-new-entry behavior on stream degradation
   - restart/reconnect detection

### Validation rule

If a behavior is safety-critical, write a test for it.

---

## Logging and journaling rules

The system must journal and/or log:

- strategy load attempts
- validation results
- warmup start/ready/failure
- mode transitions
- arm/disarm
- hard overrides
- account selection
- symbol resolution
- market data health changes
- broker connectivity changes
- order intents
- risk accept/reject decisions
- broker requests/responses
- fills
- position changes
- PnL changes
- manual actions
- shutdown/restart/reconnect flows
- configuration changes

Use structured logs where possible.
Important records must also be persisted in the event journal.

---

## Persistence rules

- Postgres is the primary database target
- SQLite is fallback only
- Trading must warn and require temporary per-session override if primary DB is unavailable and fallback is needed unexpectedly
- Persist enough information for full auditability
- Store gross PnL, fees, commissions, slippage, and net PnL
- Store latency metrics per trade path
- Store system health metrics

---

## Safety and control rules

### Arming
No trading without arming.
This applies to:
- live strategy execution
- paper strategy execution
- manual dashboard order placement

### Degraded data/sync
If market data or broker sync degrades:
- do not enter new positions
- leave broker-protected existing positions alone unless the user explicitly decides otherwise

### Shutdown with open position
Default behavior:
- warn
- block shutdown
- let user choose to flatten or leave broker-protected position in place

### Reconnect with active position
On reconnect:
- detect existing position/orders
- warn user
- offer close / leave broker-side / reattach

### Broker-side expectation mismatch
If the strategy expects broker-side protections that cannot be satisfied:
- warn
- require temporary hard override to proceed

---

## Strategy system rules

- Strategy Markdown must be strict and schema-driven
- Every strategy must compile into an internal normalized representation
- Unknown fields should fail validation unless explicitly designated warning-only
- Strategy sections required by spec must be enforced
- Simple strategies are allowed, but they must still use the strict format
- Built-in indicators/rules are the V1 approach
- Do not build a freeform strategy scripting language in V1

---

## Performance and observability expectations

Track:
- signal latency
- decision latency
- order send time
- broker ack time
- fill time
- sync update time
- DB write latency
- reconnect counts
- queue pressure/backlog if applicable
- CPU/memory and runtime health

The system should be optimized for correctness, observability, and safe latency first.
Do not over-optimize by removing visibility.

---

## Suggested implementation order

1. Core domain types and interfaces
2. Strategy spec parser + validator + compiler
3. Runtime mode state machine
4. Symbol/front-month resolver
5. Databento market data adapter
6. Warmup buffers and multi-timeframe aggregators
7. Tradovate auth/session manager
8. Tradovate order/account sync adapter
9. Risk engine
10. Execution engine
11. Persistence/journal/metrics
12. Control API
13. Dashboard
14. Hardening and paper test passes

---

## Definition of done for any module

A module is not done unless:
- it respects architecture boundaries
- it has tests
- it has logging where needed
- it has failure behavior defined
- it does not introduce hidden coupling
- it does not bypass safety model
- it is documented enough for the next pass

---

## What Codex should do when uncertain

If a decision is unclear:
1. preserve strategy-agnostic boundaries
2. preserve safety
3. preserve auditability
4. prefer explicit configuration over implicit behavior
5. prefer broker-side safety where available
6. leave clear TODOs only when absolutely necessary, never as a substitute for core implementation
