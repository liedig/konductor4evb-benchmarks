# konductor-benchmarks

Throwaway, in-region benchmark harness for the
[Konductor for Amazon EventBridge](../konductor4evb) publish path. Deploy,
invoke, tear down — nothing here is part of the platform.

The harness validates the `tune-publish-path` performance work against the
issue #6 baseline: it drives the IAM-authenticated `POST /publish` route of a
deployed dev instance at the issue #6 profile, measures client-side latency,
the platform's `Konductor/EventStreams` stage metrics, end-to-end arrival on
the domain bus, and a native `PutEvents` floor, and prints one JSON report
with every number next to the recorded baselines.

## Prerequisites

- A deployed dev instance of the platform (the `konductor-core` stack) to run
  against; this harness does not deploy one.
- A pre-existing, ACTIVE domain to benchmark. Registration requires human
  approval, so the harness points at a domain you already control instead of
  creating one.
- The same toolchain as the main repo: cargo-lambda, SAM CLI, AWS credentials
  for the dev account, `jq`.

## Usage

```bash
make benchmark-publish BENCH_DOMAIN=my-benchmark-domain
make teardown-publish-benchmark
```

`benchmark-publish` builds the driver, resolves the core stack's API endpoint,
bindings table, organization, and environment via `describe-stacks`, deploys
the `konductor-publish-benchmark` stack, invokes the function, and writes the
report to `/tmp/publish-benchmark-report.json` (a summary prints via `jq`).

If the invoke fails during setup with "does not exist" or "not ACTIVE", the
domain is still registering or provisioning — wait for approval/ACTIVE, then
re-run the same command. Fixtures (publisher binding, arrival rule and log
group, floor bus) are created per run and removed on every reachable exit;
a leftover from a hard interruption is reused on the next run.

### Profile

With `PAYLOAD={}` the run reproduces issue #6: 1,000 publishes/sec, 30 s
warm-up excluded from measurement, 420 s measured (~420k publishes), 200
in-flight cap, then a short native floor probe (100/s for 30 s) and a 120 s
settle window before collection. The run aborts early (fixtures still cleaned
up) when the measured failure rate exceeds 5% after the first 100 samples.

Override any knob via the invocation payload:

```bash
make benchmark-publish BENCH_DOMAIN=my-benchmark-domain \
  PAYLOAD='{"rate_per_sec": 200, "warmup_secs": 10, "measure_secs": 60, "max_in_flight": 50, "settle_secs": 60, "floor_rate_per_sec": 50, "floor_measure_secs": 15}'
```

### Report fields

- `publish` — client-observed `POST /publish` latency percentiles over
  accepted (202) requests, plus the 202/429/other status mix.
- `floor` — the same percentiles for native `PutEvents` on a throwaway bus
  (SDK retries disabled), the native floor.
- `stages` — the platform's own `PublishAdmissionLatency`,
  `PublishQueueWait`, and `PublishEnqueueToDelivery` percentiles over the
  measured-plus-settle window (`null` when the platform emitted nothing).
- `arrivals` — events that reached the domain bus (count plus
  ingestion-minus-send latency percentiles from the arrival rule's log).
- `reconciliation` — accepted vs arrivals + publish-DLQ depth; `matched` is
  false when events vanished or duplicated, with the signed `unaccounted`
  gap. A mismatch is reported, never hidden.
- `baselines` / `comparisons` — the recorded issue #6 numbers (platform
  p50 164.7 ms / p99 295.7 ms / 294 throttles; native p50 74.7 ms /
  p99 142.6 ms) and each measured value next to its baseline.
- `profile` — the effective profile, including whether the run aborted.

## Local gates

```bash
make check   # cargo fmt --check + clippy -D warnings
make test    # cargo test + pytest template checks
```

## Plan and design

The full plan (proposal, spec, design, tasks) lives in
[`docs/plan/`](docs/plan/proposal.md).
