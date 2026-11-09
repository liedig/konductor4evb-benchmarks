# Tasks

> Transplanted from the main repo's `add-publish-benchmark` change, revised
> for this standalone repo (user decisions 2026-11-02). Tests and docs land
> with the group whose work calls for them.

## 1. Crate scaffold and measurement core

- [x] 1.1 Create the root crate (`Cargo.toml`, `src/main.rs`, `src/handler.rs` module layout) following the main repo's `grant-benchmark` layout, and verify `cargo check` passes. Duplicate the binding item format from the main repo's `lib/konductor-common/src/grant_store.rs` with a pinned source-path-and-commit comment, and write a unit test pinning the duplicated shape.
- [x] 1.2 Write failing unit tests for the open-loop pacer (ticks at the configured rate, warm-up samples tagged and excluded, in-flight cap respected, duration bounds the run) using a virtual-clock-friendly design, then implement; verify `cargo test pacer` passes.
- [x] 1.3 Write failing unit tests for the report math (p50/p95/p99/max, status-mix counting, issue #6 baseline deltas) and implement; verify `cargo test report` passes.
- [x] 1.4 Write failing unit tests for SigV4-signed request construction against `execute-api` (signed headers present, payload hash matches) using fixed credentials, then implement the signed reqwest client; verify `cargo test sign` passes.

## 2. Fixture lifecycle

- [ ] 2.1 Write failing tests for domain setup against a stubbed platform API: a configured domain that is ACTIVE proceeds to schema ensure (`POST /schemas`, conflict tolerated); a missing or non-ACTIVE domain fails before any fixture write, with guidance naming the domain and the registration prerequisite. Implement; verify `cargo test setup` passes.
- [x] 2.2 Write failing tests for the binding fixtures: seed the IAM-account publisher binding (plus whatever `POST /schemas` requires of an IAM caller — pin the exact set against the platform's schema-admin handler) and remove them on success and on every reachable failure path. Implement; verify `cargo test fixtures` passes.
- [x] 2.3 Write failing tests for the arrival probe fixture: create rule + CloudWatch Logs target on `{org}-{env}-{domain}-event-bus`, remove both at cleanup, tolerate already-exists/already-gone. Implement; verify `cargo test arrival_probe` passes.

## 3. Run phases and collection

- [x] 3.1 Write failing tests for the measured-load phase: drives the configured profile, classifies 202/429/other, records per-request latency, and aborts early on a client error-rate threshold with fixtures still cleaned up. Use a mock HTTP server or an injected transport. Implement; verify `cargo test load` passes.
- [x] 3.2 Write failing tests for the native `PutEvents` floor probe: SDK retries disabled (max attempts 1), warm-up excluded, percentile report identical in shape to the publish probe. Implement; verify `cargo test floor` passes.
- [x] 3.3 Write failing tests for the post-run collection: `GetMetricStatistics` extended statistics over the measured window for `PublishAdmissionLatency`, `PublishQueueWait`, `PublishEnqueueToDelivery` in `Konductor/EventStreams`, publish DLQ depth, and the arrival Logs Insights query (count + ingestion-time latency), with the settle window applied first. Mock CloudWatch/Logs/SQS clients. Implement; verify `cargo test collect` passes.
- [x] 3.4 Write a failing test for the end-to-end reconciliation (accepted count == arrivals + DLQ depth, mismatch reported not hidden) and the final report JSON shape (all four surfaces + baselines + effective profile). Implement; verify `cargo test reconcile` passes.

## 4. Stack, wiring, and docs

- [x] 4.1 Write failing template tests in `tests/test_benchmark_template.py` (log group with Delete policies, `ReservedConcurrentExecutions: 1`, least-privilege role: `execute-api:Invoke` scoped to the core API stage, bindings-table Put/DeleteItem, events rule management scoped to the benchmark bus pattern, `PutEvents` for the floor probe, CloudWatch/Logs/SQS reads), then create `benchmark-stack.yaml` and `samconfig.toml`. Verify `uv run --with pytest --with pyyaml pytest tests` and `sam validate --lint` pass.
- [x] 4.2 Add the `benchmark-publish` and `teardown-publish-benchmark` Makefile targets (cargo-lambda build, sam build/deploy with the core stack's API endpoint, org, and environment resolved from `aws cloudformation describe-stacks` as parameter overrides, invoke with report to `/tmp/publish-benchmark-report.json`, re-invoke hint when setup is still provisioning). Verify `make -n benchmark-publish` renders the expected commands.
- [x] 4.3 Fill in this repo's README usage section against the finished targets (profile defaults, report fields, issue #6 baselines, resume-on-provisioning behavior). Verify the documented commands match the Makefile.

## 5. Integration checks

- [x] 5.1 Run the full local gates: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `uv run --with pytest --with pyyaml pytest tests`, `sam validate --lint`. All pass.
- [x] 5.2 Self-review the crate against the main repo's Rust conventions (no unwrap/expect outside tests, no sensitive data in logs, errors traced before swallow) and resolve every finding; re-run the gates in 5.1.
- [x] 5.3 Live validation against a dev deployment (authorized separately): deploy the tuned core stack, `make benchmark-publish`, confirm the report reconciles all accepted events and shows the improvement over the issue #6 baselines, then `make teardown-publish-benchmark`. Record the measured numbers in the main repo's `docs/decisions/tune-publish-path.md` under Evidence (the one intentional write back to the platform repo, as its rollout plan requires).
