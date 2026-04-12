# Thread Handoff

This note is the fast handoff context for continuing work in a new Codex thread.

## Read First

In the next thread, read these documents before making changes:

1. `AGENTS.md`
2. `codex_futures_bot_plan.md`
3. `STRATEGY_SPEC.md`
4. `V1_ACCEPTANCE_CRITERIA.md`
5. `docs/architecture/current_status.md`
6. `docs/architecture/phase_0_phase_1_foundations.md`

## Current Checkpoint

- The last major implementation checkpoint before this handoff note was commit `86a00b7`.
- That checkpoint completed the acceptance-hardening slice for:
  - reconnect review close-position dispatch through the audited flatten path
  - signal-time shutdown blocking until explicit review
  - execution-layer new-entry blocking
  - paper-account same-side scale-in dispatch
- The repository is intended to move out of OneDrive and use the new non-OneDrive clone as the primary workspace.

## What Is Already In Place

- Phases 0 through 6 foundations are substantially implemented.
- The strategy-agnostic execution core boundaries are in place and must remain strict.
- The runtime host, CLI, persistence, journal, metrics, health, durable history projection, market data, broker integration, risk engine, execution engine, and strategy runtime foundations are implemented.
- The local control plane is live through HTTP and WebSocket surfaces.
- Reconnect/open-position review and shutdown-with-open-position safety flows are implemented and acceptance-hardened.

## What Still Comes Next

The next highest-priority work is:

1. Finish the remaining full end-to-end paper-mode acceptance campaign from `V1_ACCEPTANCE_CRITERIA.md`.
2. Only after that, move into dashboard work.

The remaining paper-mode push should focus on complete operator-visible flows rather than isolated crate behavior where possible.

## Constraints To Preserve

- Keep the execution core strategy-agnostic.
- Do not move strategy-specific logic into broker, execution, risk, persistence, control API, or dashboard code.
- Keep arming explicit.
- Keep runtime mode explicit.
- Keep broker-side protections preferred where supported.
- Journal and log important actions and state transitions.
- Add tests for safety-critical behavior.

## Recommended First Prompt For The New Thread

Use this as the first prompt in the new workspace:

```text
Read AGENTS.md, codex_futures_bot_plan.md, STRATEGY_SPEC.md, V1_ACCEPTANCE_CRITERIA.md, docs/architecture/current_status.md, docs/architecture/phase_0_phase_1_foundations.md, and docs/architecture/thread_handoff.md. This repo was moved out of OneDrive and this is now the primary workspace.

Continue from the latest main branch state. The reconnect/shutdown acceptance hardening pass is complete, and the next priority is the remaining full end-to-end paper-mode acceptance campaign before dashboard work.

Start by reviewing the repo state against V1_ACCEPTANCE_CRITERIA.md, list the remaining paper-mode acceptance gaps, and then implement the next highest-priority missing slice with tests. Follow architecture boundaries strictly and keep the execution core strategy-agnostic.
```

## Verification Notes

The recent targeted verification that informed this handoff included:

- `cargo test -j 1 -p tv-bot-execution-engine`
- `cargo test -j 1 -p tv-bot-runtime reconnect_review_close_position_dispatches_flatten_request`
- `cargo test -j 1 -p tv-bot-runtime shutdown_signal_blocks_until_operator_reviews_open_position`

Because this repository has been worked on under Windows with OneDrive in the path, transient `LNK1104` linker file locks were observed during some test reruns. A fresh non-OneDrive workspace should be the preferred environment going forward.
