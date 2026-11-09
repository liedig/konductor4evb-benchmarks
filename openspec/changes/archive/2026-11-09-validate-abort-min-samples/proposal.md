# Proposal

## Why

The benchmark's early-abort guard exists so a single early publish failure cannot kill a multi-hour run, but the `abort_min_samples` payload override is never validated. An operator passing `"abort_min_samples": 0` silently defeats the guard: the abort predicate (`measured >= abort_min_samples && ...`) becomes true on the very first measured sample, so one transient failure can terminate a run that costs minutes of setup and a 7-minute measure window to repeat.

## What Changes

- `RunConfig::resolve` rejects an `abort_min_samples` payload override of `0` with a clear error, mirroring the existing `abort_error_rate` range validation.
- The default (`100`) and all positive overrides behave exactly as before.

## Capabilities

### New Capabilities
- `run-configuration`: Resolving and validating benchmark run configuration from stack-provided environment variables and Lambda payload overrides, so an invalid run fails fast at invocation instead of wasting a load window.

### Modified Capabilities
<!-- None, no existing specs -->

## Impact

- `src/handler.rs`: adds one validation check in `RunConfig::resolve` plus inline tests
- No changes to `src/load.rs` (it continues to trust its inputs), no API or dependency changes
