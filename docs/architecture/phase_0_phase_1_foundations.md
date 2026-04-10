# Phase 0 / Phase 1 Foundations

This repository is scaffolded as a Rust workspace with explicit crate boundaries that match the architecture plan in `AGENTS.md` and `codex_futures_bot_plan.md`.

Implemented foundations:

- `crates/core_types`: shared runtime-safe domain contracts such as `CompiledStrategy`, `RuntimeMode`, `ExecutionIntent`, `RiskDecision`, `ArmReadinessReport`, and `EventJournalRecord`
- `crates/config`: TOML plus environment-based runtime configuration loading with explicit startup mode validation
- `crates/runtime_kernel`: mode transitions, warmup state, arming gates, readiness report evaluation, a runtime orchestration bridge that evaluates risk before execution dispatch and journals every risk decision and hard-override event, and a unified command loop so manual actions and strategy intents share the same audited execution path
- `crates/strategy_loader`: strict Markdown section parsing, YAML validation, and compilation into `CompiledStrategy`
- `crates/instrument_resolver`: strategy-agnostic front-month resolution from market family into explicit Databento and Tradovate symbol mappings
- `crates/market_data`: Databento-oriented subscription contracts, async session/reconnect transport boundaries, a concrete live transport backed by the official Databento Rust client, a runtime-facing market-data service loop for manual warmup/replay handling, feed health states, rolling buffers, local multi-timeframe aggregation, warmup tracking, and runtime-ready status snapshots
- `crates/broker_tradovate`: Tradovate auth/session foundations with explicit credential and environment contracts, centralized access-token renewal, account lookup and routing-aware selection, user-sync session management, reconnect/review state tracking, provider-safe broker status snapshots, fill/account reconciliation from live sync, and a concrete live REST/WebSocket transport for bearer auth, renewal, `authorize`, `user/syncrequest`, `props` updates, heartbeat handling, `placeorder`, `placeoso`, and `liquidateposition`
- `crates/execution_engine`: strategy-agnostic execution planning and dispatch that turns normalized `ExecutionIntent` values plus runtime/broker context into explicit Tradovate order primitives, flatten-first or direct-reverse plans, broker-side bracketed entries, session-managed Tradovate submission flows, and safety-blocking errors when arming/readiness or strategy execution settings do not permit order placement
- `crates/risk_engine`: strategy-agnostic pre-execution risk evaluation that normalizes fixed and risk-based sizing, enforces daily-trade / consecutive-loss / unrealized-drawdown limits, and surfaces broker-required protection gaps as warning or temporary hard-override decisions
- `crates/control_api`: a transport-agnostic local control surface with thin HTTP-style command handlers and a WebSocket-style event hub, mapping dashboard/CLI manual commands and strategy intents onto the unified runtime command loop so control-plane transports stay thin without bypassing auditing or risk checks
- `crates/journal`: testable event-journal abstraction with an in-memory adapter

Reserved crate boundaries are scaffolded for later phases so market data, broker integration, execution, risk, control API, and persistence can be added without violating the strategy-agnostic execution core.

Important guardrails already enforced:

- runtime mode is explicit and required at startup
- trading cannot arm from `observation` or `paused`
- warmup readiness is separate from arming
- strategy runtime truth comes from compiled internal objects, not raw Markdown
- unknown Markdown sections and unknown YAML fields fail validation
- Databento reconnect state, heartbeat visibility, and disconnect reasons are explicit in runtime snapshots
- Databento live transport decodes official DBN trade/bar/system records into internal `MarketEvent` updates without leaking provider-specific details into the execution core
- manual warmup can run either live-only or replay-then-live, and replay catch-up must complete before the market-data service reports trade readiness
- multi-timeframe warmup can be satisfied from the smallest provider-supported bar feed without pushing strategy-specific logic into the execution core
- broker account selection and broker-sync readiness now flow through generic broker status snapshots, so the runtime can block unsafe paper/live mismatches, stale sync, mismatch states, and reconnect review requirements without learning Tradovate-specific internals
- Tradovate networking is isolated behind broker traits, with HTTP token/account operations, provider-specific order submission primitives, and WebSocket sync parsing confined to `broker_tradovate` so the execution core still consumes only normalized broker state
- broker sync now normalizes fills and account-state snapshots alongside positions and working orders, which keeps reconciliation provider-aware while preserving shared runtime contracts
- execution planning and dispatch remain strategy-agnostic while still producing broker-native order structures, so reversal mode, scaling permissions, broker-required stop/TP expectations, and no-new-entry safety checks are enforced without embedding signal logic into the broker adapter
- broker-preferred execution hints surface as warnings instead of silently changing behavior
- risk evaluation now adjusts entry quantities from compiled sizing rules before execution, and broker-required stop / take-profit / trailing / daily-loss gaps are surfaced as explicit override decisions instead of being silently ignored
- runtime orchestration now runs `risk_engine` directly ahead of broker dispatch and persists both the risk decision and any hard-override requirement/use into the event journal for auditability
- manual and strategy-originated intents now enter through the same runtime control loop, which normalizes strategy provenance, journals the incoming intent, and records whether dispatch was performed, skipped, or failed
- the local control API now routes manual dashboard/CLI commands and strategy-originated intents into that same runtime loop, with HTTP-style response mapping and WebSocket event publication layers that do not need their own execution path

Verification status:

- `cargo test -j 1` passed for the full workspace after the broker execution/reconciliation changes landed.
- `cargo test -j 1 -p tv-bot-broker-tradovate` passes after formatting, covering live REST auth/account/execution calls, WebSocket authorize and `user/syncrequest` flow, fill/account `props` reconciliation, reconnect review gating, and session-managed order submission context.
- `cargo test -j 1 -p tv-bot-execution-engine` passes after formatting, covering arm-gated order blocking, market and limit entry planning, broker-side OSO bracket construction, flatten-first reversal, direct-reverse sizing, scale-in blocking, safe flatten no-op behavior, dispatch through the Tradovate session manager, and planning-vs-broker error separation.
- `cargo test -j 1 -p tv-bot-risk-engine` passes after formatting, covering fixed and risk-based sizing, daily-trade and consecutive-loss limits, unrealized-drawdown blocking, broker-required override paths, and broker-preferred warning-only behavior.
- `cargo test -j 1 -p tv-bot-execution-engine -p tv-bot-risk-engine -p tv-bot-runtime-kernel` passes after formatting, covering runtime orchestration of risk evaluation before execution dispatch, quantity adjustment propagation into broker-native order placement, rejected/override-gated no-dispatch behavior, journaling of both risk decisions and hard overrides, and the unified manual/strategy command path into audited execution.
- `cargo test -j 1 -p tv-bot-control-api -p tv-bot-runtime-kernel` passes after formatting, covering control-plane mapping of dashboard and strategy commands onto the unified runtime loop, HTTP-style status translation for executed vs override-required vs internal-error command outcomes, and WebSocket-style event publication for live command results.
- This workspace lives under Windows + OneDrive, so subsequent full-workspace reruns may still hit transient `LNK1104` file-locking on generated test executables. The current post-change full-workspace rerun reproduced that linker lock in unrelated `broker_tradovate` test executables, so serial targeted verification remains the safest retry path when that happens.
