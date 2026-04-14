# Storage Fallback Override

Use this runbook when Postgres is unavailable and the runtime requests a temporary SQLite fallback override.

## Expected Runtime Behavior

- Postgres is the primary database target.
- Unexpected fallback to SQLite must warn loudly and require an explicit session-only override.
- The warning must remain visible through the runtime host surfaces.

## Operator Steps

1. Confirm the Postgres outage or connectivity problem is real.
2. Open the dashboard or query `/readiness` and `/status`.
3. Verify the runtime reports:
   - primary storage is unavailable
   - SQLite fallback is available
   - an override is required before trading can proceed
4. Decide whether the session should continue under fallback:
   - if no, leave the runtime unarmed and restore Postgres first
   - if yes, apply the temporary override intentionally and record the reason in the operator log or incident notes
5. After the override, verify the runtime surfaces still show that fallback mode is active for the current session.
6. When Postgres returns, restart into the primary storage path and confirm the warning clears.

## Do Not

- treat fallback as a silent default
- continue trading without an explicit override
- assume SQLite fallback is equivalent to the intended steady-state storage configuration

## Release Note

Any release candidate that required the fallback override during validation should not be signed off as a clean primary-storage pass until Postgres-backed validation is rerun.
