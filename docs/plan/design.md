# Design: publish-path benchmark harness

> Transplanted from the main repo's `add-publish-benchmark` change and
> revised for standalone placement (user decisions 2026-11-02: sibling repo,
> zero main-repo footprint, fully standalone fixture formats).

## Context

See proposal.md for motivation and boundaries. The harness follows the main
repo's benchmark conventions (`grant-benchmark`, `metadata-benchmark`):
dedicated crate, throwaway SAM stack, samconfig, Makefile deploy/invoke/
teardown pair, self-cleaning fixtures.

The surfaces the harness touches (verified against the platform code and
template at `tune-publish-path` completion):

- Publish route: `POST /publish` on both auth surfaces; the IAM surface
  authorizes by `BindingPrincipal::aws_account` against the bindings table.
- Domain lifecycle over the same IAM surface: `POST /domains` (register,
  provisioned asynchronously by Step Functions), `GET /domains/{name}`,
  `PUT /domains/{name}` (status transitions, e.g. back to `INACTIVE`), and
  `POST /schemas` for schema registration.
- Domain bus naming: `{org}-{env}-{domain}-event-bus`.
- Platform metrics: `Konductor/EventStreams` namespace with
  `PublishAdmissionLatency`, `PublishQueueWait`, `PublishEnqueueToDelivery`;
  queue and DLQ depth via `AWS/SQS`; fixed resource naming
  (`konductor-publish-queue` and its DLQ) lets the stack construct ARNs by
  convention.
- issue #6 profile and baselines: 1,000 messages/sec sustained, warm-up
  excluded; current-path median 164.7 ms / p99 295.7 ms; native floor
  74.7 / 142.6 ms; 294 throttles at reserved concurrency 100; 422,721
  accepted events all eventually delivered.
- Binding item format: duplicated from the main repo's
  `lib/konductor-common/src/grant_store.rs` (`binding_to_item`). This is the
  one intentional duplication; the copy site in this repo links the source
  path and the main repo's commit it was copied from.

## Goals / Non-Goals

**Goals:**

- One `make benchmark-publish` invocation produces a complete JSON report:
  client-side publish latency, platform enqueue-to-delivery numbers,
  arrival at a subscriber target, throttle counts, DLQ reconciliation, and
  the native EventBridge floor, each against the issue #6 baselines.
- Repeatable and self-cleaning: idempotent setup, fixtures removed after
  every run, teardown deletes everything the stack created.
- Standalone: builds and runs without the platform repo present.

**Non-Goals:**

- External or multi-region load drivers (user decision: in-region only).
- OAuth-path benchmarking (user decision: seeded IAM publisher binding).
- Re-running legacy-behavior A/B configurations; operators can do that by
  changing the `tune-publish-path` settings on a dev stack.
- CI performance gating or continuous benchmarking.
- Any change to the platform repository or platform behavior.

## Decisions

### 1. One Lambda function, phased run: setup → warm-up → measure → drain → collect → report → cleanup

Single crate at this repo's root (`provided.al2023`, arm64) with tokio and
reqwest. IAM authentication to API Gateway requires SigV4 (`execute-api`);
the `aws-sigv4` crate signs requests with the function's execution-role
credentials.

Setup verifies rather than provisions. Domain registration requires human
approval (`decide-registration`) and enable/disable is platform-admin-only
on the OAuth surface, so an IAM-authenticated harness cannot own a domain's
lifecycle (discovered during implementation; user decision 2026-11-02). The
benchmark domain is a pre-existing domain named by configuration
(`BENCH_DOMAIN` environment variable from a stack parameter). Setup checks
`GET /domains/{name}` and fails with operator guidance ("register and
approve a domain for benchmarking") unless it is ACTIVE. It then ensures
the benchmark schema exists via `POST /schemas`, tolerating a conflict as
"already there".

*Alternative considered:* harness-initiated registration with a pause for
human approval, resuming on the next invoke. Rejected: the human step lands
on the first run anyway; pointing the harness at an existing domain is
simpler and is what the user asked for.

### 2. Fixtures the function manages vs. resources the stack manages

Per-run lifecycles live in the function and are cleaned up on every
reachable path: the IAM-account publisher binding on the benchmark domain
(plus whatever `POST /schemas` requires of an IAM caller — pinned against
the platform handlers in task 2.2), and the arrival-probe EventBridge rule with its CloudWatch
Logs target on `{org}-{env}-{domain}-event-bus` (the bus only exists after
provisioning, so the rule cannot be stack-created). The stack carries only
the function, its log group, and its role, all deleted on teardown.
`ReservedConcurrentExecutions: 1` makes concurrent runs impossible.

### 3. Load generation: paced open-loop at 1,000 msg/s by default

A tokio interval paces one request per tick (1 ms at the default rate);
each tick spawns the send on a bounded in-flight budget (default 200),
recording send timestamp, duration, and status class. Open-loop pacing
matches the issue #6 profile and exposes queueing as latency rather than
hiding it behind a closed-loop client. Rate, warm-up, measurement duration,
and the in-flight cap are payload overrides; the defaults reproduce issue #6
(~7 minutes of measured traffic ≈ 420k publishes). Each publish body
carries a trace id and the client's send timestamp so arrival latency can
be computed downstream.

Measured calls never retry: reqwest does not retry by default, and the
EventBridge client for the floor probe is built with SDK retries disabled
(max attempts 1).

### 4. Four measurement surfaces, one report

- **Client side**: request-latency percentiles (p50/p95/p99/max) and the
  202/429/other status mix, computed in-function.
- **Platform internals**: after a fixed drain/settle window,
  `GetMetricStatistics` with extended statistics on `Konductor/EventStreams`
  over the measured window, plus publish DLQ depth from SQS.
- **Arrival**: the arrival rule delivers every benchmark event to a
  CloudWatch log group; a Logs Insights query counts arrivals and computes
  arrival latency from the embedded send timestamp versus ingestion time.
  The report reconciles accepted count against arrivals plus DLQ depth.
- **Native floor**: a short `PutEvents` probe against a function-owned
  throwaway bus target re-baselines EventBridge on the day of the run.

The JSON report embeds the issue #6 baseline constants and prints deltas
next to each measured number.

### 5. Standalone wiring

`benchmark-stack.yaml`, `samconfig.toml`, and `Makefile` live at this
repo's root. The API endpoint, organization, environment, and queue names
are deploy-time parameters; the Makefile resolves them from the deployed
core stack (outputs / naming convention) via
`aws cloudformation describe-stacks` and passes parameter overrides. The
function role is least-privilege: `execute-api:Invoke` on the core API
stage, `dynamodb:PutItem/DeleteItem` on the bindings table, EventBridge
rule management scoped to the benchmark bus name pattern, `PutEvents` for
the floor probe, CloudWatch `GetMetricStatistics`, Logs Insights queries on
its own log groups, and SQS read on the publish queue/DLQ.

## Risks / Trade-offs

- [The configured domain is missing or not ACTIVE] → setup fails before
  any fixture is written, with guidance naming the domain and the
  registration step an operator must complete first.
- [1,000 msg/s plus the platform's own traffic nears the dev account's API
  Gateway account throttle] → the default profile matches issue #6 exactly;
  the platform's `tune-publish-path` throttle-budget warning guards the
  platform side; the driver counts its own 429s separately.
- [Metric filters and SQS metrics lag the run by minutes] → fixed settle
  window before collection; window bounds are recorded in the report.
- [Arrival latency from ingestion time is coarser than true target
  invocation] → documented in the report; like-for-like with the observable
  issue #6's native comparison used.
- [A failed run leaves fixtures behind] → cleanup runs on every reachable
  failure path and removes only what the run created (bindings, arrival
  rule, log target); the domain itself is never mutated.
- [The duplicated binding item format drifts when the platform changes it]
  → the copy site links the exact source path and upstream commit; a unit
  test here pins the duplicated shape so a deliberate upstream change is a
  visible, one-file update.

## Migration Plan

None: new repo, new stack. Validation is the rollout step
`tune-publish-path` deferred: deploy the tuned platform to dev, run
`make benchmark-publish`, compare against the issue #6 baselines, then
`make teardown-publish-benchmark`.

## Open Questions

- Whether the arrival probe's Logs Insights latency math (ingestion time
  minus embedded send timestamp) is tight enough to quote as p99, or is
  reported as count-plus-approximation only. Affects report wording, not
  the harness shape.
