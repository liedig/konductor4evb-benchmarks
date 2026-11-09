//! The measured-load phase: paces publishes against the platform API with
//! per-request latency, classifies each response (202 accepted, 429
//! throttled, anything else failed, status 0 for transport failures), and
//! aborts early when the measured failure rate crosses a threshold. An
//! abort is data (`LoadOutcome::aborted`), not an error, so the handler
//! still collects, reports, and cleans up fixtures.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;

use crate::pacer::{self, Phase, Profile, Sample};
use crate::platform::{publish_body, PlatformApi};

pub const STATUS_ACCEPTED: u16 = 202;
pub const STATUS_THROTTLED: u16 = 429;
/// Synthetic status for transport failures: no HTTP response arrived.
pub const STATUS_TRANSPORT_ERROR: u16 = 0;

/// One publish attempt.
#[derive(Debug, PartialEq)]
pub struct PublishSample {
    pub trace_id: String,
    pub status: u16,
    pub latency_us: u64,
}

/// The finished load phase. Counts cover the measured window only;
/// warm-up samples stay available for debugging but never feed the report.
#[derive(Debug)]
pub struct LoadOutcome {
    pub samples: Vec<Sample<PublishSample>>,
    pub aborted: bool,
}

impl LoadOutcome {
    fn measured(&self) -> Vec<&Sample<PublishSample>> {
        pacer::measured_samples(&self.samples)
    }

    /// Accepted (202) publishes in the measured window.
    pub fn accepted(&self) -> usize {
        self.measured()
            .iter()
            .filter(|s| s.outcome.status == STATUS_ACCEPTED)
            .count()
    }

    /// Accepted (202) publishes across the whole run, warmup included:
    /// reconciliation owes every accepted publish an arrival or a DLQ
    /// entry, and warm-up sends are accepted publishes too.
    pub fn accepted_total(&self) -> usize {
        self.samples
            .iter()
            .filter(|s| s.outcome.status == STATUS_ACCEPTED)
            .count()
    }

    /// Throttled (429) publishes in the measured window.
    pub fn throttled(&self) -> usize {
        self.measured()
            .iter()
            .filter(|s| s.outcome.status == STATUS_THROTTLED)
            .count()
    }

    /// Everything else, including transport failures (status 0).
    pub fn failed(&self) -> usize {
        self.measured()
            .iter()
            .filter(|s| s.outcome.status != STATUS_ACCEPTED && s.outcome.status != STATUS_THROTTLED)
            .count()
    }

    /// Latencies of accepted publishes in the measured window: the latency
    /// surface describes the delivery path, so rejections (fast by design)
    /// stay in the status mix instead of skewing percentiles.
    pub fn accepted_latencies_us(&self) -> Vec<u64> {
        self.measured()
            .iter()
            .filter(|s| s.outcome.status == STATUS_ACCEPTED)
            .map(|s| s.outcome.latency_us)
            .collect()
    }

    /// The measured-window status mix for the report.
    pub fn status_mix(&self) -> crate::report::StatusMix {
        crate::report::status_mix(self.measured().iter().map(|s| s.outcome.status))
    }
}

/// A load run that was never executed (setup failed first) reports as
/// empty rather than panicking the report phase.
impl Default for LoadOutcome {
    fn default() -> Self {
        Self {
            samples: Vec::new(),
            aborted: false,
        }
    }
}
fn is_failure(status: u16) -> bool {
    status != STATUS_ACCEPTED && status != STATUS_THROTTLED
}

/// Drives `profile` against the platform API. Each tick publishes one
/// event whose trace id doubles as its idempotency key (`{run_id}-{tick}`),
/// so the arrival probe can match deliveries back to sends. Aborts early
/// when the measured failure rate (non-202/429, transport errors included)
/// exceeds `abort_error_rate`, evaluated only after `abort_min_samples`
/// measured samples so a single early failure cannot kill the run.
pub async fn run_load<P: PlatformApi + 'static>(
    api: &Arc<P>,
    run_id: &str,
    domain: &str,
    profile: &Profile,
    abort_error_rate: f64,
    abort_min_samples: usize,
) -> Result<LoadOutcome> {
    let send = {
        let api = api.clone();
        let run_id = run_id.to_string();
        let domain = domain.to_string();
        move |tick: u64| {
            let api = api.clone();
            let run_id = run_id.clone();
            let domain = domain.clone();
            async move {
                let trace_id = format!("{run_id}-{tick}");
                let body = publish_body(
                    &domain,
                    &trace_id,
                    chrono::Utc::now().timestamp_millis(),
                    &trace_id,
                );
                let started = Instant::now();
                let status = match api.publish(&body).await {
                    Ok(status) => status,
                    Err(e) => {
                        tracing::debug!(error = %e, %trace_id, "publish transport failure");
                        STATUS_TRANSPORT_ERROR
                    }
                };
                PublishSample {
                    trace_id,
                    status,
                    latency_us: started.elapsed().as_micros() as u64,
                }
            }
        }
    };
    // Cumulative tally over the measured window; the pacer hands over
    // only newly completed samples, so this stays O(batch) per tick.
    let tally = std::sync::Mutex::new((0usize, 0usize));
    let (samples, aborted) = pacer::run_until(profile, send, move |new_samples| {
        let Ok(mut tally) = tally.lock() else {
            return false;
        };
        for sample in new_samples {
            if sample.phase == Phase::Measured {
                tally.0 += 1;
                if is_failure(sample.outcome.status) {
                    tally.1 += 1;
                }
            }
        }
        let (measured, failed) = *tally;
        measured >= abort_min_samples && (failed as f64 / measured as f64) > abort_error_rate
    })
    .await;
    Ok(LoadOutcome { samples, aborted })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pacer::measured_samples;
    use serde_json::Value;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Publishes resolve from the tick encoded in the idempotency key
    /// (`{run_id}-{tick}`), so completion order never matters.
    struct StubApi {
        status_for: Box<dyn Fn(u64) -> Result<u16> + Send + Sync>,
        delay: Duration,
        bodies: Mutex<Vec<Value>>,
    }

    impl StubApi {
        fn new(status_for: impl Fn(u64) -> Result<u16> + Send + Sync + 'static) -> Self {
            Self {
                status_for: Box::new(status_for),
                delay: Duration::ZERO,
                bodies: Mutex::new(Vec::new()),
            }
        }
    }

    impl PlatformApi for StubApi {
        async fn ensure_schema(&self, _domain: &str) -> Result<crate::platform::SchemaEnsure> {
            unreachable!("the load phase never ensures the schema")
        }
        async fn publish(&self, body: &Value) -> Result<u16> {
            self.bodies.lock().unwrap().push(body.clone());
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let key = body["idempotencyKey"].as_str().expect("idempotency key");
            let tick: u64 = key.rsplit('-').next().unwrap().parse().unwrap();
            (self.status_for)(tick)
        }
    }

    fn run_id() -> &'static str {
        "bench-run"
    }

    #[tokio::test(start_paused = true)]
    async fn classifies_accepted_throttled_and_failed_statuses() {
        let api = Arc::new(StubApi::new(|tick| match tick % 4 {
            0 | 1 => Ok(202),
            2 => Ok(429),
            _ => Ok(500),
        }));
        let profile = Profile::new(8, 0, 1, 10).unwrap();

        let outcome = run_load(&api, run_id(), "benchmark", &profile, 0.9, 1)
            .await
            .unwrap();

        assert!(!outcome.aborted);
        assert_eq!(outcome.samples.len(), 8);
        assert_eq!(outcome.accepted(), 4);
        assert_eq!(outcome.throttled(), 2);
        assert_eq!(outcome.failed(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn warmup_publishes_are_excluded_from_the_counts() {
        let api = Arc::new(StubApi::new(
            |tick| if tick < 4 { Ok(429) } else { Ok(202) },
        ));
        let profile = Profile::new(4, 1, 1, 10).unwrap();

        let outcome = run_load(&api, run_id(), "benchmark", &profile, 0.9, 1)
            .await
            .unwrap();

        assert_eq!(outcome.samples.len(), 8);
        assert_eq!(measured_samples(&outcome.samples).len(), 4);
        assert_eq!(outcome.accepted(), 4);
        assert_eq!(outcome.throttled(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn transport_failures_count_as_failed_without_dropping_the_sample() {
        let api = Arc::new(StubApi::new(|tick| {
            if tick == 0 {
                Err(anyhow::anyhow!("connection reset"))
            } else {
                Ok(202)
            }
        }));
        let profile = Profile::new(4, 0, 1, 10).unwrap();

        let outcome = run_load(&api, run_id(), "benchmark", &profile, 0.9, 2)
            .await
            .unwrap();

        assert!(
            !outcome.aborted,
            "one transport failure stays below the threshold"
        );
        assert_eq!(outcome.failed(), 1);
        let failed = measured_samples(&outcome.samples)
            .into_iter()
            .find(|s| s.outcome.status == STATUS_TRANSPORT_ERROR)
            .expect("the failed publish still produces a sample");
        assert_eq!(failed.outcome.trace_id, "bench-run-0");
    }

    #[tokio::test(start_paused = true)]
    async fn aborts_early_when_the_failure_rate_crosses_the_threshold() {
        let api = Arc::new(StubApi::new(|_| Ok(500)));
        let profile = Profile::new(100, 0, 60, 50).unwrap();

        let outcome = run_load(&api, run_id(), "benchmark", &profile, 0.5, 10)
            .await
            .unwrap();

        assert!(outcome.aborted);
        assert!(
            outcome.samples.len() < 200,
            "a 6,000-tick run stops almost immediately, got {}",
            outcome.samples.len()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn runs_to_completion_below_the_threshold() {
        let api = Arc::new(StubApi::new(
            |tick| {
                if tick % 4 == 3 {
                    Ok(500)
                } else {
                    Ok(202)
                }
            },
        ));
        let profile = Profile::new(40, 0, 1, 10).unwrap();

        let outcome = run_load(&api, run_id(), "benchmark", &profile, 0.3, 4)
            .await
            .unwrap();

        assert!(!outcome.aborted, "25% failures stay under a 30% threshold");
        assert_eq!(outcome.samples.len(), 40);
    }

    #[tokio::test]
    async fn records_per_request_latency() {
        let mut api = StubApi::new(|_| Ok(202));
        api.delay = Duration::from_millis(10);
        let api = Arc::new(api);
        let profile = Profile::new(20, 0, 1, 10).unwrap();

        let outcome = run_load(&api, run_id(), "benchmark", &profile, 0.9, 1)
            .await
            .unwrap();

        assert_eq!(outcome.accepted(), 20);
        assert!(
            outcome
                .samples
                .iter()
                .all(|s| s.outcome.latency_us >= 8_000),
            "a 10ms stub yields latencies near 10,000us"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn every_publish_carries_a_unique_trace_id_and_idempotency_key() {
        let api = Arc::new(StubApi::new(|_| Ok(202)));
        let profile = Profile::new(4, 0, 1, 10).unwrap();

        run_load(&api, run_id(), "benchmark", &profile, 0.9, 1)
            .await
            .unwrap();

        let bodies = api.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 4);
        let mut keys: Vec<&str> = bodies
            .iter()
            .map(|b| b["idempotencyKey"].as_str().unwrap())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 4, "idempotency keys are unique per publish");
        for body in bodies.iter() {
            assert_eq!(body["domain"], "benchmark");
            assert_eq!(body["eventName"], crate::platform::BENCH_EVENT);
            assert_eq!(
                body["data"]["traceId"].as_str().unwrap(),
                body["idempotencyKey"].as_str().unwrap(),
                "trace id doubles as the idempotency key"
            );
            assert!(body["data"]["sentAtMs"].is_i64());
        }
    }
}
