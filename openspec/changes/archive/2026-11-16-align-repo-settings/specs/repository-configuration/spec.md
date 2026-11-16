# Spec Delta

## ADDED Requirements

### Requirement: Label taxonomy

The repository SHALL define its label set in `.github/labels.yml` as the single source of truth for name, color, and description. The set MUST keep the platform's generic axes (priorities P0-P3, `type:*`, `status:*`, community labels) and MUST define area labels only for components this repository contains: driver, infra, ci, deps, and docs.

#### Scenario: Areas match the repo contents

- **WHEN** a maintainer triages an issue about any part of this repository
- **THEN** a matching area label exists (`area:driver`, `area:infra`, `area:ci`, `area:deps`, `area:docs`)
- **AND** no platform-component area labels (for example `area:web`, `area:sdk`, `area:domain-mgmt`) are defined

#### Scenario: Automatically applied labels are defined

- **WHEN** an issue form or a dependabot configuration applies a label automatically
- **THEN** that label is defined in `labels.yml`

### Requirement: Contribution templates

The repository SHALL provide issue forms for bug reports, feature requests, questions, and documentation, an issue chooser configuration, and a pull request template. Each form MUST auto-apply its type label plus `status:needs-triage`, and the bug form MUST collect benchmark context: the run profile (invocation payload) and a report excerpt.

#### Scenario: Bug form captures a benchmark run

- **WHEN** a contributor reports a bug through the form
- **THEN** the issue is labeled `type:bug` and `status:needs-triage`
- **AND** the form asks for the failing run's profile, a report excerpt, the commit, and the AWS Region

#### Scenario: Security reports stay private

- **WHEN** a reporter opens the issue chooser
- **THEN** a contact link directs vulnerability reports to this repository's private security advisories instead of a public issue

#### Scenario: Pull request checklist mirrors the gates

- **WHEN** a contributor opens a pull request
- **THEN** the description template includes a checklist naming the Makefile gates: `cargo fmt --check`, `cargo clippy`, `cargo test`, and the template pytest checks

### Requirement: CI gates

The repository SHALL run continuous integration on every pull request and on pushes to the default branch: format check, clippy with warnings denied, the Rust test suite, and the template pytest checks, followed by a release-mode arm64 build of the driver. Workflow actions MUST be pinned by commit SHA.

#### Scenario: Pull request runs all gates

- **WHEN** a pull request is opened or updated
- **THEN** the CI workflow runs format, clippy, both test suites, and the arm64 build
- **AND** a failure in any gate fails the workflow run

#### Scenario: Duplicate runs collapse

- **WHEN** a push and a pull request event fire for the same commit
- **THEN** concurrency grouping collapses them into a single workflow run

### Requirement: Repository automation

The repository SHALL close referenced issues when a pull request merges into the default branch, and SHALL keep dependencies current with weekly dependabot updates for the cargo and github-actions ecosystems only.

#### Scenario: Merged pull request resolves referenced issues

- **WHEN** a pull request targeting the default branch is merged
- **THEN** every issue its body references receives a comment linking the pull request and is closed as completed

#### Scenario: Dependabot scopes and labels its pull requests

- **WHEN** dependabot opens an update pull request
- **THEN** it covers only cargo or github-actions dependencies
- **AND** it applies `type:chore` with `area:deps` (cargo) or `area:ci` (github-actions)
