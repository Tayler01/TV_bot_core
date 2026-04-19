# CLI Console Plan

## Purpose

Add a terminal operator console to the existing CLI in [apps/cli](/C:/repos/TV_bot_core/apps/cli) that provides a simplified live-operations surface similar to the web dashboard, without violating the boundaries in [AGENTS.md](/C:/repos/TV_bot_core/AGENTS.md).

The console must:

- consume only the local HTTP and WebSocket control plane
- remain subordinate to backend truth for mode, arming, readiness, chart data, fills, and activity
- preserve the strategy-agnostic execution core
- keep live and paper modes impossible to confuse
- require explicit confirmation for dangerous actions
- work on Windows, Linux, and macOS terminals

This plan defines the V1 scope, operator workflows, architecture, delivery phases, and acceptance bar for the CLI console.

## Planning Status

Current state as of 2026-04-18:

- The repository already has a standalone CLI in [apps/cli/src/main.rs](/C:/repos/TV_bot_core/apps/cli/src/main.rs) for launch, status, readiness, history, lifecycle commands, reconnect review, shutdown review, and flatten.
- The runtime host already exposes the main control-plane surfaces needed for a terminal console through [apps/runtime/src/host.rs](/C:/repos/TV_bot_core/apps/runtime/src/host.rs).
- The shared wire contracts already exist in [crates/control_api/src/lib.rs](/C:/repos/TV_bot_core/crates/control_api/src/lib.rs).
- The dashboard already proves the viability of the chart, event feed, and lifecycle-command model through the local control API only.
- The missing work is primarily a terminal UI surface, CLI-side state management, terminal rendering, keyboard interaction, and console-specific tests.

## Scope Guardrails

These rules are fixed for V1:

- The terminal console must not call Databento or Tradovate directly.
- The terminal console must not become a second source of truth for positions, orders, readiness, or chart state.
- The console must only show the currently loaded strategy contract.
- Strategy files may influence defaults through already compiled runtime metadata, but the console must not parse or interpret strategy Markdown itself.
- All operator actions must continue to flow through the audited runtime command path.
- The console must not introduce a generic freeform command shell for trading actions in V1.
- The console must not add ambiguous verbs such as `stop bot` if the actual behavior is `pause`, `disarm`, or `shutdown`.

## V1 Product Decision

The first release should be a terminal console mode added to the existing CLI binary, not a separate app.

Recommended command surface:

- `tv-bot-cli console`

This keeps packaging simple, preserves the existing CLI workflows, and lets operators choose between one-shot commands and the persistent terminal console without splitting the product.

## V1 Operator Experience

The V1 console should behave like a compact trading workstation.

### Main Layout

- Top summary rail
  - mode
  - arm state
  - warmup status
  - strategy and contract
  - broker health
  - market-data health
  - reconnect-review-required state
  - dispatch readiness
- Center chart pane
  - live chart for the currently loaded strategy contract only
  - current price
  - latest candle timestamp
  - clear loading, degraded, and unavailable states
- Bottom activity feed
  - newest-first operator activity and transaction flow
  - fills
  - order changes
  - command results
  - major warnings and health transitions
- Footer help bar
  - key bindings
  - connection state
  - current timeframe if shown

### Primary Operator Actions

The V1 console should support:

- switch to `paper`
- switch to `observation`
- switch to `live`
- `arm`
- `disarm`
- `pause`
- `resume`
- refresh snapshot
- quit console

The V1 console should not include these actions initially:

- manual entry
- flatten
- close position
- cancel working orders
- runtime shutdown

Those can be added later after the core operator flow is stable.

### Clarified Start And Stop Semantics

The console must not use vague labels such as `start bot` or `stop bot`.

V1 should map user intent to explicit actions:

- `Start in paper`: set mode to `paper`
- `Start in observation`: set mode to `observation`
- `Start live path`: set mode to `live`
- `Enable trading`: `arm`
- `Disable trading`: `disarm`
- `Temporarily halt runtime`: `pause`
- `Continue runtime`: `resume`

If later product language wants a `Start` or `Stop` affordance, it should be implemented as a composite workflow with explicit intermediate confirmations, not as a hidden shorthand.

## V1 Assumptions

The plan below assumes:

- the console is keyboard-driven
- the chart defaults to the strategy-preferred or runtime-advertised default timeframe
- the console initially supports a fixed timeframe, with optional timeframe switching if low risk
- the bottom feed includes both transaction activity and important warnings
- terminal operators need a lighter remote-friendly surface, not a full dashboard replacement on day one

## Control-Plane Reuse

The console should reuse the existing runtime-host surfaces:

- `GET /status`
- `GET /readiness`
- `GET /history`
- `GET /journal`
- `GET /chart/config`
- `GET /chart/snapshot`
- `GET /chart/history`
- `POST /runtime/commands`
- `WS /events`
- `WS /chart/stream`

These are already rooted in [apps/runtime/src/host.rs](/C:/repos/TV_bot_core/apps/runtime/src/host.rs) and modeled in [crates/control_api/src/lib.rs](/C:/repos/TV_bot_core/crates/control_api/src/lib.rs).

The CLI console must not add a direct broker or market-data code path.

## Activity Feed Definition

The bottom pane should be a normalized operator feed, not just a raw event dump.

### V1 Feed Content

- command results from `/events`
- journal records from `/events`
- history projection changes relevant to:
  - latest order
  - latest fill
  - latest position
- broker health changes
- market-data degradation or recovery
- readiness changes when they materially affect operator action

### V1 Feed Sources

Use these existing sources first:

- initial bootstrap from `/history` and `/journal`
- live updates from `/events`

### V1 Feed Rule

Prefer meaningful operator messages over raw payloads.

Example feed lines:

- `fill buy 1 MESM6 @ 5234.25`
- `order working stop sell 1 MESM6 @ 5228.25`
- `runtime armed in paper mode`
- `readiness warning: primary database unavailable`
- `broker sync degraded`

### Future Option

If composing the feed from `/events` and `/history` becomes noisy or inconsistent, add a dedicated backend feed projection later.
That is not required for V1.

## Chart Plan

The chart should be simple, stable, and terminal-appropriate.

### V1 Chart Requirements

- only the currently loaded strategy contract
- render from runtime-host chart APIs only
- show recent bars clearly in a terminal
- show latest price and latest candle time
- show chart-unavailable reason when the host reports it
- show degraded state clearly

### V1 Timeframe Policy

Default behavior:

- use the runtime-advertised default timeframe

Optional V1 enhancement:

- allow switching among host-supported timeframes using number keys if implementation remains low risk

If timeframe switching causes schedule risk, defer it and keep the console fixed to the default timeframe for the first release.

### V1 Chart Rendering Direction

Use a terminal-native chart approach:

- lightweight candlestick-like glyphs if feasible
- otherwise a sparkline or mini-bar renderer with clear axis labels

The goal is legibility and operator awareness, not pixel-perfect parity with the dashboard.

### V1 Chart Overlays

Required:

- latest price indication

Strongly preferred if cheap:

- active position marker
- recent fill marker

Deferred:

- full working-order overlay rendering
- multiple overlay toggles
- advanced zoom and pan behavior

## Interaction Plan

The console should be keyboard-first with explicit confirmations.

### Suggested Key Map

- `p`: switch mode to paper
- `o`: switch mode to observation
- `l`: switch mode to live
- `a`: arm
- `d`: disarm
- `space`: pause or resume
- `r`: refresh all snapshots
- `?`: show help overlay
- `q`: quit console

Optional if timeframe switching lands:

- `1`, `2`, `3`: switch timeframe among runtime-host-supported values

### Confirmation Rules

Always confirm:

- switching to live mode
- arming while in live mode
- arming with override

Do not confirm by default:

- switching between observation and paper
- disarm
- pause
- resume
- refresh
- quit console

### Dangerous-State Visibility

The console must prominently display:

- live mode
- hard override active
- reconnect review required
- shutdown review pending
- degraded broker state
- degraded market-data state
- dispatch unavailable

## CLI Architecture Plan

The current [apps/cli/src/main.rs](/C:/repos/TV_bot_core/apps/cli/src/main.rs) is already sizable.
The console work should split the CLI into modules instead of growing one file further.

### Proposed Module Layout

- `apps/cli/src/main.rs`
  - argument parsing and subcommand dispatch only
- `apps/cli/src/commands.rs`
  - existing one-shot CLI commands
- `apps/cli/src/console/mod.rs`
  - console entrypoint
- `apps/cli/src/console/app.rs`
  - top-level event loop and state coordination
- `apps/cli/src/console/state.rs`
  - terminal view state and projections
- `apps/cli/src/console/render.rs`
  - `ratatui` drawing code
- `apps/cli/src/console/input.rs`
  - key handling and confirmation flow
- `apps/cli/src/console/api.rs`
  - snapshot fetches, WebSocket subscriptions, command posting
- `apps/cli/src/console/feed.rs`
  - event normalization into terminal feed items
- `apps/cli/src/console/chart.rs`
  - terminal chart shaping and chart-stream merge logic
- `apps/cli/src/console/commands.rs`
  - mapping operator actions to existing runtime lifecycle commands

### Dependency Direction

The console modules may depend on:

- `tv-bot-config`
- `tv-bot-control-api`
- `tv-bot-core-types`
- `reqwest`
- `tokio`
- terminal UI crates

The console modules must not depend on:

- broker adapters
- market-data adapters
- strategy loader internals
- dashboard code

## Suggested Library Choices

Recommended crates:

- `ratatui`
- `crossterm`

Why:

- cross-platform terminal support
- stable Windows support
- clear separation between event handling and drawing
- enough flexibility for a chart pane, status widgets, and confirmation dialogs

## Runtime State Model

The console should maintain one local state object built from backend truth.

### State Domains

- snapshot state
  - latest `status`
  - latest `readiness`
  - latest `history`
  - latest `journal`
- connection state
  - snapshot polling health
  - `/events` WebSocket health
  - `/chart/stream` WebSocket health
- chart state
  - config
  - current timeframe
  - current bars
  - latest price
  - availability detail
- activity feed state
  - normalized recent feed items
  - severity
  - timestamp
- UI state
  - active confirmation dialog
  - help overlay open or closed
  - selected focus region if needed
  - terminal size adaptation flags

### Ownership Rule

The CLI console may cache and derive view models, but backend snapshots and stream events remain the source of truth.

## Rendering Plan

### Desktop And Wide Terminal Layout

- top summary rail across full width
- chart in the main center region
- activity feed below chart
- footer help strip

### Narrow Terminal Layout

When terminal width is constrained:

- keep top summary compact
- shorten labels
- reduce feed detail
- preserve live or paper mode visibility above all else

If the terminal is too small for safe readability, show a clear message rather than rendering misleadingly truncated controls.

## Error And Recovery Plan

The console must degrade gracefully.

### Required States

- runtime host unavailable
- chart unavailable because no strategy is loaded
- chart unavailable because market data is unavailable
- event stream disconnected and retrying
- chart stream disconnected and retrying
- command failed with conflict
- command failed because override is required

### Recovery Behavior

- snapshot polling should continue even if WebSockets disconnect
- `/events` and `/chart/stream` should reconnect with bounded retry delay
- chart state should survive temporary stream drops
- the feed should show connection interruptions as operator-visible events

## Documentation Plan

When implementation begins, align these docs:

- [docs/ops/cli_standalone.md](/C:/repos/TV_bot_core/docs/ops/cli_standalone.md)
- [docs/ops/debugging_guide.md](/C:/repos/TV_bot_core/docs/ops/debugging_guide.md)
- [README.md](/C:/repos/TV_bot_core/README.md)
- [docs/architecture/current_status.md](/C:/repos/TV_bot_core/docs/architecture/current_status.md)

Consider adding:

- `docs/ops/cli_console.md`

## Delivery Phases

### Phase 1: Spec And CLI Refactor

- add this planning document
- split existing CLI command code out of `main.rs`
- add a new `console` subcommand
- add terminal UI dependencies
- define app state, render loop, and input abstractions

### Phase 2: Read-Only Console

- render top summary rail from `/status` and `/readiness`
- render feed pane from `/history`, `/journal`, and `/events`
- render loading, unavailable, and degraded states
- add reconnect handling for event streams

### Phase 3: Chart Pane

- fetch `/chart/config`
- fetch `/chart/snapshot`
- subscribe to `/chart/stream`
- render terminal chart
- render latest price and chart status detail

### Phase 4: Interactive Controls

- mode switching
- arm and disarm
- pause and resume
- live-mode confirmation
- live-arm confirmation
- override-aware arm confirmation

### Phase 5: Polish And Hardening

- help overlay
- narrow-terminal behavior
- optional timeframe switching
- feed compaction and severity styling
- better chart markers if cheap and stable

### Phase 6: Tests And Operator Sign-Off

- unit coverage
- integration coverage
- cross-platform terminal validation
- docs updates

## Implementation Order

Recommended build order:

1. Refactor CLI structure so the console has a clean home.
2. Add a read-only status and feed shell.
3. Add chart bootstrap and chart streaming.
4. Add interactive commands and confirmations.
5. Add polish and test hardening.

This ordering keeps the first visible milestone low risk and useful.

## Test Plan

Implementation is not complete without tests.

### Unit Tests

- keybinding to operator-action mapping
- operator-action to runtime-lifecycle-command mapping
- confirmation rules for live mode and live arming
- feed normalization from control-plane events
- chart data mapping into terminal-renderable bars
- narrow-terminal layout fallback rules

### Integration Tests

- console bootstrap with runtime host available
- console bootstrap with no strategy loaded
- console bootstrap with chart available
- `/events` reconnect behavior
- `/chart/stream` reconnect behavior
- mode switch to paper
- mode switch to live with confirmation gate
- arm in paper mode
- arm in live mode with confirmation gate
- arm with override-required flow
- disarm flow
- pause and resume flow
- degraded broker or market-data warning surfacing
- reconnect-review-required surfacing

### Manual QA

- Windows terminal
- Linux terminal
- macOS terminal
- narrow width
- wide width
- resize while running
- event-stream interruption and recovery

## Acceptance Bar

The CLI console is not done unless all of the following are true:

- it only ever shows the currently loaded strategy contract
- it uses only the local runtime host HTTP and WebSocket control plane
- live and paper modes are visually unmistakable
- arm, disarm, pause, and mode changes flow through the audited runtime command path
- dangerous live actions require confirmation
- degraded and review-required states are visible and understandable
- the activity feed is useful to operators, not just a raw payload dump
- the console works on Windows, Linux, and macOS
- safety-critical behavior has automated tests

## Deferred Work

These are explicitly out of scope for the first release:

- direct manual entry from the console
- flatten and close-position shortcuts
- cancel-working-order shortcuts
- process shutdown controls
- advanced chart pan and zoom
- full working-order chart overlays
- multi-contract or symbol switching
- terminal-side strategy loading workflows

## Risks And Mitigations

### Risk: Terminal chart becomes too complex too early

Mitigation:

- start with a simple but readable renderer
- keep overlays minimal
- prioritize correctness and operator clarity over fidelity

### Risk: Event feed becomes noisy

Mitigation:

- normalize events into operator-focused feed items
- keep a bounded recent-item buffer
- add filtering only after the base feed is stable

### Risk: Windows terminal quirks

Mitigation:

- use `crossterm`
- validate early on Windows
- keep input and rendering isolated behind small modules

### Risk: Ambiguous start or stop behavior

Mitigation:

- use explicit verbs in the UI
- confirm dangerous actions
- never overload `stop` to mean multiple things

## Recommended Immediate Next Step

Start implementation with Phase 1 and Phase 2 together:

- refactor the CLI into modules
- add the `console` subcommand
- render a read-only terminal shell with top status rail and bottom activity feed

That gives the project a usable operator surface quickly while keeping the first implementation slice small, testable, and safe.
