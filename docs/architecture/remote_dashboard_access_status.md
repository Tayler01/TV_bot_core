# Remote Dashboard Access Status

## Purpose

Record the remote dashboard access work that is already implemented in the codebase, so the architecture and ops docs match the current runtime and dashboard behavior.

This status note sits alongside:

- [remote_dashboard_access_plan.md](/C:/repos/TV_bot_core/docs/architecture/remote_dashboard_access_plan.md)
- [remote_dashboard_access_implementation_checklist.md](/C:/repos/TV_bot_core/docs/architecture/remote_dashboard_access_implementation_checklist.md)

## Implemented In This Branch

As of 2026-04-19, the current branch implements the backend and dashboard slices for:

- trusted authenticated operator identity propagation
- backend role-based authorization for privileged routes
- dashboard visibility for authenticated operator and authorization state
- dashboard capability gating based on backend-advertised permissions

The runtime still remains localhost-first and the dashboard still consumes only the runtime control plane.

## Current Header Contract

The runtime now supports trusted local identity headers through [config/runtime.example.toml](/C:/repos/TV_bot_core/config/runtime.example.toml) and [crates/config/src/lib.rs](/C:/repos/TV_bot_core/crates/config/src/lib.rs).

Current configurable trusted headers:

- `x-authenticated-user`
- `x-authenticated-name`
- `x-authenticated-session`
- `x-authenticated-device`
- `x-authenticated-provider`
- `x-authenticated-roles`

These are only trusted when:

- `remote_access.trust_local_identity_headers = true`
- the runtime is behind a trusted local upstream

Privileged commands can now be forced to fail closed when identity is missing by enabling:

- `remote_access.require_authenticated_identity_for_privileged_commands = true`

## Current Role Model

The shared role model now lives in [crates/core_types/src/lib.rs](/C:/repos/TV_bot_core/crates/core_types/src/lib.rs).

Current roles:

- `viewer`
- `operator`
- `trade_operator`

Current implication rules:

- `trade_operator` includes operator-level capabilities
- `operator` includes viewer-level visibility

## Current Backend Authorization

The runtime host now applies backend authorization in [apps/runtime/src/host.rs](/C:/repos/TV_bot_core/apps/runtime/src/host.rs).

Current policy:

- `viewer`
  - read-only status access
  - cannot update settings
  - cannot manage strategies
  - cannot change runtime mode or gating
  - cannot arm or trade
- `operator`
  - can manage runtime posture
  - can manage strategies
  - can update settings
  - cannot submit trade-capable actions
- `trade_operator`
  - can perform trade-capable actions
  - can arm
  - can close, flatten, cancel, and manual-enter through the runtime command path

Current privileged route behavior:

- `/settings` requires `operator`
- `/strategies/upload` requires `operator`
- `/strategies/validate` requires `operator`
- `/runtime/commands` checks the requested command and enforces `operator` or `trade_operator`
- `/commands` currently treats command execution as trade-capable and requires `trade_operator`

Rejected authorization attempts are returned as `Forbidden` and journaled as privileged command rejections.

## Current Runtime Status Surface

The control API now exposes authenticated operator and authorization state through [crates/control_api/src/lib.rs](/C:/repos/TV_bot_core/crates/control_api/src/lib.rs).

Current `RuntimeStatusSnapshot` additions:

- `authenticated_operator`
- `authorization`

Current authorization booleans:

- `can_view`
- `can_manage_runtime`
- `can_manage_strategies`
- `can_update_settings`
- `can_trade`

This data is now used by the dashboard instead of relying only on local UI assumptions.

## Current Dashboard Behavior

The dashboard now consumes and displays this state through:

- [apps/dashboard/src/App.tsx](/C:/repos/TV_bot_core/apps/dashboard/src/App.tsx)
- [apps/dashboard/src/components/dashboardControlPanels.tsx](/C:/repos/TV_bot_core/apps/dashboard/src/components/dashboardControlPanels.tsx)
- [apps/dashboard/src/components/dashboardMonitoring.tsx](/C:/repos/TV_bot_core/apps/dashboard/src/components/dashboardMonitoring.tsx)
- [apps/dashboard/src/hooks/useDashboardRuntimeHost.ts](/C:/repos/TV_bot_core/apps/dashboard/src/hooks/useDashboardRuntimeHost.ts)
- [apps/dashboard/src/types/controlApi.ts](/C:/repos/TV_bot_core/apps/dashboard/src/types/controlApi.ts)

Current dashboard auth UX:

- shows authenticated operator identity in the system bar
- shows current access level as a pill
- warns when the session is read-only
- disables unauthorized controls for setup, runtime posture, and trade actions
- treats `403` responses as access warnings rather than generic fatal failures

Important guardrail:

- the dashboard only mirrors backend capabilities for operator clarity
- backend authorization remains authoritative

## Current Audit Behavior

Authenticated operator identity now flows into journaled execution and lifecycle payloads through:

- [apps/runtime/src/host.rs](/C:/repos/TV_bot_core/apps/runtime/src/host.rs)
- [crates/runtime_kernel/src/lib.rs](/C:/repos/TV_bot_core/crates/runtime_kernel/src/lib.rs)

Current persisted operator fields include:

- user id
- display name
- session id
- device id
- provider
- roles

## Tests Added

Current targeted coverage includes:

- runtime host auth-required rejection for missing operator identity
- runtime host acceptance for `operator` on settings updates
- runtime host rejection for `viewer` on operator routes
- runtime host rejection for `operator` on trade routes
- runtime status exposure of authenticated operator and authorization state
- runtime request injection of trusted operator identity and roles
- config defaults and overrides for the trusted roles header
- runtime-kernel audit persistence of authenticated operator payloads
- dashboard tests for `operator` and `viewer` capability gating

## Remaining Work

This branch does not yet complete the full remote-access program.

Still pending:

- real Aurora-side trusted ingress wiring that injects identity and role headers
- finalized role mapping for actual operators and devices
- end-to-end private remote paper validation on the exchange-near host
- break-glass validation from the deployed Aurora-side host
- optional public-browser Cloudflare path if needed later

## Operational Meaning

From this point forward, the remote dashboard deployment should be treated as a three-layer system:

1. trusted remote-access and identity ingress
2. same-host dashboard and reverse proxy
3. localhost-only runtime host and storage

The runtime and dashboard are now ready for that ingress layer to supply trusted identity and roles.
