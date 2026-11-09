# Design

## Context

See proposal.md - Why. The platform repo (`../konductor4evb`) already has a mature `.github/` setup by the same author: three-axis labels, four issue forms, SHA-pinned workflows, dependabot, rulesets. This repo's gates live in its Makefile (`check`: fmt + clippy; `test`: cargo test + `pytest tests/`). The live label set on this repo is the platform's, applied by hand at some point.

## Goals / Non-Goals

**Goals:**
- Contributor-facing settings that describe this repository: a publish-API benchmark harness (Rust driver, SAM stack, Makefile, pytest template checks)
- Reuse the platform's conventions wherever they are not repo-specific, so working across both repos feels uniform
- The live label set ends up matching `labels.yml`

**Non-Goals:**
- No branch-protection rulesets (`main-integrity.json` / `main-review.json`) and no CODEOWNERS import; deferred, the harness is single-maintainer
- No droid automation workflows (`droid.yml`, `droid-review.yml`); out of the chosen scope
- No docs workflow; this repo has no docs site
- No label-sync workflow and no `test_repository_config.py`-style consistency test; both stay platform-side for now (possible follow-up)

## Decisions

### Decision 1: Adapt, don't invent

Every file is a port of the platform's equivalent with only repo-specific edits: dropdown options, contact-link URLs, ecosystem lists, and the CI job bodies. This keeps maintenance knowledge transferable and review against the originals easy.

### Decision 2: Area taxonomy mirrors the repo layout

`area:driver` (src/ Rust Lambda driver), `area:infra` (benchmark-stack.yaml, samconfig.toml, Makefile), `area:ci`, `area:deps`, `area:docs`. The last four names match the platform exactly; only `driver` is new. Platform areas whose components do not exist here (web, sdk, notifications, trust, domain-mgmt, schema-mgmt, event-streams, control-plane) are not defined and are deleted from the live repo.

### Decision 3: Labels are applied imperatively, not synced

Following the platform's documented practice, `labels.yml` is the source of truth and a maintainer applies it with a `gh label create --force` loop, deleting labels the file does not define. The one-time application (including deleting the 8 platform area labels) is an apply-phase task executed with explicit confirmation; nothing in CI mutates labels.

### Decision 4: CI mirrors the Makefile gates, SHA-pinned

Jobs: check (`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`) -> test (`cargo test`, `uv run --with pytest --with pyyaml pytest tests`) -> build (`cargo lambda build --release --arm64`, via `cargo-lambda` installed with taiki-e/install-action). Conventions carried from the platform: `push` limited to main plus `pull_request`, SHA-keyed concurrency group so duplicate runs collapse, `permissions: contents: read`, actions pinned by SHA, `Swatinem/rust-cache` with a shared key.

### Decision 5: The bug form asks for benchmark evidence

A bug here means a bad run or a wrong measurement, so the form requires the run profile (the `PAYLOAD` used) and a report excerpt alongside the platform's fields (description, steps, commit, region). The chooser's security contact link points to this repository's advisories, not the platform's.
