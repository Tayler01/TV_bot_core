# Aurora Remote Setup And Test

Use this runbook for the exchange-near Aurora-side host that will run the bot close to the exchange while you operate it from your PC through the remote dashboard.

This runbook assumes:

- the Aurora-side server is the low-latency execution host
- the dashboard is hosted on that same server
- the runtime host stays bound to localhost
- a trusted ingress layer injects authenticated operator headers into the local dashboard origin

Related docs:

- [remote_deployment.md](/C:/repos/TV_bot_core/docs/ops/remote_deployment.md)
- [remote_operator_access.md](/C:/repos/TV_bot_core/docs/ops/remote_operator_access.md)
- [remote_dashboard_access_status.md](/C:/repos/TV_bot_core/docs/architecture/remote_dashboard_access_status.md)

## Aurora-Side Target Topology

Recommended topology on the Aurora-side host:

```text
Operator PC
  -> Tailscale private path
  -> trusted identity-capable ingress
  -> Caddy
  -> dashboard static files
  -> runtime HTTP on 127.0.0.1:8080
  -> runtime WebSocket on 127.0.0.1:8081
  -> Postgres on localhost or private same-region network
```

The important separation is:

- the runtime binds only to localhost
- the browser never reaches runtime ports directly
- identity headers are injected only by the trusted local ingress

## What The Runtime Expects

The current runtime remote-auth implementation expects these headers from the trusted local ingress:

- `x-authenticated-user`
- `x-authenticated-name`
- `x-authenticated-session`
- `x-authenticated-device`
- `x-authenticated-provider`
- `x-authenticated-roles`

The roles header is a comma-separated list using:

- `viewer`
- `operator`
- `trade_operator`

Example:

```text
x-authenticated-user: operator@example.com
x-authenticated-name: Primary Operator
x-authenticated-session: session-123
x-authenticated-device: desktop-01
x-authenticated-provider: tailscale
x-authenticated-roles: trade_operator
```

## Recommended Runtime Config On Aurora Host

Keep the runtime on localhost and enable trusted identity parsing explicitly.

Suggested production block:

```toml
[control_api]
http_bind = "127.0.0.1:8080"
websocket_bind = "127.0.0.1:8081"

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

Do not enable this unless the headers come only from the trusted local ingress path on the Aurora-side host.

## Aurora Host Bring-Up Sequence

1. Provision the Aurora-side Linux server in the exchange-near region.
2. Install:
   - `postgresql`
   - `caddy`
   - `tailscaled`
3. Copy the packaged release bundle to the host and unpack it under `/opt/tv-bot-core/`.
4. Install the runtime `systemd` unit and confirm the runtime answers locally on `127.0.0.1:8080` and `127.0.0.1:8081`.
5. Install the Caddy config and confirm the dashboard serves locally.
6. Join the server to Tailscale and confirm the host is reachable from your PC.
7. Add the trusted identity-capable ingress layer in front of Caddy.
8. Enable the remote-access config block above.
9. Restart the runtime and verify `/status` reflects the expected authenticated operator and authorization state when reached through the private ingress path.

## Trusted Ingress Requirement

The current codebase needs an ingress path that can:

- authenticate the remote user
- identify the remote device or session
- map the operator to one or more roles
- forward that identity to the local dashboard origin as trusted headers

Recommended Aurora-side V1 approach:

- keep Tailscale as the private network gate
- use a trusted local ingress path that can forward Tailscale-backed operator identity into the dashboard origin
- keep Caddy as the same-host static server and runtime reverse proxy

If your chosen ingress path uses different upstream header names, change the runtime config to match.

## Role Mapping On Aurora Host

Recommended initial mapping:

- your primary trading user: `trade_operator`
- your secondary operational user, if any: `operator`
- read-only observer account, if any: `viewer`

Do not assign `trade_operator` to extra accounts until the remote paper test flow has passed.

## Aurora-Side Host Checks

Run these locally on the Aurora-side server after deployment:

```bash
sudo systemctl status tv-bot-runtime
sudo systemctl status caddy
tailscale status
curl http://127.0.0.1:8080/status
curl http://127.0.0.1:8080/readiness
curl -I http://127.0.0.1/
```

Expected result:

- runtime healthy locally
- dashboard assets served by Caddy
- runtime ports reachable only on localhost
- private ingress path ready for remote browser use

## Pre-Remote Identity Smoke Test

Before testing from your PC, verify that the trusted ingress is actually forwarding the identity contract you expect.

Minimum checks:

- `/status` shows `authenticated_operator`
- `/status.authorization.can_manage_runtime` matches the mapped role
- `/status.authorization.can_trade` is `true` only for `trade_operator`

For a `viewer` session:

- `can_manage_runtime = false`
- `can_manage_strategies = false`
- `can_update_settings = false`
- `can_trade = false`

For an `operator` session:

- `can_manage_runtime = true`
- `can_manage_strategies = true`
- `can_update_settings = true`
- `can_trade = false`

For a `trade_operator` session:

- all capabilities above should be `true`

## Remote PC Validation

From your operator PC over the Aurora-side private ingress path:

1. Open the dashboard.
2. Confirm the system bar shows:
   - authenticated operator identity
   - expected access level
   - correct paper or live mode
3. Confirm the context rail shows:
   - operator identity
   - access label
4. Confirm unauthorized controls are disabled for non-trade accounts.

Expected UI behavior:

- `viewer` cannot change mode, load strategy, save settings, arm, or trade
- `operator` can manage setup and runtime posture but cannot trade
- `trade_operator` can use trade-capable controls

## Aurora Paper Validation Flow

Run this full paper-only flow on the Aurora-side host before any live rollout.

### Phase 1: Access And Auth

- open the remote dashboard from your PC
- confirm the signed-in operator identity is correct
- confirm the access pill matches the intended role
- confirm a `403` appears as an access warning if you intentionally test with a lower-privilege account

### Phase 2: Host And Transport

- confirm dashboard load succeeds
- confirm `/events` updates in the Events tab
- confirm `/chart/stream` updates in the chart
- confirm no direct connection to `127.0.0.1:8080` or `127.0.0.1:8081` is used by the browser

### Phase 3: Strategy And Setup

- refresh the strategy library
- validate the selected strategy
- load the selected strategy
- save a harmless runtime-settings update
- confirm setup actions are journaled with authenticated operator identity

### Phase 4: Runtime Posture

- switch to paper mode if not already there
- start warmup
- wait for readiness
- confirm `operator` sessions can do this and `viewer` sessions cannot

### Phase 5: Trade-Capable Flow

With the `trade_operator` account only:

- arm
- submit one manual paper entry with a clear reason
- confirm the request is journaled with authenticated operator identity and role
- cancel working orders if applicable
- close or flatten the paper position
- disarm

### Phase 6: Review Flows

- test reconnect review handling if reproducible in paper
- test shutdown review handling if reproducible in paper
- confirm `operator` can handle non-trade review acknowledgements
- confirm trade-capable review actions remain limited to `trade_operator`

### Phase 7: Degraded-State Safety

- simulate or reproduce degraded feed or broker sync
- confirm no-new-entry posture is visible remotely
- confirm the runtime still blocks unsafe new entries
- confirm the dashboard remains a reflection of backend truth, not the source of truth

## What To Check In Journal And History

During the Aurora-side paper session, confirm journal payloads include:

- `authenticated_operator.user_id`
- `authenticated_operator.display_name`
- `authenticated_operator.session_id`
- `authenticated_operator.device_id`
- `authenticated_operator.provider`
- `authenticated_operator.roles`

Review these actions specifically:

- strategy validation
- strategy load
- settings save
- mode change
- warmup start
- arm
- manual entry
- cancel working orders
- close or flatten
- disarm
- reconnect or shutdown review decisions

## Aurora-Side Failure Tests

Run these before calling the remote path ready:

- test a `viewer` session against a privileged route and confirm `Forbidden`
- test an `operator` session against a trade route and confirm `Forbidden`
- test a missing-header privileged request path and confirm fail-closed behavior
- test browser refresh and reconnect while the dashboard is already open
- test break-glass SSH and CLI access from the Aurora-side host

## Live Readiness Gate

Do not promote the Aurora-side deployment to live until all of these are true:

- paper validation completed successfully
- operator identity shows correctly in the UI
- privileged journaling includes authenticated operator identity
- role enforcement behaves exactly as expected
- runtime and Postgres ports are not publicly reachable
- break-glass SSH and CLI validation succeeded
- the exact Aurora-side runtime config used for the test session is recorded

## Suggested Validation Record

Capture these items in the test notes:

- Aurora host name or instance id
- region and exchange-near location
- candidate commit SHA
- packaged release bundle name
- runtime config path
- operator account used
- role used for each test
- paper account used
- strategy path used
- timestamp range of the validation session
- result of each phase above

## Hand-Off Note

Once the Aurora-side paper flow passes, the next practical step is:

- freeze the working ingress configuration
- record the final role mapping
- repeat the same checklist once more on the exact release candidate you plan to merge and deploy
