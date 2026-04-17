# Dashboard Chart-First Redesign Plan

## Purpose

The dashboard should now evolve from a chart-first shell into a true chart-centric workspace where:

- the chart is the product
- the header and side surfaces behave like compact toolbars, not dashboard cards
- almost every visible control is actionable or high-signal
- explanatory prose is removed from the primary workspace
- the interface feels sleek, modern, and self-explanatory rather than dense or admin-heavy

This document is the source of truth for the Phase 5 productization pass in `apps/dashboard`.
The separate chart-delivery and data-surface plan still lives in [docs/architecture/dashboard_live_chart_plan.md](</C:/repos/TV_bot_core/docs/architecture/dashboard_live_chart_plan.md>).

## Updated Product Direction

The next design target is more opinionated than the earlier shell-reset pass.

The dashboard should no longer read as:

- top hero
- chart section
- side cards
- lower dashboard tabs

It should read as:

- utility header
- left tool rail
- dominant chart stage
- right trade rail
- compact lower dock

That means:

- no unnecessary title treatment inside the main workspace
- no descriptive paragraphs above the fold unless they are warnings
- no broad card stack in the right rail
- no duplicate status summaries competing with the chart
- no decorative framing that makes the chart feel boxed in

## North-Star Experience

The operator should feel like they opened a polished chart workspace first and a dashboard second.

In the first few seconds, the screen should answer:

- What contract is loaded?
- What timeframe am I on?
- Am I live, paper, or observation?
- Am I armed or blocked?
- Do I have exposure or working orders?
- Is the feed healthy?
- What is the next useful action?

The chart should be the main visual anchor for those answers.

## Design Principles

### 1. Chart Dominance

The chart must own the center of gravity.

Desktop target:

- chart stage should carry roughly `68-78%` of above-the-fold visual emphasis
- right rail should feel like a narrow trade sidebar, not a second content region
- left rail should behave like a compact context or chart-tools rail, not a second dashboard

### 2. Toolbar Mental Model

Header and rails should be designed like toolbars.

That means:

- short labels
- small but readable controls
- grouped actions
- high signal density
- little or no explanatory copy

If a surface is not helping the operator read the chart or act on the chart, it should move to the lower dock.

### 3. Minimal Chrome

The workspace should feel calm and expensive, not boxed in.

That means:

- fewer heavy card borders
- fewer nested panels
- more use of spacing, alignment, and contrast hierarchy
- one subtle chart frame instead of several stacked chart containers

### 4. Progressive Disclosure

The main workspace should contain:

- posture
- chart
- exposure
- actions

Audit, diagnostics, and configuration stay available, but in the dock.

### 5. Self-Explanatory Language

Primary surfaces should use terse operator language:

- `Paper`
- `Armed`
- `Warmup`
- `Orders`
- `Flatten`
- `Cancel`
- `No new entries`

Avoid:

- long detail blurbs
- internal implementation language
- multi-sentence helper text in the active workspace

## Research Snapshot

Research basis for this plan:

- Robinhood Legend product materials already reviewed in the previous planning pass
- TradingView Lightweight Charts official API docs reviewed again for time-scale and history-loading behavior:
  - [ITimeScaleApi](https://tradingview.github.io/lightweight-charts/docs/api/interfaces/ITimeScaleApi)
  - [ISeriesApi](https://tradingview.github.io/lightweight-charts/docs/api/interfaces/ISeriesApi)
  - [ChartOptionsBase](https://tradingview.github.io/lightweight-charts/docs/api/interfaces/ChartOptionsBase)

### Key Additional Takeaways

- `fitContent()`, `setVisibleLogicalRange(...)`, and `setVisibleRange(...)` give us explicit control over first-open framing
- `barsInLogicalRange(...)` plus `subscribeVisibleLogicalRangeChange(...)` is the intended pattern for loading more history before the operator hits empty space
- the chart library already gives us the control we need to open with a filled chart and keep historical paging smooth

## Architecture Guardrails

These rules remain fixed:

- dashboard consumes only the local control plane
- chart stays locked to the currently loaded strategy contract
- frontend is not the source of truth for candles, positions, orders, or readiness
- dangerous actions remain backend-confirmed and audit-logged
- execution core remains strategy-agnostic

## Target Workspace Architecture

### 1. Utility Header

The current header should shrink again and lose any remaining hero feel.

It should contain only:

- mode badge
- arm badge
- warmup or readiness badge
- broker or feed badge
- review-required badge when relevant
- loaded contract or symbol
- refresh

Rules:

- no large title block
- no summary paragraph
- no repeated status language already visible elsewhere

### 2. Left Tool Rail

This should become a compact chart-support rail.

Primary contents:

- contract identity
- strategy identity
- latest price or session snapshot
- quick overlay toggles
- quick chart mode controls if needed

Rules:

- dense, vertically scannable
- minimal text
- no large audit summaries

### 3. Center Chart Stage

This becomes the visual product core.

The chart stage should contain:

- top chart toolbar
- chart canvas
- compact chart readout strip
- optional compact sub-toolbar under the chart only when needed

The chart toolbar should contain only chart-related controls:

- timeframe switcher
- fit
- live follow
- load more history
- overlay toggles
- chart-state badge

### 4. Right Trade Rail

This should be a narrow, dense operator rail with no dashboard-card feel.

The target grouping is:

- posture block
  - mode, arm, warmup, gate, pause or resume
- ticket block
  - side, size, price inputs, submit
- exposure block
  - position count, working orders, flatten, cancel
- safety review block only when active

Rules:

- no long descriptive notes
- use pills, micro-metrics, and short labels
- keep dangerous buttons distinct

### 5. Lower Dock

This remains for slower or deeper work:

- trades
- checks
- setup
- health
- latency
- journal
- events

This area can keep slightly more explanation, but still should not read like documentation.

## Visual Direction

### Tone

- dark
- calm
- precise
- minimal
- premium

### Specific UI Rules

- reduce panel stacking around the chart
- replace thick chart container feel with a subtle frame
- use softer separators and hairlines
- keep chart background neutral
- reserve stronger color for mode, review, and order-state overlays
- avoid bright decorative gradients near the chart itself

### Chart Frame Direction

The chart should have a minimalist border treatment:

- subtle `1px` or near-hairline frame
- low-contrast edge against the workspace background
- modest radius
- no thick nested wrappers
- no oversized panel padding that shrinks the canvas

The chart should feel mounted into the workspace, not placed inside a card.

## Immediate Chart Improvements Required

These chart changes are now first-class requirements, not optional polish.

### A. Open With Enough History To Fill The Chart

The chart should not open sparse or half-empty.

The runtime and dashboard should cooperate so the default chart view:

- loads enough history to fill the visible canvas immediately
- includes left-side overscan so the operator can pan a bit before needing a fetch
- keeps loading more history before the operator hits empty space

Planned policy:

1. On chart bootstrap, request a larger initial history window than the current visible viewport needs.
2. Size the initial target by breakpoint and timeframe.
3. Apply a visible logical range or fit strategy so the screen opens full.
4. Subscribe to visible logical range changes and fetch more bars when the left buffer drops below a threshold.

Recommended starting targets:

- desktop: enough bars for roughly `1.8x-2.5x` the visible viewport
- tablet: enough bars for roughly `1.6x-2.2x`
- mobile: enough bars for roughly `1.4x-2.0x`

Initial working defaults for the first implementation pass:

- `1m`: request `300-500` bars on desktop, `220-320` on tablet, `160-240` on mobile
- `5m`: request enough bars to show a meaningful multi-session window on open
- `1s`: request enough bars to avoid a toy-looking chart while still respecting performance

### B. Infinite-Left History Behavior

The chart should feel continuous.

Implementation plan:

- use Lightweight Charts visible logical range APIs to detect when the operator is nearing the loaded left edge
- fetch older bars through `/chart/history`
- prepend them without snapping the operator out of context
- expose `Load older` as a fallback, but rely on background prefetch first

### C. Minimal Chart Border

The chart frame should be visually quieter.

Implementation plan:

- remove heavy chart panel padding
- reduce border contrast
- simplify chart frame layers
- keep only one primary chart edge treatment
- tune grid and pane lines so they feel deliberate but understated

## Implementation Plan

### Phase 1: Chrome Reduction

- remove remaining hero-style header treatment
- convert header into a true utility strip
- remove non-warning prose above the fold
- reduce duplicate status wording

### Phase 2: Toolbar-Driven Layout

- convert left rail into a real tool/context rail
- convert right rail into a narrow trade rail
- keep only the most important controls visible
- move everything secondary into the dock

### Phase 3: Chart Stage Expansion

- enlarge the chart footprint further
- reduce chart wrapper padding
- simplify chart frame styling
- make readout strip and toolbar denser

### Phase 4: Chart Data Loading Upgrade

- add enough first-open history to fill the chart
- add viewport-aware history sizing
- add logical-range-triggered historical prefetch
- keep explicit `Load older` as backup

### Phase 5: Minimal Visual Sweep

- reduce border noise
- unify spacing rhythm
- tighten typography
- replace verbose copy with short labels
- ensure no toolbar feels redundant

### Phase 6: Responsive And Sign-Off Pass

- verify chart stays dominant at `390px`, `768px`, `1024px`, and `1440px`
- verify no horizontal overflow
- verify trade rail remains usable without crowding the chart
- verify dock still reads clearly on smaller widths

## Suggested Module Targets

The current component split is good enough to keep building on.

The next restructuring target should be:

- `apps/dashboard/src/components/layout/dashboardUtilityHeader.tsx`
- `apps/dashboard/src/components/chartStage/dashboardContextRail.tsx`
- `apps/dashboard/src/components/chartStage/dashboardTradeRail.tsx`
- `apps/dashboard/src/components/chartStage/dashboardChartToolbar.tsx`
- `apps/dashboard/src/components/chartStage/dashboardChartStage.tsx`
- `apps/dashboard/src/components/docks/dashboardBottomDock.tsx`

The chart-specific data work should continue to live with:

- `apps/dashboard/src/hooks/useDashboardChart.ts`
- `apps/dashboard/src/lib/chartAdapter.ts`

## Acceptance Bar

This redesign is not complete unless all of the following are true:

- the chart is unmistakably the primary surface above the fold
- the header reads like a utility strip, not a hero
- the side surfaces feel like toolbars, not card stacks
- there is little or no explanatory prose above the fold
- the chart opens with enough history to feel full and useful
- the chart keeps loading history before the operator reaches empty space
- the chart frame feels minimal and premium
- live, paper, and observation remain unmistakable
- no supported width has page-level horizontal overflow

## Immediate Work Order

1. Treat this revised document as the source of truth for the next chart-first pass.
2. Strip the header down into a utility-only bar.
3. Convert the left and right rails into denser toolbar-style surfaces.
4. Expand the chart stage and reduce chart wrapper chrome.
5. Upgrade chart bootstrap and historical paging so the chart opens full.
6. Run another responsive and browser QA sweep after the chart-stage resize.

## Documentation Follow-Up

Keep these docs aligned while this pass lands:

- [README.md](</C:/repos/TV_bot_core/README.md>)
- [apps/dashboard/README.md](</C:/repos/TV_bot_core/apps/dashboard/README.md>)
- [docs/architecture/current_status.md](</C:/repos/TV_bot_core/docs/architecture/current_status.md>)
- [docs/architecture/dashboard_live_chart_plan.md](</C:/repos/TV_bot_core/docs/architecture/dashboard_live_chart_plan.md>)
