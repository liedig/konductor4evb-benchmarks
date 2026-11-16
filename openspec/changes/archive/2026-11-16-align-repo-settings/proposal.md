# Proposal

## Why

The GitHub repository's settings describe the wrong codebase. The live label set was copied from the platform repo (`area:domain-mgmt`, `area:web`, `area:sdk`, ...), but none of those components exist here — this repo is the benchmark harness that exercises the platform's publish API. And the file-based surfaces (issue templates, workflows, dependabot, PR template) were never created: `.github/` does not exist. Contributors get no guidance, and nothing runs the repo's own gates (`cargo fmt`, `clippy`, `cargo test`, `pytest tests/`) on a pull request.

## What Changes

- Add `.github/labels.yml` as the label source of truth: keep the platform's generic axes (P0-P3, `type:*`, `status:*`, community labels) and replace the platform areas with this repo's real ones (`area:driver`, `area:infra`, `area:ci`, `area:deps`, `area:docs`).
- Add four issue forms (bug report, feature request, question, documentation) plus the chooser config; the bug form collects benchmark-specific context (run profile, report excerpt) and the security contact link points at this repo's advisories.
- Add a pull request template whose checklist mirrors this repo's Makefile gates.
- Add two workflows: `ci.yml` (check -> test -> arm64 build, adapted from the platform) and `issues.yml` (resolve referenced issues on merge, ported verbatim).
- Add `dependabot.yml` for the `cargo` and `github-actions` ecosystems only.
- Sync the live label set from `labels.yml` with `gh label create --force`, deleting the 8 platform `area:*` labels it does not define.

## Capabilities

### New Capabilities
- `repository-configuration`: The GitHub repository's contributor-facing configuration — label taxonomy, issue forms, PR template, CI gates, and dependency automation — kept consistent with what this repo actually contains.

### Modified Capabilities
<!-- None -->

## Impact

- New files only under `.github/`; no changes to `src/`, `tests/`, or the stack
- Live repository: label set replaced (8 platform area labels deleted, ~5 benchmark labels added); no other repo settings touched
- `openspec/`: this change's artifacts; the spec lands at `openspec/specs/repository-configuration/spec.md` on archive
