# Tasks

## 1. Labels

- [x] 1.1 Create `.github/labels.yml`: port the platform's priority, type, status, and community labels verbatim; define `area:driver`, `area:infra`, `area:ci`, `area:deps`, `area:docs` with platform-matching descriptions; keep the header comment documenting the `gh label create --force` application loop — verify: `uv run --with pyyaml python -c "import yaml; yaml.safe_load(open('.github/labels.yml'))"` parses, and every label referenced by the issue forms and dependabot config (tasks 2 and 5) appears in the file

## 2. Issue templates

- [x] 2.1 Create `.github/ISSUE_TEMPLATE/bug_report.yml` adapted from the platform: component dropdown becomes Driver / Stack & Makefile / CI / Docs; adds required run-profile and report-excerpt fields; keeps commit and AWS Region fields — verify: YAML parses; form labels are `type:bug`, `status:needs-triage`
- [x] 2.2 Create `.github/ISSUE_TEMPLATE/feature_request.yml`, `question.yml`, and `documentation.yml`, near-verbatim ports with their platform type labels — verify: YAML parses; each applies its type label plus `status:needs-triage`
- [x] 2.3 Create `.github/ISSUE_TEMPLATE/config.yml`: blank issues disabled; security contact link points to `liedig/konductor4evb-benchmarks` security advisories — verify: YAML parses; no link references the platform repo

## 3. Pull request template

- [x] 3.1 Create `.github/pull_request_template.md` adapted from the platform: Summary / Labels / Test Plan sections, label axes listing this repo's areas, checklist naming `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and `uv run --with pytest --with pyyaml pytest tests` — verify: every checklist command exists in the Makefile

## 4. Workflows

- [x] 4.1 Create `.github/workflows/ci.yml`: check (fmt + clippy) -> test (cargo test + pytest via uv) -> build (`cargo lambda build --release --arm64`, cargo-lambda via taiki-e/install-action); push-to-main + pull_request triggers, SHA-keyed concurrency group, `permissions: contents: read`, all actions SHA-pinned with version comments — verify: `uv run --with pyyaml python -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"` parses; `actionlint .github/workflows/ci.yml` passes if available
- [x] 4.2 Create `.github/workflows/issues.yml` as a verbatim port of the platform's resolve-referenced-issues workflow — verify: YAML parses; diff against the platform original shows no unintended edits

## 5. Dependabot

- [x] 5.1 Create `.github/dependabot.yml` with weekly Monday updates for the `cargo` and `github-actions` ecosystems only, labels `type:chore` + `area:deps` (cargo) / `area:ci` (github-actions), and the platform's AWS-SDK and rust-platform cargo groups — verify: YAML parses; every referenced label exists in `labels.yml`

## 6. Live label sync

- [x] 6.1 Apply `labels.yml` to the live repo with the documented `gh label create --force` loop, then delete the 8 platform area labels the file does not define (`area:domain-mgmt`, `area:schema-mgmt`, `area:event-streams`, `area:control-plane`, `area:notifications`, `area:trust`, `area:web`, `area:sdk`) — requires explicit user confirmation at apply time — verify: `gh label list --repo liedig/konductor4evb-benchmarks` output matches `labels.yml` exactly

## 7. Integration Verification

- [x] 7.1 Run `make test` — verify: cargo tests and template pytest checks still pass (no repo code changed, guards against accidental edits)
- [x] 7.2 Run `openspec validate align-repo-settings` — verify: change validates clean
