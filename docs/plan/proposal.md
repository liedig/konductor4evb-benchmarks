# Proposal: publish-path benchmark harness

> Transplanted from the main repo's `add-publish-benchmark` OpenSpec change
> on 2026-11-02. The user decided the harness stays out of the platform
> repository, so it lives here as a standalone, fully decoupled tool.

## Why

The `tune-publish-path` change reworked the publish path to close the
latency and throughput gap from issue #6 (median 164.7 ms / p99 295.7 ms
versus ~75 ms native EventBridge at 1,000 messages/sec, 294 throttles at
reserved concurrency 100). Its rollout plan requires re-running that load
profile against a dev deployment to prove the improvement — but no
publish-path benchmark exists. The grant engine and the metadata read path
each have a throwaway in-region harness in the main repo
(`grant-benchmark`, `metadata-benchmark`); this repo gives the publish path
the same discipline without adding tooling weight to the platform repo.

## What changes

- A standalone Rust Lambda crate (this repo's root crate): an in-region
  driver for the deployed platform's IAM-authenticated `POST /publish`
  route, pacing a configurable sustained rate (default 1,000 msg/s, the
  issue #6 profile) with a warm-up phase, recording client-side latency and
  the 202/429/4xx/5xx status mix.
- A native `PutEvents` floor probe in the same function, re-baselining
  EventBridge on the day of the run.
- End-to-end arrival measurement: a temporary EventBridge rule on the
  benchmark domain's bus delivers to a CloudWatch log group; the report
  reconciles accepted publishes against arrivals plus publish DLQ depth.
- Platform-side numbers collected from CloudWatch after the run:
  `PublishAdmissionLatency`, `PublishQueueWait`, `PublishEnqueueToDelivery`
  (namespace `Konductor/EventStreams`), throttle evidence, and DLQ depth.
- Fixture discipline identical to `grant-benchmark`: the function seeds its
  own IAM-account publisher binding and removes it after the run; the
  benchmark domain is a pre-existing domain named by configuration
  (registration needs human approval and disable is platform-admin-only, so
  the harness cannot own a domain's lifecycle). No admin tokens, no leftover
  state.
- A throwaway SAM stack (`benchmark-stack.yaml`) with its own samconfig and
  `benchmark-publish` / `teardown-publish-benchmark` Makefile targets,
  mirroring the main repo's `benchmark-grants` pair.
- A JSON report placing every measured number next to the recorded
  issue #6 baselines.

## Boundaries

- Zero footprint in the platform repo: no crates, Makefile targets,
  templates, docs, or changelog entries there (user decision 2026-11-02).
- Fully standalone: the small fixture formats (the bindings-table item
  shape) are duplicated from `konductor-common` rather than pulled in as a
  git dependency (user decision 2026-11-02); the drift risk is accepted and
  noted in design.md.
- The platform's publish behavior does not change; this is measurement
  tooling around it.

## Impact

- New repository only: root crate, `benchmark-stack.yaml`, `samconfig.toml`,
  `Makefile`, pytest template tests under `tests/`.
- Dependencies: tokio, reqwest, `aws-sigv4` (SigV4 for `execute-api`), the
  AWS SDK clients for DynamoDB/EventBridge/CloudWatch/Logs/SQS.
- Prerequisites: a deployed dev instance of the platform.
- Cost/safety: one default run sends ~420k publishes in a dev account;
  fixtures self-clean and the stack carries the `auto-delete=yes` tag.
