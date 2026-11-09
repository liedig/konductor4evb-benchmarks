//! Open-loop load pacing: one send per tick at the configured rate, warm-up
//! ticks tagged and excluded from measurement, and a bounded in-flight
//! budget so a slow target surfaces as latency instead of unbounded
//! concurrency (design decision 3).

use std::time::Duration;

/// What one run measures. `rate_per_sec` and the durations reproduce the
/// issue #6 profile by default; `max_in_flight` bounds spawned sends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Profile {
    pub rate_per_sec: u32,
    pub warmup_secs: u32,
    pub measure_secs: u32,
    pub max_in_flight: usize,
}

/// Rejected profile parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidProfile(&'static str);

impl std::fmt::Display for InvalidProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid profile: {}", self.0)
    }
}

impl std::error::Error for InvalidProfile {}

/// Which part of the run a tick belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Warmup,
    Measured,
}

/// One completed send, tagged with its tick and phase.
#[derive(Debug, PartialEq)]
pub struct Sample<T> {
    pub tick: u64,
    pub phase: Phase,
    pub outcome: T,
}

/// The measured samples only, in completion order.
pub fn measured_samples<T>(samples: &[Sample<T>]) -> Vec<&Sample<T>> {
    samples
        .iter()
        .filter(|s| s.phase == Phase::Measured)
        .collect()
}

impl Profile {
    pub fn new(
        rate_per_sec: u32,
        warmup_secs: u32,
        measure_secs: u32,
        max_in_flight: usize,
    ) -> Result<Self, InvalidProfile> {
        if rate_per_sec == 0 {
            return Err(InvalidProfile("rate_per_sec must be positive"));
        }
        // Far past any API Gateway account limit; guards tick_interval's
        // nanosecond division from a zero (panicking) interval.
        if rate_per_sec > 100_000 {
            return Err(InvalidProfile("rate_per_sec must not exceed 100000"));
        }
        if measure_secs == 0 {
            return Err(InvalidProfile("measure_secs must be positive"));
        }
        if max_in_flight == 0 {
            return Err(InvalidProfile("max_in_flight must be positive"));
        }
        Ok(Self {
            rate_per_sec,
            warmup_secs,
            measure_secs,
            max_in_flight,
        })
    }

    fn warmup_ticks(&self) -> u64 {
        u64::from(self.rate_per_sec) * u64::from(self.warmup_secs)
    }

    /// Total scheduled ticks; the run spawns exactly this many sends.
    pub fn total_ticks(&self) -> u64 {
        self.warmup_ticks() + u64::from(self.rate_per_sec) * u64::from(self.measure_secs)
    }

    fn tick_interval(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 / u64::from(self.rate_per_sec))
    }

    fn phase_of(&self, tick: u64) -> Phase {
        if tick < self.warmup_ticks() {
            Phase::Warmup
        } else {
            Phase::Measured
        }
    }
}

/// Runs the schedule, spawning one `send` per tick under a semaphore that
/// bounds in-flight sends. Missed ticks burst rather than shift the
/// schedule, so a slow target shows up as growing latency. Samples come back
/// in completion order; every sample carries its tick and phase.
pub async fn run<T, F, Fut>(profile: &Profile, send: F) -> Vec<Sample<T>>
where
    F: Fn(u64) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    run_until(profile, send, |_| false).await.0
}

/// Like [`run`], but stops spawning new sends once `should_abort` returns
/// true. The predicate receives only the samples completed since its last
/// call (never the full history), so per-call work stays bounded by the
/// in-flight cap and the caller keeps cumulative tallies itself. In-flight
/// sends still complete and are collected; the returned flag records
/// whether the abort fired.
pub async fn run_until<T, F, Fut, A>(
    profile: &Profile,
    send: F,
    should_abort: A,
) -> (Vec<Sample<T>>, bool)
where
    F: Fn(u64) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
    A: Fn(&[Sample<T>]) -> bool + Send + Sync + 'static,
{
    let send = std::sync::Arc::new(send);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(profile.max_in_flight));
    let mut interval = tokio::time::interval(profile.tick_interval());
    let mut set = tokio::task::JoinSet::new();
    let mut samples = Vec::with_capacity(profile.total_ticks() as usize);
    let mut aborted = false;
    let collect = |set: &mut tokio::task::JoinSet<Sample<T>>, samples: &mut Vec<Sample<T>>| {
        while let Some(joined) = set.try_join_next() {
            match joined {
                Ok(sample) => samples.push(sample),
                Err(e) => {
                    tracing::error!(error = %e, "a send task failed to complete; its sample is lost")
                }
            }
        }
    };
    'ticks: for tick in 0..profile.total_ticks() {
        interval.tick().await;
        let Ok(permit) = semaphore.clone().acquire_owned().await else {
            break;
        };
        let phase = profile.phase_of(tick);
        let send = send.clone();
        set.spawn(async move {
            let outcome = send(tick).await;
            drop(permit);
            Sample {
                tick,
                phase,
                outcome,
            }
        });
        let before = samples.len();
        collect(&mut set, &mut samples);
        if should_abort(&samples[before..]) {
            aborted = true;
            break 'ticks;
        }
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(sample) => samples.push(sample),
            Err(e) => {
                tracing::error!(error = %e, "a send task failed to complete; its sample is lost")
            }
        }
    }
    (samples, aborted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn profile(rate: u32, warmup: u32, measure: u32, cap: usize) -> Profile {
        Profile::new(rate, warmup, measure, cap).unwrap()
    }

    #[test]
    fn a_zero_rate_is_rejected() {
        assert_eq!(
            Profile::new(0, 1, 1, 1),
            Err(InvalidProfile("rate_per_sec must be positive"))
        );
    }

    #[test]
    fn a_zero_measurement_window_is_rejected() {
        assert!(Profile::new(100, 1, 0, 1).is_err());
    }

    #[test]
    fn a_zero_in_flight_cap_is_rejected() {
        assert!(Profile::new(100, 1, 1, 0).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn ticks_at_the_configured_rate_for_the_configured_duration() {
        let p = profile(1000, 0, 3, 10);
        let samples = run(&p, |_| async {}).await;
        assert_eq!(samples.len(), 3000, "three measured seconds at 1,000/s");
        assert!(samples.iter().all(|s| s.phase == Phase::Measured));
    }

    #[tokio::test(start_paused = true)]
    async fn warmup_ticks_are_tagged_and_excluded_from_measurement() {
        let p = profile(100, 2, 1, 10);
        let samples = run(&p, |_| async {}).await;
        assert_eq!(samples.len(), 300);
        assert_eq!(
            samples.iter().filter(|s| s.phase == Phase::Warmup).count(),
            200
        );
        let measured = measured_samples(&samples);
        assert_eq!(measured.len(), 100);
        assert!(measured.iter().all(|s| s.tick >= 200));
    }

    #[tokio::test(start_paused = true)]
    async fn the_in_flight_cap_is_respected() {
        let p = profile(1000, 0, 1, 5);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let samples = run(&p, {
            let in_flight = in_flight.clone();
            let max_seen = max_seen.clone();
            move |_| {
                let in_flight = in_flight.clone();
                let max_seen = max_seen.clone();
                async move {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                }
            }
        })
        .await;
        assert_eq!(samples.len(), 1000, "the run still completes every tick");
        assert_eq!(max_seen.load(Ordering::SeqCst), 5);
    }
    #[test]
    fn an_absurd_rate_is_rejected_before_the_tick_interval_can_panic() {
        assert!(Profile::new(2_000_000_000, 0, 1, 1).is_err());
        assert!(
            Profile::new(100_000, 0, 1, 1).is_ok(),
            "the cap still allows stress profiles"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_abort_predicate_sees_only_new_samples() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let p = profile(1000, 0, 2, 10);
        let calls = Arc::new(AtomicUsize::new(0));
        let max_batch = Arc::new(AtomicUsize::new(0));
        let (samples, aborted) = run_until(&p, |_| async {}, {
            let calls = calls.clone();
            let max_batch = max_batch.clone();
            move |new_samples: &[Sample<()>]| {
                calls.fetch_add(1, Ordering::SeqCst);
                max_batch.fetch_max(new_samples.len(), Ordering::SeqCst);
                false
            }
        })
        .await;
        assert!(!aborted);
        assert_eq!(samples.len(), 2000);
        assert!(
            calls.load(Ordering::SeqCst) >= 1000,
            "the predicate is consulted throughout"
        );
        assert!(
            max_batch.load(Ordering::SeqCst) <= 10,
            "per-call input stays bounded by the in-flight cap, got {}",
            max_batch.load(Ordering::SeqCst)
        );
    }
}
