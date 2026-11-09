# Spec: publish-path benchmark

> Capability contract for this repo's harness, transplanted from the main
> repo's `add-publish-benchmark` change. Normative behavior uses SHALL/MUST.

## Purpose

A repeatable, in-region benchmark harness for the Konductor publish path:
fixed load profile, measurement discipline, fixture lifecycle, and report
contract, so publish-path performance claims (issue #6,
`tune-publish-path`, and future regressions) are validated the same way
every time.

## Requirements

### Requirement: In-region load driver

The harness SHALL run as an in-region Lambda function that drives the
deployed platform's IAM-authenticated `POST /publish` route over HTTP, and
SHALL NOT be part of the deployed platform stacks. The driver SHALL pace
requests at a configurable sustained rate with a configurable warm-up phase;
the defaults SHALL reproduce the issue #6 profile (1,000 messages per
second, warm-up excluded from the measurement window).

#### Scenario: Default profile matches issue #6

- **WHEN** the benchmark is invoked with no payload overrides
- **THEN** it drives 1,000 publishes per second with a warm-up phase whose
  samples are excluded from the reported measurement window

#### Scenario: Configurable profile

- **WHEN** the invocation payload supplies rate, warm-up, or duration values
- **THEN** the driver paces itself accordingly and echoes the effective
  profile in its report

### Requirement: Latency and throttle measurement

For the measured window, the report SHALL include client-side publish
request latency percentiles (p50, p95, p99, max), the status mix
(202/429/other), and the platform's `PublishAdmissionLatency`,
`PublishQueueWait`, and `PublishEnqueueToDelivery` metrics collected from
CloudWatch. A native `PutEvents` floor probe SHALL re-baseline EventBridge
on the day of the run, and the report SHALL place every measured number
next to the recorded issue #6 baselines.

#### Scenario: Throttles are visible

- **WHEN** the platform throttles publish requests during a run
- **THEN** the report shows the 429 count and the platform-side throttle
  evidence rather than hiding them inside latency statistics

#### Scenario: Same-day EventBridge floor

- **WHEN** a run completes
- **THEN** the report includes the native floor probe's percentiles next to
  the platform-path percentiles and the issue #6 native baseline

### Requirement: End-to-end arrival reconciliation

The harness SHALL deliver benchmark events to a subscriber target on the
benchmark domain's event bus and count arrivals. The report SHALL reconcile
accepted (202) publishes against delivered arrivals plus the publish DLQ
depth, so no accepted event is silently unaccounted for.

#### Scenario: Accepted events are accounted for

- **WHEN** the accepted count exceeds arrivals plus DLQ depth after the
  settle window
- **THEN** the report states the shortfall explicitly instead of passing

### Requirement: Fixture lifecycle

The harness SHALL run against a pre-existing domain named by configuration
(registration requires human approval and enable/disable is
platform-admin-only, so the harness cannot own a domain's lifecycle). It
SHALL verify the domain is ACTIVE before the load phase and fail with
operator guidance otherwise. It SHALL seed the fixtures it needs (at minimum
an IAM-account publisher binding for its own execution identity) and remove
them after the run, including on failure paths it can reach. SDK retries
SHALL be disabled for measured calls so latency reflects single attempts.

#### Scenario: Clean rerun

- **WHEN** the benchmark is invoked twice in a row
- **THEN** the second run sees the same starting state as the first
  (no leftover bindings, no leftover enabled benchmark domain)

### Requirement: Deploy-invoke-teardown operation

The harness SHALL ship as a throwaway stack with its own SAM configuration
and Makefile targets for deploy/run and teardown, mirroring the main repo's
grant benchmark targets. Teardown SHALL delete every resource the stack
created, including the temporary arrival rule and its log group.

#### Scenario: Not part of the platform

- **WHEN** the benchmark stack is deleted
- **THEN** no platform resource (API, queue, domain bus, core stack) is
  affected and no benchmark resource remains

### Requirement: Standalone repository

The harness SHALL live in this repository only. It SHALL NOT require
changes to the platform repository, and it SHALL NOT depend on the
platform's crates at build time; fixture formats are duplicated and the
duplication is documented at the copy site.

#### Scenario: Platform repo untouched

- **WHEN** the harness is built, deployed, run, and torn down
- **THEN** the platform repository needs no commit, branch, or working-tree
  change
