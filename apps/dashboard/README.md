# Dashboard

This package contains the local React dashboard for the trading runtime.

The dashboard consumes only the local control plane:

- `GET /health`
- `GET /status`
- `GET /readiness`
- `GET /history`
- `GET /strategies`
- `POST /strategies/validate`
- `POST /runtime/commands`
- `GET /events`

## Current Slice

The current dashboard now covers:

- explicit runtime mode with strong paper/live separation
- strategy library browsing and host-backed validation before load
- strategy load through the audited runtime lifecycle command path
- warmup, arm/disarm, pause/resume, mode switch, close-position, and cancel-working-orders controls
- reconnect and shutdown review action cards through the runtime lifecycle command path
- local `/events` WebSocket operator feed for journal, command, readiness, history, and health updates
- account routing
- grouped readiness checks
- broker, feed, storage, and host health
- history, PnL, working-order, fill, and latest latency summaries

## Local Development

From [apps/dashboard](</C:/repos/TV_bot_core/apps/dashboard>):

```bash
npm install
npm run dev
```

The Vite dev server proxies the local runtime control plane by default:

- HTTP proxy target: `http://127.0.0.1:8080`
- WebSocket proxy target: `ws://127.0.0.1:8081`

If the runtime uses different binds, set:

- `VITE_CONTROL_API_PROXY_TARGET`
- `VITE_CONTROL_API_WS_PROXY_TARGET`

For static builds or alternate local reverse proxies, `VITE_CONTROL_API_BASE_URL` can point the dashboard at a different local control-plane origin.
If the event stream is served from a separate WebSocket origin instead of the same host, set `VITE_CONTROL_API_EVENTS_URL`.

## Follow-up Note

After this richer operator-surface slice, circle back to reconnect hardening and the broader reconnect-review/operator-resolution pass before calling the dashboard and paper campaign complete.
