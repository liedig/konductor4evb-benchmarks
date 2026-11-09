# Tasks

## 1. Validation in `RunConfig::resolve`

- [x] 1.1 Reject `abort_min_samples: 0` in `RunConfig::resolve` with an error naming the parameter, mirroring the `abort_error_rate` check — verify: new unit test `resolve_rejects_zero_abort_min_samples` passes, error message contains `abort_min_samples`
- [x] 1.2 Add unit test: positive override (e.g. `{"abort_min_samples": 50}`) resolves with that value — verify: test passes
- [x] 1.3 Add unit test: omitted override still defaults to 100 (covered by the existing defaults test; confirm it stays green) — verify: `resolve_defaults_to_the_issue6_profile` passes

## 2. Integration Verification

- [x] 2.1 Run `cargo test` for the full crate — verify: all 68 tests pass, no regressions in load-phase abort tests
- [x] 2.2 Run `openspec validate validate-abort-min-samples` — verify: change validates clean
