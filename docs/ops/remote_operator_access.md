# Remote Operator Access

Use this runbook when operating the exchange-near server from your PC over the recommended private remote-access path.

This runbook assumes:

- the server is already deployed using [remote_deployment.md](/C:/repos/TV_bot_core/docs/ops/remote_deployment.md)
- the server and your PC are joined to the same Tailscale tailnet
- Caddy is serving the dashboard
- the runtime host remains bound to localhost on the server

## Remote Access Model

The V1 operator path is:

- operator PC
- Tailscale
- dashboard origin on the server
- Caddy reverse proxy
- localhost runtime host

The operator PC should never connect directly to the runtime host ports.

## First Login Checklist

Before you operate remotely:

- confirm your Tailscale account uses MFA
- confirm the server shows as online in the tailnet
- confirm the dashboard hostname resolves to the intended server
- confirm the dashboard loads over the intended private path
- confirm the runtime mode shown in the dashboard is the expected mode before you do anything else

## Recommended Operator Workflow

1. Open the dashboard.
2. Check `mode`, `arm`, `readiness`, broker health, market-data health, and storage posture.
3. Check the selected account and confirm it matches paper or live expectations.
4. Load or verify the target strategy.
5. Warm up and wait for readiness.
6. Arm only when you intend to enable trading.
7. Keep the journal and history surfaces visible during important actions.

## Remote Safety Checks

Before any trade-capable action:

- confirm whether you are in `paper` or `live`
- confirm the account name is the expected one
- confirm the runtime is explicitly armed only when intended
- confirm no reconnect-review or shutdown-review state is pending
- confirm no degraded-data or broker-sync warning is blocking new entries

## Web Dashboard Validation

The remote dashboard path is acceptable only if all of these work from your PC:

- main dashboard page load
- `status`
- `readiness`
- `history`
- `journal`
- `settings`
- strategy library and validation
- event feed
- live chart stream

If the page loads but the live event or chart surfaces do not update, suspect WebSocket proxying first.

## Tailscale Troubleshooting

If the remote dashboard does not load:

1. Check the server is online in Tailscale.
2. Check your PC is online in Tailscale.
3. Check tailnet ACLs or grants.
4. Check the dashboard origin name or IP.
5. Check Caddy on the server.
6. Check the runtime host locally on the server.

Useful server-side checks:

```bash
tailscale status
sudo systemctl status caddy
sudo systemctl status tv-bot-runtime
curl http://127.0.0.1:8080/status
curl http://127.0.0.1:8080/readiness
```

## Break-Glass SSH

If the browser path is down, use SSH through Tailscale or your approved admin path.

Once on the server:

```bash
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml status
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml readiness
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml history
```

## Break-Glass Command Patterns

Status and readiness:

```bash
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml status
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml readiness
```

Warmup:

```bash
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml warmup start
```

Mode:

```bash
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml start paper --yes
```

Arm and disarm:

```bash
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml arm --yes
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml disarm --yes
```

Flatten:

```bash
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml flatten <contract-id> --yes --reason "remote break-glass flatten"
```

Reconnect review:

```bash
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml reconnect-review leave-broker-protected --contract-id <contract-id> --yes --reason "remote operator decision"
```

Shutdown review:

```bash
/opt/tv-bot-core/current/bin/tv-bot-cli --config /opt/tv-bot-core/current/config/runtime.remote.toml shutdown leave-broker-protected --contract-id <contract-id> --yes --reason "remote operator decision"
```

## Audit Checks During Remote Operation

Verify the dashboard or CLI history and journal views capture:

- strategy load
- warmup start and completion
- mode changes
- arm and disarm
- manual actions
- reconnect or shutdown review decisions
- fills, order changes, position changes, and PnL changes

Remote production use should not be treated as complete until authenticated operator identity is also preserved in those records.

## Release-Gate Checks For Remote Operation

Before relying on remote operation for serious sessions:

- verify the dashboard path over Tailscale
- verify break-glass SSH and CLI access
- verify runtime and database ports are not public
- run the paper validation flow remotely

Use [paper_demo_verification.md](/C:/repos/TV_bot_core/docs/ops/paper_demo_verification.md) for the paper release-gate flow.
