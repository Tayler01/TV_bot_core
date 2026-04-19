# CLI Console

`tv-bot-cli console` is a full-screen terminal operator surface for the local runtime host. It is a simplified alternative to the web dashboard and talks only to the local control API.

## What It Shows

- runtime mode, arm state, warmup status, strategy, contract, and health summary
- a live contract chart in the center pane
- a live activity feed with trade, command, warning, and system events
- current event-stream and chart-stream connection state

## What It Controls

The console uses the same audited runtime command path as the rest of the CLI. It does not place orders directly and it does not bypass arm or readiness checks.

Supported operator actions:

- switch mode to `paper`, `observation`, or `live`
- arm and disarm
- pause and resume
- refresh snapshots manually
- change supported chart timeframes
- filter the activity feed

## Launch

From the workspace root:

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml console
```

Common local-dev launch:

```powershell
.\target\debug\tv-bot-cli.exe --config .\config\runtime.local.toml console --refresh-seconds 1
```

Options:

- `--config <path>`
- `--base-url <url>`
- `console --refresh-seconds <n>`

If `--base-url` is omitted, the console uses the control API bind from runtime config.

## Layout

Top pane:

- left: runtime identity and current posture
- right: broker, market data, storage, and readiness context

Center pane:

- chart summary with chart availability, timeframe, bar count, contract context, and stream state
- live line chart for the active contract

Bottom pane:

- recent activity feed with color-coded categories

Footer:

- current keybindings
- active stream state
- pending confirmation prompt when needed

On narrower terminals, the console collapses into a more compact stacked layout.

## Keybindings

Navigation and overlays:

- `q`: quit the console
- `h` or `?`: open help
- `Esc`: close help

Mode and runtime control:

- `p`: switch to paper mode
- `o`: switch to observation mode
- `l`: switch to live mode
- `a`: arm
- `d`: disarm
- `Space`: pause or resume

Chart and feed:

- `1`: select `1s` chart if supported
- `2`: select `1m` chart if supported
- `3`: select `5m` chart if supported
- `r`: refresh snapshots immediately
- `f`: cycle feed filter

Confirmation flow:

- `y`: confirm the pending action
- `n`: cancel the pending action
- `Esc`: cancel the pending action

When help or a confirmation dialog is open, normal action keys are blocked.

## Confirmation Behavior

The console requires explicit confirmation for dangerous actions, including:

- switching into `live`
- arming when runtime posture requires operator confirmation

The confirmation prompt is shown both in the footer state and as a centered modal.

## Chart Behavior

The chart is a live line chart derived from the runtime chart snapshot and chart stream.

It includes:

- the active timeframe for the loaded contract
- a live price guide
- an active-position guide when present
- recent buy and sell fill markers

Timeframe selection follows the runtime's advertised supported frames. Unsupported frame requests are rejected in the activity feed.

History sizing follows the same snapshot sizing rules used by the dashboard-oriented chart flow:

- `1s` requests a larger intraday buffer
- `1m` loads a dashboard-sized intraday window
- `5m` loads a shorter swing-style view

Actual available history still depends on what the runtime currently has buffered.

## Activity Feed

The feed groups console-visible runtime activity into these filters:

- `all`
- `warnings`
- `trades`
- `commands`
- `system`

Rendered labels:

- `TRADE`
- `CMD`
- `WARN`
- `SYS`

The feed is populated from control-plane events plus chart and stream status changes, not from direct broker access.

## Stream Model

The console uses both snapshot polling and live streams:

- snapshot refresh for status, readiness, history, journal, chart config, and chart snapshot
- `/events` for runtime activity
- `/chart/stream` for live chart updates

If the selected timeframe changes, the chart stream is restarted for the new timeframe.

## Safety Notes

- The console never becomes the source of truth. Runtime state remains authoritative.
- No trading actions bypass arm state.
- Mode changes, arm/disarm, and pause/resume still go through the backend command path.
- Live mode and override-sensitive actions remain explicitly confirmed.

## Troubleshooting

If the chart is empty:

- confirm a strategy is loaded
- confirm market data is connected
- check `status` and `readiness`
- verify the runtime exposes chart support for the active contract

If the feed is quiet:

- check the active feed filter with `f`
- verify the footer shows open event and chart streams
- press `r` to force a fresh snapshot pull

If a control action is rejected:

- inspect the activity feed for the command result
- inspect `readiness` for blocking checks or override requirements
- remember that `observation` mode is not armable for trading

## Related Docs

- [Standalone CLI](C:\repos\TV_bot_core\docs\ops\cli_standalone.md)
- [Debugging Guide](C:\repos\TV_bot_core\docs\ops\debugging_guide.md)
- [Reconnect And Shutdown Review](C:\repos\TV_bot_core\docs\ops\reconnect_and_shutdown_review.md)
