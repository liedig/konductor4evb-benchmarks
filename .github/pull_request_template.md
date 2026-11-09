## Summary

<!-- What does this PR do and why? -->

## Labels

<!-- Apply at minimum one type: and one area: label. Add P0-P3 if urgency is clear. -->
- type: `type:bug` / `type:feature` / `type:refactor` / `type:chore` / `type:test` / `type:docs` / `type:security`
- area: `area:driver` / `area:infra` / `area:ci` / `area:deps` / `area:docs`
- priority (optional): `P0` / `P1` / `P2` / `P3`

## Test Plan

<!-- How was this tested? -->

## Checklist

- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test` passes
- [ ] `uv run --with pytest --with pyyaml pytest tests` passes
- [ ] README or docs/plan updated (if applicable)
