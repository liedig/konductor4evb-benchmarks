# Design

## Context

See proposal.md - Why. `RunConfig::resolve` in `src/handler.rs` is the single place where payload overrides are parsed and validated; `abort_error_rate` is already range-checked there with a descriptive `bail!`. The load phase (`src/load.rs`) trusts its inputs by design.

## Goals / Non-Goals

**Goals:**
- Reject `abort_min_samples: 0` at configuration resolution, before any fixture or probe is created
- Error message names the parameter and the constraint, like the `abort_error_rate` message does

**Non-Goals:**
- No upper bound on `abort_min_samples` (a huge value just means "never abort," which is a legitimate operator choice)
- No validation inside `load.rs`; it keeps trusting its inputs
- No change to the abort predicate itself

## Decisions

### Decision 1: Validate in `resolve`, not in `load::run_load`

All payload validation already lives in `RunConfig::resolve` (u32 range checks, `abort_error_rate` bounds). Adding the check there keeps one validation boundary and fails the invocation before setup touches any AWS resources. Validating in the load phase would waste the fixture seeding, arrival probe, and floor bus creation on a config that was never viable.

### Decision 2: Reject only zero

The failure mode being fixed is the guard becoming unconditional (`measured >= 0` is always true). Any positive value preserves the guard's "minimum evidence before aborting" semantics, so only zero is an error.
