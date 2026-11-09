//! Report math: percentiles, the status mix, and the recorded issue #6
//! baselines every run is compared against (design decision 4).

/// The issue #6 measurements (1,000 msg/s, warm-up excluded): the pre-change
/// platform path and the native EventBridge floor, in milliseconds.
pub const ISSUE6: Issue6Baselines = Issue6Baselines {
    platform_median_ms: 164.7,
    platform_p99_ms: 295.7,
    native_median_ms: 74.7,
    native_p99_ms: 142.6,
    throttled_requests: 294,
};

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Issue6Baselines {
    pub platform_median_ms: f64,
    pub platform_p99_ms: f64,
    pub native_median_ms: f64,
    pub native_p99_ms: f64,
    pub throttled_requests: u32,
}

/// Nearest-rank percentiles over microsecond latencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Percentiles {
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

/// The publish status mix for a window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct StatusMix {
    pub accepted: u64,
    pub throttled: u64,
    pub other: u64,
}

/// A measured value placed next to its issue #6 baseline.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Comparison {
    pub measured_ms: f64,
    pub baseline_ms: f64,
    pub delta_ms: f64,
}

/// Nearest-rank percentiles; `None` for an empty window so a failed run
/// reports "no samples" instead of zeros.
pub fn percentiles(latencies_us: &[u64]) -> Option<Percentiles> {
    if latencies_us.is_empty() {
        return None;
    }
    let mut sorted = latencies_us.to_vec();
    sorted.sort_unstable();
    let rank = |p: u64| {
        let n = sorted.len() as u64;
        let idx = (p * n).div_ceil(100).max(1) - 1;
        sorted[idx as usize]
    };
    Some(Percentiles {
        p50_us: rank(50),
        p95_us: rank(95),
        p99_us: rank(99),
        max_us: sorted[sorted.len() - 1],
    })
}

/// 202 is accepted, 429 is throttled, everything else is other; the report
/// shows throttles explicitly rather than hiding them in latency statistics.
pub fn status_mix(statuses: impl Iterator<Item = u16>) -> StatusMix {
    let mut mix = StatusMix::default();
    for status in statuses {
        match status {
            202 => mix.accepted += 1,
            429 => mix.throttled += 1,
            _ => mix.other += 1,
        }
    }
    mix
}

/// Places a microsecond measurement next to its millisecond baseline.
pub fn compare(measured_us: u64, baseline_ms: f64) -> Comparison {
    let measured_ms = measured_us as f64 / 1_000.0;
    Comparison {
        measured_ms,
        baseline_ms,
        delta_ms: measured_ms - baseline_ms,
    }
}

/// The delivery accounting verdict: every accepted publish must surface as
/// an arrival or a DLQ entry. A mismatch is reported with its size and
/// sign, never hidden (design decision 4). Arrivals are run-scoped by trace
/// id; the DLQ leg is the run-scoped delta of a platform-wide approximate
/// counter (the queue is shared, so the absolute depth means nothing to a
/// single run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Reconciliation {
    pub accepted: u64,
    pub arrivals: u64,
    pub dlq_depth: u64,
    pub accounted: u64,
    /// Accepted minus accounted: positive means events vanished, negative
    /// means duplicates arrived.
    pub unaccounted: i64,
    pub matched: bool,
}

pub fn reconcile(accepted: u64, arrivals: u64, dlq_depth: u64) -> Reconciliation {
    let accounted = arrivals + dlq_depth;
    let unaccounted = accepted as i64 - accounted as i64;
    Reconciliation {
        accepted,
        arrivals,
        dlq_depth,
        accounted,
        unaccounted,
        matched: unaccounted == 0,
    }
}

/// The profile the run actually used, including whether the load phase
/// aborted early.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct EffectiveProfile {
    pub rate_per_sec: u32,
    pub warmup_secs: u32,
    pub measure_secs: u32,
    pub max_in_flight: usize,
    pub aborted: bool,
}

/// The client-observed publish surface: latencies over accepted publishes
/// (rejections are fast by design and belong to the status mix, not the
/// latency distribution, which mirrors how issue #6 measured).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PublishSurface {
    pub samples_measured: usize,
    pub latencies: Option<Percentiles>,
    pub status_mix: StatusMix,
}

/// The native floor surface: latencies over delivered puts.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FloorSurface {
    pub samples_measured: usize,
    pub delivered: usize,
    pub failed: usize,
    pub latencies: Option<Percentiles>,
}

/// Each measured surface against its issue #6 baseline.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Comparisons {
    pub publish_median_vs_issue6: Option<Comparison>,
    pub publish_p99_vs_issue6: Option<Comparison>,
    pub floor_median_vs_issue6: Option<Comparison>,
    pub floor_p99_vs_issue6: Option<Comparison>,
}

/// The complete run report: four surfaces (publish, floor, platform stage
/// metrics, arrivals) plus reconciliation, baselines, and the effective
/// profile.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Report {
    pub run_id: String,
    pub domain: String,
    pub profile: EffectiveProfile,
    pub publish: PublishSurface,
    pub floor: FloorSurface,
    pub stages: Vec<crate::collect::StageStats>,
    pub arrivals: crate::collect::ArrivalStats,
    pub reconciliation: Reconciliation,
    pub baselines: Issue6Baselines,
    pub comparisons: Comparisons,
}

/// Assembles the final report from the three run phases and the
/// collection. Empty surfaces keep `None` latencies rather than zeros.
pub fn assemble(
    run_id: &str,
    domain: &str,
    profile: &crate::pacer::Profile,
    load: &crate::load::LoadOutcome,
    floor: &crate::floor::FloorOutcome,
    collection: &crate::collect::Collection,
    dlq_baseline: u64,
) -> Report {
    let publish_latencies = load.accepted_latencies_us();
    let floor_latencies = floor.delivered_latencies_us();
    let publish_p = percentiles(&publish_latencies);
    let floor_p = percentiles(&floor_latencies);
    Report {
        run_id: run_id.to_string(),
        domain: domain.to_string(),
        profile: EffectiveProfile {
            rate_per_sec: profile.rate_per_sec,
            warmup_secs: profile.warmup_secs,
            measure_secs: profile.measure_secs,
            max_in_flight: profile.max_in_flight,
            aborted: load.aborted,
        },
        publish: PublishSurface {
            samples_measured: crate::pacer::measured_samples(&load.samples).len(),
            latencies: publish_p,
            status_mix: load.status_mix(),
        },
        floor: FloorSurface {
            samples_measured: crate::pacer::measured_samples(&floor.samples).len(),
            delivered: floor_latencies.len(),
            failed: floor.failed(),
            latencies: floor_p,
        },
        stages: collection.stages.clone(),
        arrivals: collection.arrivals.clone(),
        reconciliation: reconcile(
            load.accepted_total() as u64,
            collection.arrivals.arrivals,
            collection.dlq_depth.saturating_sub(dlq_baseline),
        ),
        baselines: ISSUE6,
        comparisons: Comparisons {
            publish_median_vs_issue6: publish_p
                .map(|p| compare(p.p50_us, ISSUE6.platform_median_ms)),
            publish_p99_vs_issue6: publish_p.map(|p| compare(p.p99_us, ISSUE6.platform_p99_ms)),
            floor_median_vs_issue6: floor_p.map(|p| compare(p.p50_us, ISSUE6.native_median_ms)),
            floor_p99_vs_issue6: floor_p.map(|p| compare(p.p99_us, ISSUE6.native_p99_ms)),
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank_over_sorted_copies() {
        let mut latencies: Vec<u64> = (1..=100u64).rev().collect();
        latencies.push(50);
        let p = percentiles(&latencies).unwrap();
        assert_eq!(p.p50_us, 50);
        assert_eq!(p.p95_us, 95);
        assert_eq!(p.p99_us, 99);
        assert_eq!(p.max_us, 100);
    }

    #[test]
    fn percentiles_of_a_single_sample_are_that_sample() {
        let p = percentiles(&[42]).unwrap();
        assert_eq!(
            p,
            Percentiles {
                p50_us: 42,
                p95_us: 42,
                p99_us: 42,
                max_us: 42
            }
        );
    }

    #[test]
    fn percentiles_of_no_samples_are_none() {
        assert_eq!(percentiles(&[]), None);
    }

    #[test]
    fn the_status_mix_counts_accepted_throttled_and_other() {
        let mix = status_mix([202, 202, 429, 500, 400, 202].into_iter());
        assert_eq!(
            mix,
            StatusMix {
                accepted: 3,
                throttled: 1,
                other: 2
            }
        );
    }

    #[test]
    fn a_comparison_places_the_measurement_next_to_the_baseline() {
        let c = compare(100_000, ISSUE6.platform_median_ms);
        assert_eq!(c.measured_ms, 100.0);
        assert_eq!(c.baseline_ms, 164.7);
        assert!((c.delta_ms - (-64.7)).abs() < 1e-9);
    }
    #[test]
    fn reconcile_balances_when_arrivals_plus_dlq_cover_every_acceptance() {
        let r = reconcile(100, 97, 3);

        assert!(r.matched);
        assert_eq!(r.accounted, 100);
        assert_eq!(r.unaccounted, 0);
    }

    #[test]
    fn reconcile_reports_the_gap_instead_of_hiding_it() {
        let r = reconcile(100, 96, 1);

        assert!(!r.matched);
        assert_eq!(r.unaccounted, 3, "three accepted events never surfaced");
    }

    #[test]
    fn reconcile_treats_extra_arrivals_as_a_mismatch_too() {
        let r = reconcile(100, 101, 0);

        assert!(!r.matched);
        assert_eq!(
            r.unaccounted, -1,
            "duplicate deliveries show as a negative gap"
        );
    }

    #[test]
    fn the_report_json_carries_all_surfaces_baselines_profile_and_the_reconcile_verdict() {
        use crate::collect::{ArrivalStats, Collection, StageStats};
        use crate::floor::{FloorOutcome, FloorSample};
        use crate::load::{LoadOutcome, PublishSample};
        use crate::pacer::{Phase, Profile, Sample};

        let load = LoadOutcome {
            samples: vec![
                Sample {
                    tick: 0,
                    phase: Phase::Warmup,
                    outcome: PublishSample {
                        trace_id: "run-x-warm".into(),
                        status: 202,
                        latency_us: 10_000,
                    },
                },
                Sample {
                    tick: 1,
                    phase: Phase::Measured,
                    outcome: PublishSample {
                        trace_id: "run-x-1".into(),
                        status: 202,
                        latency_us: 100_000,
                    },
                },
                Sample {
                    tick: 2,
                    phase: Phase::Measured,
                    outcome: PublishSample {
                        trace_id: "run-x-2".into(),
                        status: 202,
                        latency_us: 200_000,
                    },
                },
                Sample {
                    tick: 3,
                    phase: Phase::Measured,
                    outcome: PublishSample {
                        trace_id: "run-x-3".into(),
                        status: 429,
                        latency_us: 50_000,
                    },
                },
            ],
            aborted: false,
        };
        let floor = FloorOutcome {
            samples: vec![
                Sample {
                    tick: 0,
                    phase: Phase::Measured,
                    outcome: FloorSample {
                        trace_id: "run-x-floor-0".into(),
                        error_code: None,
                        latency_us: 40_000,
                    },
                },
                Sample {
                    tick: 1,
                    phase: Phase::Measured,
                    outcome: FloorSample {
                        trace_id: "run-x-floor-1".into(),
                        error_code: Some("ThrottlingException".into()),
                        latency_us: 60_000,
                    },
                },
            ],
        };
        let collection = Collection {
            stages: vec![
                StageStats {
                    metric: "PublishAdmissionLatency".into(),
                    p50_ms: Some(1.0),
                    p95_ms: Some(2.0),
                    p99_ms: Some(3.0),
                },
                StageStats {
                    metric: "PublishQueueWait".into(),
                    p50_ms: Some(4.0),
                    p95_ms: Some(5.0),
                    p99_ms: Some(6.0),
                },
                StageStats {
                    metric: "PublishEnqueueToDelivery".into(),
                    p50_ms: None,
                    p95_ms: None,
                    p99_ms: None,
                },
            ],
            dlq_depth: 0,
            arrivals: ArrivalStats {
                arrivals: 3,
                p50_ms: Some(150.0),
                p95_ms: Some(180.0),
                p99_ms: Some(210.0),
            },
        };
        let profile = Profile::new(2, 1, 1, 4).unwrap();

        let report = assemble(
            "run-x",
            "benchmark",
            &profile,
            &load,
            &floor,
            &collection,
            0,
        );
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(json["run_id"], "run-x");
        assert_eq!(json["domain"], "benchmark");
        assert_eq!(json["profile"]["rate_per_sec"], 2);
        assert_eq!(json["profile"]["measure_secs"], 1);
        assert_eq!(json["profile"]["aborted"], false);

        assert_eq!(json["publish"]["status_mix"]["accepted"], 2);
        assert_eq!(json["publish"]["status_mix"]["throttled"], 1);
        assert_eq!(json["publish"]["latencies"]["p50_us"], 100_000);
        assert_eq!(json["publish"]["samples_measured"], 3);

        assert_eq!(json["floor"]["failed"], 1);
        assert_eq!(json["floor"]["latencies"]["p50_us"], 40_000);

        assert_eq!(json["stages"].as_array().unwrap().len(), 3);
        assert_eq!(json["stages"][0]["metric"], "PublishAdmissionLatency");
        assert_eq!(json["stages"][2]["p50_ms"], serde_json::Value::Null);

        assert_eq!(json["arrivals"]["arrivals"], 3);
        assert_eq!(json["arrivals"]["p99_ms"], 210.0);

        assert_eq!(
            json["reconciliation"]["accepted"], 3,
            "warmup publishes are accepted publishes too"
        );
        assert_eq!(json["reconciliation"]["arrivals"], 3);
        assert_eq!(json["reconciliation"]["dlq_depth"], 0);
        assert_eq!(json["reconciliation"]["matched"], true);

        assert_eq!(json["baselines"]["platform_median_ms"], 164.7);
        assert_eq!(json["baselines"]["native_p99_ms"], 142.6);
        assert_eq!(
            json["comparisons"]["publish_median_vs_issue6"]["baseline_ms"],
            164.7
        );
        assert_eq!(
            json["comparisons"]["publish_p99_vs_issue6"]["measured_ms"],
            200.0
        );
        assert_eq!(
            json["comparisons"]["floor_median_vs_issue6"]["baseline_ms"],
            74.7
        );
    }

    #[test]
    fn the_report_tolerates_a_run_with_no_measured_samples() {
        use crate::collect::{ArrivalStats, Collection};
        use crate::floor::FloorOutcome;
        use crate::load::LoadOutcome;
        use crate::pacer::Profile;

        let load = LoadOutcome {
            samples: vec![],
            aborted: true,
        };
        let floor = FloorOutcome { samples: vec![] };
        let collection = Collection {
            stages: vec![],
            dlq_depth: 0,
            arrivals: ArrivalStats {
                arrivals: 0,
                p50_ms: None,
                p95_ms: None,
                p99_ms: None,
            },
        };
        let profile = Profile::new(1, 0, 1, 1).unwrap();

        let report = assemble(
            "run-empty",
            "benchmark",
            &profile,
            &load,
            &floor,
            &collection,
            0,
        );
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(json["publish"]["latencies"], serde_json::Value::Null);
        assert_eq!(json["floor"]["latencies"], serde_json::Value::Null);
        assert_eq!(json["profile"]["aborted"], true);
        assert_eq!(
            json["comparisons"]["publish_median_vs_issue6"],
            serde_json::Value::Null
        );
    }
}
