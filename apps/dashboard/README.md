# Dashboard

This package contains the local React dashboard for the trading runtime.

The dashboard consumes only the local control plane:

- `GET /health`
- `GET /status`
- `GET /readiness`
- `GET /chart/config`
- `GET /chart/snapshot`
- `GET /chart/history`
- `GET /history`
- `GET /journal`
- `GET /settings`
- `GET /strategies`
- `POST /strategies/upload`
- `POST /strategies/validate`
- `POST /settings`
- `POST /runtime/commands`
- `GET /events`
- `GET /chart/stream` (WebSocket)

## Current Slice

The current dashboard now covers:

- a dark-first shell refresh with a higher-signal operator status rail for mode, arm, readiness, warmup, dispatch, and safety posture
- a grouped control center that separates mode/gating, strategy/settings, and execution-facing operator actions with a denser command-summary rail, dedicated form-grid rules for settings/manual actions, and tighter high-frequency action groupings
- a denser monitoring and audit deck that gives history, latency, journal, and live event surfaces clearer section hierarchy and cleaner empty states
- shared dashboard primitives plus extracted monitoring, control-center, and safety-review components, with the polling/event/command controller now split into runtime-host, strategy-workflow, and settings-workflow hooks and the projection helpers living in a dedicated lib module so the redesign is no longer trapped inside one oversized `App.tsx`
- explicit runtime mode with strong paper/live separation
- strategy library upload, browsing, and host-backed validation before load
- host-backed runtime settings editing for startup mode, default strategy path, SQLite fallback policy, and paper/live account routing names
- strategy load through the audited runtime lifecycle command path
- warmup, arm/disarm, pause/resume, mode switch, explicit `disable new entries`, manual entry, direct flatten/current-position close, and cancel-working-orders controls
- reconnect and shutdown review action cards through the runtime lifecycle command path
- local `/events` WebSocket operator feed for journal, command, readiness, history, and health updates
- account routing
- grouped readiness checks
- broker, feed, storage, and host health with connectivity clocks and feed/storage detail
- history and PnL drill-downs including an explicit real-time P&L chart, per-trade P&L cards, richer trade ledger, working-order/fill views, and floating snapshot context
- persisted journal audit-trail drill-downs with severity/category summaries and formatted payloads
- latency detail views including per-stage trade-path timing and host-correlation context
- a live contract chart backed only by `/chart/config`, `/chart/snapshot`, `/chart/history`, and `/chart/stream`, with timeframe switching, fit/live-follow controls, load-older paging, active-position context, exact working-order price overlays, recent fill markers, a working-order summary rail, chart-side runtime alert banners, and operator readout strips for the currently loaded contract
- a clearly-labeled sample-candle fallback when live market data is unconfigured, so the chart workspace still renders cleanly during local setup or documentation/demo flows
- a browser-verified responsive QA pass across `390px`, `768px`, `1024px`, and `1440px` with no page-level horizontal overflow in the current dark UI

The chart now renders the currently loaded strategy contract through the local control plane and does not call Databento or Tradovate directly.
The chart module now also surfaces reconnect, shutdown, degraded-feed, chart-stream, dispatch posture, and the no-market-data sample fallback directly inside the chart itself, and the latest browser sign-off sweep cleared the fresh-open local console path plus responsive width sweeps without page-level overflow. The remaining dashboard work is now centered on a chart-first redesign around that live chart rather than on first chart delivery itself, with status tracked in [docs/architecture/dashboard_production_ui_plan.md](</C:/repos/TV_bot_core/docs/architecture/dashboard_production_ui_plan.md>) and [docs/architecture/dashboard_live_chart_plan.md](</C:/repos/TV_bot_core/docs/architecture/dashboard_live_chart_plan.md>).

## Local Development

From [apps/dashboard](</C:/repos/TV_bot_core/apps/dashboard>):

```bash
npm install
npm run dev
```

The Vite dev server proxies the local runtime control plane by default:

- HTTP proxy target: `http://127.0.0.1:8080`
- WebSocket proxy target: `ws://127.0.0.1:8081`

The dev proxy includes the dashboard read paths and settings/journal routes used by the operator surface, so `http://127.0.0.1:4173` can talk to the local runtime host without extra frontend configuration.

If the runtime uses different binds, set:

- `VITE_CONTROL_API_PROXY_TARGET`
- `VITE_CONTROL_API_WS_PROXY_TARGET`

For static builds or alternate local reverse proxies, `VITE_CONTROL_API_BASE_URL` can point the dashboard at a different local control-plane origin.
If the event stream is served from a separate WebSocket origin instead of the same host, set `VITE_CONTROL_API_EVENTS_URL`.

## Follow-up Note

Reconnect hardening now includes startup review-required gating plus paper startup/reconnect `close_position`, `leave_broker_protected`, and `reattach_bot_management` coverage through the real runtime host, and the broader paper release-sweep regression is also in place.
The remaining dashboard work is now centered on the chart-first redesign and production sign-off around the live chart module, plus the final hands-on paper/demo release verification pass.

## Production UI Follow-Up

The dashboard is functionally broad now, but it still needs a chart-first product pass to feel like a polished trading workspace instead of a set of strong modules on one page.
The tracked redesign plan for that chart-first, dark-first, responsive, operator-grade interface lives in [docs/architecture/dashboard_production_ui_plan.md](</C:/repos/TV_bot_core/docs/architecture/dashboard_production_ui_plan.md>).

That plan is the current source of truth for:

- making the live contract chart the center of the workspace
- reorganizing the page into chart-adjacent rails plus a lower detail dock
- dark-mode-first visual redesign
- overflow and responsive hardening
- component decomposition and frontend QA gates

The tracked plan for the live contract-chart surface itself lives in [docs/architecture/dashboard_live_chart_plan.md](</C:/repos/TV_bot_core/docs/architecture/dashboard_live_chart_plan.md>).

The first implementation slices of that plan are now in place: the dashboard shell is dark-first, the operator rail is more compact and scan-friendly, the control center is grouped into clearer operator modules with a denser summary strip, safer form-grid behavior, and tighter action groupings, the monitoring/audit deck has stronger hierarchy, the monitoring, control-center, and safety-review flows now live in dedicated component files, the dashboard polling/event/command orchestration is split across dedicated runtime-host, strategy-workflow, and settings-workflow hooks, the runtime-to-view-model shaping logic now lives in a dedicated projection module, and the live contract-chart renderer now mounts below the control center with runtime-host-backed timeframe switching, fit/live-follow controls, chart-stream updates, buffered history paging, active-position context, exact working-order price overlays, recent fill overlays, chart-side runtime alert banners, and operator readout strips. The next pass is to make that chart the center of the whole workspace rather than one strong module inside the current page.
