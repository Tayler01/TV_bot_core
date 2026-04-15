# Dashboard

This package contains the local React dashboard for the trading runtime.

The dashboard consumes only the local control plane:

- `GET /health`
- `GET /status`
- `GET /readiness`
- `GET /history`
- `GET /journal`
- `GET /settings`
- `GET /strategies`
- `POST /strategies/upload`
- `POST /strategies/validate`
- `POST /settings`
- `POST /runtime/commands`
- `GET /events`

## Current Slice

The current dashboard now covers:

- a dark-first shell refresh with a higher-signal operator status rail for mode, arm, readiness, warmup, dispatch, and safety posture
- a grouped control center that separates mode/gating, strategy/settings, and execution-facing operator actions
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
The remaining dashboard work is now mostly production UI polish plus the final hands-on paper/demo release verification pass.

## Production UI Follow-Up

The dashboard is functionally broad now, but it is still not visually production-ready.
The tracked redesign plan for the dark-first, responsive, operator-grade interface lives in [docs/architecture/dashboard_production_ui_plan.md](</C:/repos/TV_bot_core/docs/architecture/dashboard_production_ui_plan.md>).

That plan is the current source of truth for:

- dark-mode-first visual redesign
- layout hierarchy and control-center restructuring
- overflow and responsive hardening
- component decomposition and frontend QA gates

The first implementation slices of that plan are now in place: the dashboard shell is dark-first, the operator rail is more compact and scan-friendly, the control center is grouped into clearer operator modules, and the worst top-level/mobile overflow issues were reduced while the broader component/layout cleanup remains ongoing.
