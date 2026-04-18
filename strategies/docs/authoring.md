# Strategy Authoring Guide

This guide explains how to author strict Markdown strategy files for this repository.

## Canonical Example

Use this file as the current canonical sample and layout reference:

- `strategies/examples/micro_silver_elephant_tradovate_v1.md`

It is also the default strategy referenced by `config/runtime.example.toml`.

## Rules

- Strategy files are Markdown.
- Runtime behavior comes from structured YAML blocks only.
- Each required section must contain one YAML block.
- Unknown required sections are invalid.
- Unknown fields should fail validation unless the schema explicitly allows warning-only behavior.
- Prose may explain intent, but prose must not change execution behavior.

## Required Sections

Every strategy file must include:

1. `Metadata`
2. `Market`
3. `Session`
4. `Data Requirements`
5. `Warmup`
6. `Signal Confirmation`
7. `Entry Rules`
8. `Exit Rules`
9. `Position Sizing`
10. `Execution`
11. `Trade Management`
12. `Risk`
13. `Failsafes`
14. `State Behavior`
15. `Dashboard Display`

The canonical schema definition remains:

- `STRATEGY_SPEC.md`

## Authoring Workflow

1. Start from `strategies/examples/micro_silver_elephant_tradovate_v1.md`.
2. Update metadata, market, session, and data requirements first.
3. Define warmup explicitly for every required timeframe.
4. Define execution and broker-preference behavior explicitly.
5. Define risk and failsafes before treating the strategy as runnable.
6. Validate the strategy through the runtime host, dashboard, or CLI before using it operationally.

## Practical Authoring Checklist

- `strategy_id` is unique and stable
- `schema_version` is present
- market family is correct
- contract selection is explicit
- timezone is explicit
- warmup covers every timeframe used by the strategy
- broker preference requirements are explicit
- risk limits are explicit
- failsafes are explicit
- dashboard display preferences are explicit

## Common Mistakes

- prose outside YAML describing behavior that the runtime cannot see
- missing required sections
- mismatched warmup and timeframe requirements
- leaving broker safety preferences ambiguous
- assuming a freeform scripting model exists in V1

## Validation Surfaces

You can validate a strategy through:

- dashboard upload and validation
- runtime host strategy validation endpoints
- CLI load flow through the local control plane

For the complete field-by-field schema, use:

- `STRATEGY_SPEC.md`
