# Remote Deployment

Use this runbook when you want to run the bot on a Linux server close to the exchange and access the dashboard remotely from your operator PC.

This runbook follows the V1 remote-access recommendation in [remote_dashboard_access_plan.md](/C:/repos/TV_bot_core/docs/architecture/remote_dashboard_access_plan.md):

- keep the runtime host on localhost-only binds
- use Tailscale for private operator access
- use Caddy to serve the dashboard and proxy the runtime host
- do not expose runtime or Postgres ports publicly

For the Aurora-side host bring-up and remote paper validation flow, also use [aurora_remote_setup_and_test.md](/C:/repos/TV_bot_core/docs/ops/aurora_remote_setup_and_test.md).

## Recommended Host Layout

Recommended services on the exchange-near Linux host:

- `tv-bot-runtime`
- `postgresql`
- `caddy`
- `tailscaled`

Recommended filesystem layout:

```text
/opt/tv-bot-core/current/
  bin/
    tv-bot-runtime
    tv-bot-cli
  dashboard/
  config/
    runtime.remote.toml
  deploy/
    remote/linux/Caddyfile
    remote/linux/systemd/
  docs/ops/
  strategies/examples/
```

You can adapt the directory names, but keep the dashboard assets, runtime binary, config, and deployment assets together so upgrades stay repeatable.

## Prerequisites

- A Linux server in the target region with root or sudo access.
- A domain or tailnet DNS name for the dashboard origin.
- Tailscale installed on the server and the operator PC.
- Databento and Tradovate credentials available through environment variables or a protected environment file.
- Postgres installed locally on the host or reachable over a private same-region network.

## Recommended Runtime Config

Keep the control plane on localhost:

```toml
[control_api]
http_bind = "127.0.0.1:8080"
websocket_bind = "127.0.0.1:8081"
```

Do not change these binds to `0.0.0.0` for the standard remote dashboard deployment.

Keep secrets out of the TOML file.
Use [credential_setup.md](/C:/repos/TV_bot_core/docs/ops/credential_setup.md) as the credential source of truth.

If you are enabling authenticated remote operator roles, also configure:

```toml
[remote_access]
trust_local_identity_headers = true
require_authenticated_identity_for_privileged_commands = true
authenticated_user_header = "x-authenticated-user"
authenticated_display_name_header = "x-authenticated-name"
authenticated_session_header = "x-authenticated-session"
authenticated_device_header = "x-authenticated-device"
authenticated_provider_header = "x-authenticated-provider"
authenticated_roles_header = "x-authenticated-roles"
```

Only enable this when the Aurora-side ingress path is trusted to inject those headers locally.

## Release Bundle

Build the Linux release bundle from the repo root:

```bash
./scripts/package_release.sh
```

This produces a bundle under `dist/releases/` that includes:

- runtime binary
- CLI binary
- built dashboard
- runtime config example
- ops docs
- deployment examples
- example strategies

Copy the extracted bundle to the target host, for example:

```bash
scp tv-bot-core-<version>-linux-<arch>-<commit>.tar.gz user@server:/tmp/
```

Then unpack it on the server:

```bash
sudo mkdir -p /opt/tv-bot-core
sudo tar -C /opt/tv-bot-core -xzf /tmp/tv-bot-core-<version>-linux-<arch>-<commit>.tar.gz
sudo ln -sfn /opt/tv-bot-core/tv-bot-core-<version>-linux-<arch>-<commit> /opt/tv-bot-core/current
```

## Example Deployment Assets

Checked-in examples live under:

- [deploy/remote/linux/Caddyfile](/C:/repos/TV_bot_core/deploy/remote/linux/Caddyfile)
- [deploy/remote/linux/systemd/tv-bot-runtime.service](/C:/repos/TV_bot_core/deploy/remote/linux/systemd/tv-bot-runtime.service)
- [deploy/remote/linux/systemd/caddy.service](/C:/repos/TV_bot_core/deploy/remote/linux/systemd/caddy.service)

Treat them as examples, not vendor-locked production truth.

## Environment File

Recommended protected environment file:

```text
/etc/tv-bot-core/runtime.env
```

Suggested ownership:

- owner: `tvbot`
- group: `tvbot`
- mode: `0600`

Suggested contents:

```bash
DATABENTO_API_KEY=db-...
TV_BOT__BROKER__USERNAME=your.tradovate.login
TV_BOT__BROKER__PASSWORD=your-password
TV_BOT__BROKER__CID=your-cid
TV_BOT__BROKER__SEC=your-secret
TV_BOT__BROKER__PAPER_ACCOUNT_NAME=SIM123456
TV_BOT__PERSISTENCE__PRIMARY_URL=postgres://tvbot:strong-password@127.0.0.1:5432/tv_bot_core
```

## Runtime Service Install

Create a dedicated runtime user:

```bash
sudo useradd --system --home /opt/tv-bot-core --shell /usr/sbin/nologin tvbot
```

Create needed directories:

```bash
sudo mkdir -p /var/lib/tv-bot-core
sudo mkdir -p /var/log/tv-bot-core
sudo chown -R tvbot:tvbot /var/lib/tv-bot-core /var/log/tv-bot-core
```

Install the runtime service example:

```bash
sudo cp /opt/tv-bot-core/current/deploy/remote/linux/systemd/tv-bot-runtime.service /etc/systemd/system/tv-bot-runtime.service
sudo systemctl daemon-reload
sudo systemctl enable --now tv-bot-runtime.service
```

Verify:

```bash
sudo systemctl status tv-bot-runtime.service
curl http://127.0.0.1:8080/status
curl http://127.0.0.1:8080/readiness
```

## Caddy Install

Install Caddy through your normal distro path or the official Caddy packages.

Copy the example config:

```bash
sudo cp /opt/tv-bot-core/current/deploy/remote/linux/Caddyfile /etc/caddy/Caddyfile
sudo systemctl restart caddy
```

The example config serves:

- static dashboard assets from `/opt/tv-bot-core/current/dashboard`
- runtime HTTP routes through `127.0.0.1:8080`
- runtime WebSocket routes through `127.0.0.1:8081`

The checked-in Caddyfile is intentionally transport-only.
Authenticated operator identity still needs a trusted ingress-capable layer in front of the local dashboard origin if you want the runtime to enforce `viewer`, `operator`, and `trade_operator` roles.

## Tailscale Access

Install Tailscale on the server and join it to your tailnet.

Typical flow:

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up
```

After enrollment, verify the server appears in the tailnet admin console and can be reached from the operator PC over Tailscale.

Use a Tailscale or tailnet-restricted DNS name for the dashboard when possible.

## Firewall Checklist

Required host posture:

- allow SSH only through your intended admin path
- allow Tailscale traffic
- if using public HTTPS later, allow only `80` and `443` to Caddy
- deny public inbound access to `8080`
- deny public inbound access to `8081`
- deny public inbound access to Postgres

Example `ufw` flow:

```bash
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp
sudo ufw allow 443/tcp
sudo ufw allow 80/tcp
sudo ufw enable
```

If you are using Tailscale-only access and no public HTTPS yet, do not open `80` and `443` publicly unless you actually need them.

The important rule is simple:

- the browser must never reach the runtime host ports directly

## Verification Sequence

After deployment:

1. Verify the runtime host locally:
   - `curl http://127.0.0.1:8080/status`
   - `curl http://127.0.0.1:8080/readiness`
2. Verify Caddy on the server:
   - `curl -I http://127.0.0.1/`
3. Verify the dashboard from the operator PC through Tailscale.
4. Verify the event and chart WebSockets load through the browser.
5. Verify runtime ports are not exposed externally.
6. Verify `/status` shows the expected authenticated operator and authorization state when reached through the private ingress path.

## Update Flow

Repeat upgrades with this pattern:

1. Build a new release bundle.
2. Copy the bundle to the server.
3. Extract to a versioned directory under `/opt/tv-bot-core/`.
4. Move the `current` symlink.
5. Restart `tv-bot-runtime`.
6. Restart `caddy` if needed.
7. Verify `/status`, `/readiness`, and the dashboard.

## Break-Glass Notes

If the dashboard path is unavailable:

- connect through Tailscale SSH or normal SSH over the approved admin path
- use `tv-bot-cli` on the server
- verify runtime health locally before taking action

See [remote_operator_access.md](/C:/repos/TV_bot_core/docs/ops/remote_operator_access.md) and [cli_standalone.md](/C:/repos/TV_bot_core/docs/ops/cli_standalone.md).
