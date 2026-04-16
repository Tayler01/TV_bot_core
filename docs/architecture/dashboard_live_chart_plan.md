# Dashboard Live Chart Plan

## Purpose

Add a live, operator-grade contract chart to the local dashboard without violating the boundaries in [AGENTS.md](</C:/repos/TV_bot_core/AGENTS.md>).
The chart must:

- render only the currently loaded strategy contract
- consume only the local HTTP and WebSocket control plane
- stay subordinate to backend truth for candles, positions, orders, fills, and health
- preserve the strategy-agnostic execution core
- fit the existing dark-first dashboard shell

This plan covers both the frontend charting surface and the backend chart-data/control-plane work required to support it cleanly.

## Progress Snapshot

Current state as of 2026-04-15:

- The planning and architectural recommendation in this document are complete.
- Phase 1 `chart control-plane foundation` is now materially in place in [apps/runtime/src/host.rs](</C:/repos/TV_bot_core/apps/runtime/src/host.rs>) and [crates/control_api/src/lib.rs](</C:/repos/TV_bot_core/crates/control_api/src/lib.rs>) through `GET /chart/config`, `GET /chart/snapshot`, `GET /chart/history`, and `GET /chart/stream`.
- The host now exposes chart wire models, strategy-driven timeframe negotiation, in-memory candle pagination from market-data buffers, and symbol-scoped active-position or working-order or recent-fill overlay projection for the currently loaded contract.
- Phase 2 `chart shell and toolbar` is now in place in [apps/dashboard/src/components/dashboardLiveChart.tsx](</C:/repos/TV_bot_core/apps/dashboard/src/components/dashboardLiveChart.tsx>), [apps/dashboard/src/hooks/useDashboardChart.ts](</C:/repos/TV_bot_core/apps/dashboard/src/hooks/useDashboardChart.ts>), and [apps/dashboard/src/lib/chartAdapter.ts](</C:/repos/TV_bot_core/apps/dashboard/src/lib/chartAdapter.ts>) with a dark live chart module, timeframe switching, chart-stream updates, buffered history paging, and strategy-driven chart defaults.
- Phase 3 is partially in place: active-position context and recent fill markers now render on the chart, while working orders are currently summarized beside the chart rather than drawn as exact price bands because the current `BrokerOrderUpdate` wire model does not expose working-order price levels.
- The remaining work is polish and deeper overlay fidelity rather than first delivery.

## Scope Guardrails

These rules are fixed for V1:

- The dashboard must not call Databento or Tradovate directly.
- The chart symbol must be locked to the currently loaded strategy contract.
- The chart must not become a second source of truth for positions, orders, or readiness.
- Strategy files may influence chart defaults through compiled metadata, but chart logic must not embed strategy-specific execution behavior.
- Interactive trading from chart gestures is out of scope for the first delivery slice; chart actions continue to flow through audited runtime commands.

## Current Repo Reality

The repository already has several important building blocks:

- [crates/core_types/src/lib.rs](</C:/repos/TV_bot_core/crates/core_types/src/lib.rs>) already defines `Timeframe` values for `1s`, `1m`, and `5m`.
- [crates/core_types/src/lib.rs](</C:/repos/TV_bot_core/crates/core_types/src/lib.rs>) already defines `MarketEvent::Bar` with `open`, `high`, `low`, `close`, `volume`, and `closed_at`.
- [crates/market_data/src/lib.rs](</C:/repos/TV_bot_core/crates/market_data/src/lib.rs>) already maintains rolling warmup buffers and can expose bars by timeframe, including existing `5m` aggregation from a smaller provider feed.
- [crates/core_types/src/lib.rs](</C:/repos/TV_bot_core/crates/core_types/src/lib.rs>) already includes `dashboard_display.preferred_chart_timeframe` in compiled strategy metadata.
- [apps/dashboard/src/types/controlApi.ts](</C:/repos/TV_bot_core/apps/dashboard/src/types/controlApi.ts>) and [apps/dashboard/src/lib/api.ts](</C:/repos/TV_bot_core/apps/dashboard/src/lib/api.ts>) already model the runtime status, readiness, history, journal, settings, and event-feed surfaces.
- [apps/dashboard/src/components/dashboardMonitoring.tsx](</C:/repos/TV_bot_core/apps/dashboard/src/components/dashboardMonitoring.tsx>) already renders a dark monitoring deck, but its only chart today is the PnL visualization, not a real contract price chart.

What is still missing:

- a chart-specific control-plane snapshot endpoint
- a chart-specific history pagination endpoint
- a chart-specific live stream that does not flood the operator event feed
- a dashboard contract-chart component and controller hook
- runtime-backed overlays for active positions, working orders, fills, and chart health state

## Research Snapshot

Research date: 2026-04-15

Official documentation reviewed:

- TradingView Lightweight Charts official docs: [Home](https://tradingview.github.io/lightweight-charts/), [time scale](https://tradingview.github.io/lightweight-charts/docs/4.1/time-scale), [series API](https://tradingview.github.io/lightweight-charts/docs/api/interfaces/ISeriesApi), and [pane primitives](https://tradingview.github.io/lightweight-charts/docs/next/plugins/pane-primitives)
- TradingView Advanced Charts official docs: [Widget Constructor](https://www.tradingview.com/charting-library-docs/latest/core_concepts/Widget-Constructor/)
- Highcharts Stock official docs: [Stock Tools](https://www.highcharts.com/docs/stock/stock-tools)
- Apache ECharts official docs: [Axis Concepts](https://echarts.apache.org/handbook/en/concepts/axis/)

### Option Review

| Option | Strengths | Weaknesses | Repo Fit |
| --- | --- | --- | --- |
| TradingView Lightweight Charts | Finance-native candlestick focus, real-time series updates, time-scale APIs, price lines, markers, pane support, and custom primitives for overlays | No turnkey trading terminal, drawings, or full indicator workbench out of the box | Best V1 fit |
| TradingView Advanced Charts | Full widget with symbol/interval/timeframe/time-zone config, featuresets, studies, drawings, and trading-platform order or position line support | Much heavier integration, custom datafeed contract, static library hosting, and a bigger product decision than this repo currently needs for first delivery | Good later upgrade path, not the first implementation |
| Highcharts Stock | Strong stock-tools GUI for annotations, indicators, and zoom | Commercial licensing and a heavier general-purpose UI layer than we need for the first operator chart | Viable but less attractive than Lightweight Charts for V1 |
| Apache ECharts | Flexible charting engine, crosshair and zoom concepts, broad visualization toolbox | More custom finance behavior and UX work than a finance-native library | Better for bespoke analytics than the first trading chart |

## Recommendation

Use **TradingView Lightweight Charts** for the first live contract-chart delivery, and shape the backend chart data contract so we can add a future **TradingView Advanced Charts** adapter later if we decide we need full drawings, studies, and terminal-style layout tooling.

This is the best fit for the current repository because:

- it gives us a finance-native candlestick surface without pulling a full broker terminal into the dashboard immediately
- it supports the operator overlays we need now through price lines, markers, panes, and custom primitives
- it keeps the dashboard in control of the visual shell while the runtime remains the source of truth
- it minimizes licensing and packaging complexity for the first pass

The plan below keeps the chart API renderer-agnostic so the dashboard can evolve later without redesigning the runtime host again.

## Target Operator Experience

The finished charting surface should behave like a real operator console, not a toy widget.

### Core Experience

- One chart, one contract: the chart always follows the contract resolved from the currently loaded strategy.
- Candlestick chart with live updates and dark-mode visuals aligned with the rest of the dashboard.
- Volume pane beneath the main price pane.
- Timeframe switcher for host-supported frames.
- Crosshair, OHLC readout, visible-range zoom and pan, and a `fit` control.
- Live-follow mode that can be paused when the operator pans into older history.
- Clear market-data and broker-health badges near the chart.

### Runtime Overlays

- Current price line
- Average-entry line for the active position
- Active position badge with side, quantity, average price, and unrealized PnL
- Working-order lines for entry, stop, target, and other broker-side protections when present
- Fill markers for recent executions
- Visual banners for degraded feed, reconnect-review-required, or no-strategy-loaded states

### Operator Controls

- Default timeframe loaded from the compiled strategy display preference when valid
- Manual timeframe switching without symbol switching
- Overlay toggles for positions, orders, fills, and volume
- Reset-view and jump-to-now controls
- Readable chart legend showing contract, timeframe, feed state, and replay/live state

## Backend Architecture Plan

The live chart cannot be added as frontend-only work.
The runtime host needs a chart-specific data surface that remains separate from the generic operator event feed.

### New Control-Plane Surfaces

Add a dedicated chart API family to the runtime host:

- `GET /chart/config`
- `GET /chart/snapshot?timeframe=1m&limit=500`
- `GET /chart/history?timeframe=1m&before=<timestamp>&limit=500`
- `GET /chart/stream` as a dedicated WebSocket

Do **not** multiplex high-frequency bar updates onto the existing `/events` stream.
That stream is an operator audit feed and should stay readable.

### Suggested Response Shapes

Define new chart-specific wire types in [crates/control_api/src/lib.rs](</C:/repos/TV_bot_core/crates/control_api/src/lib.rs>) and mirror them in [apps/dashboard/src/types/controlApi.ts](</C:/repos/TV_bot_core/apps/dashboard/src/types/controlApi.ts>).

Suggested top-level models:

- `RuntimeChartConfigResponse`
- `RuntimeChartSnapshot`
- `RuntimeChartHistoryResponse`
- `RuntimeChartStreamEvent`

Suggested payload fields:

- instrument identity and display metadata
- resolved Tradovate and Databento symbols
- current account routing context
- supported timeframes
- default timeframe
- chart bars with OHLCV and completion state
- position overlay snapshot
- working-order overlay snapshot
- fill marker list
- latest price and latest closed bar time
- replay/live/degraded state
- chart capability flags such as `can_load_older_history`

### Data Sources

Use runtime-owned sources only:

- Live bars from the existing [crates/market_data/src/lib.rs](</C:/repos/TV_bot_core/crates/market_data/src/lib.rs>) rolling buffers
- Older history from runtime-owned historical fetches through the market-data adapter, never from the dashboard
- Positions, working orders, fills, and PnL context from the runtime projection and broker-sync state already surfaced through history/status

For the first delivery pass, do not introduce a separate persistent candle store unless runtime fetch latency proves unacceptable.
Keep the first design provider-backed and buffer-backed, with a later option to add a local candle cache.

### Timeframe Policy

Initial host-supported chart timeframes should match the current runtime enum and existing market-data support:

- `1s`
- `1m`
- `5m`

The host should advertise supported frames rather than letting the dashboard assume them.

Default timeframe selection should follow this order:

1. `dashboard_display.preferred_chart_timeframe` if it validates to a supported enum value
2. smallest strategy-required timeframe
3. `1m`

Future higher intervals such as `15m`, `30m`, `1h`, and `1d` should be added only after the host can aggregate or query them cleanly.
The chart API should be forward-compatible with that expansion now.

### State Ownership

The backend owns:

- candle history
- candle completion state
- market-data health
- overlay truth for positions, orders, and fills
- contract identity
- supported timeframes

The frontend owns only:

- current selected timeframe
- local zoom or pan state
- local overlay visibility toggles
- live-follow on or off

## Frontend Architecture Plan

### New Dashboard Modules

Add a dedicated chart surface instead of growing the existing monitoring file again.

Suggested additions:

- `apps/dashboard/src/components/dashboardLiveChart.tsx`
- `apps/dashboard/src/hooks/useDashboardChart.ts`
- `apps/dashboard/src/lib/chartAdapter.ts`
- `apps/dashboard/src/lib/chartFormat.ts`

Keep [apps/dashboard/src/App.tsx](</C:/repos/TV_bot_core/apps/dashboard/src/App.tsx>) as composition only.
The chart should become another focused dashboard module, not a new mega-component.

### Rendering Strategy

Use a thin React wrapper around Lightweight Charts with imperative chart creation via refs.

Responsibilities:

- `useDashboardChart.ts`
  - fetch chart config and snapshot
  - manage timeframe changes
  - manage pagination requests for older history
  - own the dedicated chart WebSocket subscription
  - merge stream updates into a local chart view model
- `chartAdapter.ts`
  - convert runtime wire models into Lightweight Charts series data and overlay models
  - isolate the renderer-specific mapping layer so the host API stays reusable
- `dashboardLiveChart.tsx`
  - render the toolbar, badges, legend, chart panes, and empty/degraded states

### UI Placement

The chart should become a primary monitoring surface, not an afterthought beneath the audit cards.

Recommended layout:

- place the live contract chart directly below the top operator rail and above the lower monitoring or audit deck
- let the chart span the main content width on desktop
- stack the toolbar and legend safely on mobile widths

### Dark-Mode Behavior

The chart must adopt the same production-dark token system tracked in [docs/architecture/dashboard_production_ui_plan.md](</C:/repos/TV_bot_core/docs/architecture/dashboard_production_ui_plan.md>).

Requirements:

- paper, live, and observation state accents must remain unmistakable
- stop, target, position, and fill overlays must have distinct and accessible colors
- long contract labels, account names, or status strings must wrap or truncate safely

## Delivery Phases

### Phase 1: Chart Control-Plane Foundation

- Add chart types to `crates/control_api`
- Add host routes in `apps/runtime`
- Expose chart config, initial snapshot, and historical pagination
- Add dedicated chart stream route
- Add host tests for no-strategy, loaded-strategy, timeframe validation, and chart snapshot serialization

### Phase 2: Chart Shell And Toolbar

- Add `lightweight-charts` to `apps/dashboard/package.json`
- Build the chart controller hook and adapter layer
- Render the candlestick pane, volume pane, timeframe chips, fit control, live-follow control, and health badges
- Honor strategy-driven default timeframe

### Phase 3: Live Overlays

- Add position overlay lines and labels
- Add working-order overlays and protective-order labeling
- Add recent fill markers
- Add degraded-feed, replay, and reconnect-review banners inside the chart module

### Phase 4: Load-More History And Performance Hardening

- Support pan-left history pagination through `GET /chart/history`
- Batch incremental updates to avoid React thrash at `1s` cadence
- Verify chart performance on supported viewport sizes and larger history payloads

### Phase 5: Operator Polish And Acceptance

- Tune legend density, hover copy, and overlay toggles
- Add responsive QA and overflow checks for chart toolbars and legends
- Reconcile chart states with readiness, mode, and reconnect-review states
- Update documentation, screenshots, and operator runbooks if the chart becomes part of release sign-off

## Test Plan

### Rust And Host Tests

- unit tests for timeframe negotiation and default-timeframe selection
- host tests for chart endpoints with and without a loaded strategy
- host tests for invalid timeframe requests
- host tests proving the chart stream is isolated from the generic `/events` feed
- host tests proving position, order, and fill overlays reflect the current runtime projection

### Frontend Tests

- rendering tests for:
  - no strategy loaded
  - snapshot loaded
  - degraded feed
  - reconnect-review-required
  - timeframe switch
- adapter tests that map runtime chart models into series data and overlays correctly
- event-merge tests for incremental bar and overlay updates

### Browser QA

- responsive checks at `390px`, `768px`, `1024px`, and `1440px`
- verify no page-level horizontal overflow with the chart mounted
- verify toolbars and legends do not clip or spill
- verify chart remains readable in dark mode across paper, live, and observation status accents

## Acceptance Bar

The live chart work is not done unless all of the following are true:

- the chart only ever shows the currently loaded contract
- the chart can load and update without any direct provider calls from the dashboard
- the default timeframe follows compiled strategy display metadata when valid
- timeframe switching works across all host-advertised chart frames
- live candles update correctly through the dedicated chart stream
- active positions, working orders, and recent fills are visible on-chart
- degraded/reconnect states are visible on the chart and do not mislead the operator
- the chart fits the dark operator-console design without layout spill or horizontal overflow
- host and frontend tests cover the chart data path and high-risk states

## Open Questions And Risks

- How much older history should V1 guarantee per timeframe before on-demand pagination is required?
- Do we want tooltips to display exchange time, local time, or both?
- Should higher intervals such as `15m` and `1h` be aggregated in host memory or fetched explicitly from the market-data adapter?
- If the product later needs built-in drawings and full indicator tooling, do we adopt TradingView Advanced Charts after the first release path is stable?

## Recommended Immediate Work Order

1. Define the chart control-plane contract in `crates/control_api`.
2. Add runtime-host chart routes and chart-stream tests.
3. Add the dashboard chart hook plus Lightweight Charts shell.
4. Add overlays for active positions, working orders, and fills.
5. Add load-more history, responsive QA, and final operator polish.

## Documentation Follow-Up

Keep these files aligned as chart work lands:

- [README.md](</C:/repos/TV_bot_core/README.md>)
- [apps/dashboard/README.md](</C:/repos/TV_bot_core/apps/dashboard/README.md>)
- [docs/architecture/current_status.md](</C:/repos/TV_bot_core/docs/architecture/current_status.md>)
- [docs/architecture/dashboard_production_ui_plan.md](</C:/repos/TV_bot_core/docs/architecture/dashboard_production_ui_plan.md>)
