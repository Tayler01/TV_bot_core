# Release Readiness Review

Review date: 2026-04-15

## Candidate

- Candidate branch: `main`
- Candidate commit: `19d39bf`
- Latest CI run: [GitHub Actions run 24484408061](https://github.com/Tayler01/TV_bot_core/actions/runs/24484408061)
- CI result: success on the candidate commit

## Current Verdict

The repository is now close to V1 release-ready from a code, dashboard, CI, and packaging perspective, but it is not fully signed off yet.

Current state:

- code and host/dashboard acceptance coverage are broadly in place
- cross-platform CI is green on the candidate commit
- local Windows packaging can be produced for the candidate commit
- final hands-on paper/demo verification is still blocked in this workspace by missing Tradovate demo credentials and paper account selection
- final release sign-off is still waiting on that external verification pass

## Acceptance Summary

### Met Or Substantially Met In Code And CI

- runtime modes are explicit and surfaced through the dashboard and CLI
- strict strategy parsing, validation, compilation, upload, and library workflows are implemented
- warmup is manual, visible, and blocks trade readiness
- Databento market-data health, symbol resolution, and warmup state are surfaced through status/readiness
- Tradovate execution and sync paths are implemented behind the strategy-agnostic runtime host
- arming is enforced for manual and strategy-driven trading paths
- readiness includes mode, strategy, warmup, account, symbol mapping, data, broker, storage, and risk summary state
- dashboard control-center functions are present through the local control API
- CLI control flow is present for launch, load, warmup, arm/disarm, flatten, status, readiness, and confirmations
- persistence, journaling, health, latency, and trade-history surfaces are implemented
- reconnect and shutdown review flows are covered through the host/operator path
- paper-mode host acceptance coverage exists for entry, scale-in, flatten, no-new-entry gating, arm-before-trade enforcement, startup/reconnect review handling, and broader release-sweep scenarios
- dashboard responsive UI hardening is now browser-verified at `390px`, `768px`, `1024px`, and `1440px` with no page-level horizontal overflow

### Still Pending Final Sign-Off

- final dashboard production sign-off and operator ergonomics review
- final Postgres-primary release-candidate walkthrough and fallback-override operator verification
- final release-checklist pass tying CI, packaging, validation notes, and the exact candidate commit together

### Blocked Externally In This Workspace

- hands-on Tradovate paper/demo verification on the candidate commit
- broker/account routing confirmation against a real paper/demo account
- final end-to-end paper validation of manual entry, broker-side protections, close/cancel/flatten, degraded gating, and reconnect review decisions against live demo infrastructure

The blocker is concrete:

- `TV_BOT__MARKET_DATA__API_KEY` is available in this workspace
- Tradovate demo credentials are not available in this workspace:
  - `TV_BOT__BROKER__USERNAME`
  - `TV_BOT__BROKER__PASSWORD`
  - `TV_BOT__BROKER__CID`
  - `TV_BOT__BROKER__SEC`
- `paper_account_name` is still unset in `config/runtime.local.toml`

## Final Release Gate Check

Against the final release gate in `V1_ACCEPTANCE_CRITERIA.md`:

1. strict strategy files load and validate correctly: met
2. manual warmup works: met
3. paper mode works end-to-end through Tradovate demo/paper: blocked externally in this workspace
4. arming is enforced for all trading: met
5. broker/account/data/storage health are surfaced in readiness view: met
6. dashboard supports core control-center functions: met in code, final production sign-off still pending
7. full event journal and structured debug logging exist: met
8. PnL includes fees/commission/slippage tracking: met
9. latency metrics exist: met
10. critical reconnect/open-position safety flows work: met in host/operator acceptance coverage
11. primary Postgres persistence works and fallback behavior is safe: substantially met in code/tests, final release-candidate walkthrough still pending
12. tests cover the most important safety-critical flows: met

## Packaging Note

The Windows packaging path now produces a current-candidate bundle for `19d39bf`, and the PowerShell packaging script has been hardened to fail fast when `cargo` or `npm` commands fail instead of silently continuing after a broken step.

For Windows packaging runs, stop any running local runtime or dashboard dev server first so locked binaries or native Node modules do not interfere with the build.

## Remaining Path To V1 Sign-Off

1. Provide Tradovate demo credentials and an explicit paper account name.
2. Run `docs/ops/paper_demo_verification.md` on candidate `19d39bf`.
3. Run the remaining storage/audit operator verification from `docs/ops/release_checklist.md`.
4. Record the exact runtime config, strategy file, and candidate commit used for that validation session.
5. Mark release complete only if the validation notes, CI results, packaging artifact, and runbooks all point to the same candidate commit.
