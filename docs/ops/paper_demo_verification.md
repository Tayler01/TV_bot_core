# Paper Demo Verification

Use this runbook before calling paper/demo readiness complete for a release candidate.

## Preconditions

- Runtime configuration points to the Tradovate demo or paper account explicitly.
- Market data credentials are valid.
- Postgres is available, or the operator has already accepted the session-local SQLite fallback override.
- The strategy file to test has already been validated through the runtime host or dashboard.

## Verification Flow

1. Start the runtime host with the intended release configuration.
2. Open the dashboard and verify the mode banner clearly shows `paper`.
3. Confirm `/status` and `/readiness` show:
   - the expected paper account
   - healthy market data
   - healthy broker sync
   - the active storage backend and any fallback warning state
4. Load the target strategy, then run warmup and wait for readiness.
5. Arm the runtime explicitly.
6. Submit a manual entry and verify:
   - the request is accepted only while armed
   - broker-side stop-loss and take-profit protections are present when required
   - the order routes to the paper/demo account
7. If the strategy allows scaling, submit a same-side scale-in and verify the paper account routing and protections again.
8. Use the dashboard or CLI to cancel working orders, close the current position, and flatten the current position as applicable.
9. Trigger the operator no-new-entry gate and verify a new manual entry is blocked with an operator-visible conflict.
10. Simulate or confirm degraded market data or broker sync and verify:
    - new entries are blocked
    - existing broker-protected positions are left alone unless the operator chooses otherwise
11. Restart or reconnect the runtime with an existing paper position or working orders and verify:
    - runtime enters review-required state
    - arming and new entries stay blocked until the operator resolves the review
    - `close_position`, `leave_broker_protected`, and `reattach_bot_management` behave as expected

## Audit Checks

Verify the journal and history surfaces contain:

- strategy load and validation records
- warmup start and completion
- arm and disarm transitions
- manual actions
- risk decisions and dispatch results
- reconnect or shutdown review decisions
- fills, positions, open-order changes, and PnL updates

## Exit Criteria

Paper verification is complete only when the operator confirms:

- all tested actions used the explicit paper/demo account
- no trading happened while disarmed
- review-required states blocked new risk until an operator decision
- journal and history records match the actions taken
