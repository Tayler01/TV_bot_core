# Reconnect And Shutdown Review

Use this runbook when the runtime detects active exposure during reconnect or when shutdown is requested with open risk still present.

## Reconnect Review

The runtime should enter `review required` when it reconnects or starts up and detects:

- an open position
- working orders
- or both

### Expected Guardrails

- arming stays blocked
- new entries stay blocked
- the operator must choose one of the explicit resolution paths

### Resolution Paths

1. `close_position`
   Use when the operator wants the runtime to dispatch the existing audited flatten or liquidation path.
2. `leave_broker_protected`
   Use when broker-side protections are already in place and the operator wants the runtime to stand down.
3. `reattach_bot_management`
   Use when the operator wants the runtime to resume management of the existing exposure.

### Verification

After resolving the review, confirm:

- the review-required state clears
- status reflects the expected exposure state
- journal records show the review decision
- no unexpected new entry was sent as part of the review resolution

## Shutdown Review

Shutdown should warn and block when open exposure exists.

### Expected Operator Choices

1. flatten first
2. leave the broker-protected position in place
3. cancel the shutdown request and continue observing

### Verification

Before completing shutdown, confirm:

- the chosen action is explicit in the dashboard or CLI
- the action is journaled
- the resulting position and working-order state matches the chosen review outcome
