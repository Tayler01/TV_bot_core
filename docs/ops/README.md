# Operations Docs

This directory now holds the operator runbooks and release-check materials for V1 hardening.

## Available Runbooks

- `paper_demo_verification.md`
  Manual paper/demo verification flow for the release gate.
- `storage_fallback_override.md`
  What to do when Postgres is unavailable and the runtime asks for a temporary SQLite override.
- `reconnect_and_shutdown_review.md`
  Operator handling for reconnect and shutdown review-required states with active exposure.
- `release_checklist.md`
  Final release verification checklist covering CI, paper acceptance, dashboard verification, and packaging follow-through.

## Packaging

Use the checked-in packaging scripts to build a release bundle from the workspace root:

- Windows: `.\scripts\package_release.ps1`
- Linux/macOS: `./scripts/package_release.sh`

Both scripts produce release artifacts under `dist/releases/` and include:

- runtime and CLI binaries
- the built dashboard bundle
- sample runtime config
- operator runbooks
- example strategies
- a release manifest with commit and build metadata

## Scope

These docs are for local operator/runtime behavior only:

- the dashboard still talks only to the local control API
- runtime decisions remain the source of truth
- dangerous actions must remain explicit, reviewable, and journaled
