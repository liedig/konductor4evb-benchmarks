//! Phased benchmark run: setup → load → floor → settle → collect →
//! report, with cleanup (arrival probe, floor bus, seeded binding) running
//! on every reachable path. Each phase has its own module and tests; this
//! module owns configuration and the cleanup-always orchestration.

use std::future::Future;

use anyhow::{Context, Result};

use crate::collect::PUBLISH_DLQ;
use crate::pacer::Profile;

/// Everything a run needs, from stack-provided env with payload overrides.
#[derive(Debug, Clone, PartialEq)]
pub struct RunConfig {
    pub domain: String,
    pub api_endpoint: String,
    pub table_name: String,
    /// Domains table the ACTIVE check reads directly (the platform's
    /// describe endpoint requires an owner binding the harness must not
    /// need; it seeds only a publisher binding).
    pub domains_table: String,
    pub account_id: String,
    pub region: String,
    /// `{org}-{env}-{domain}-event-bus`, the platform-side domain bus.
    pub bus_name: String,
    pub dlq_name: String,
    pub profile: Profile,
    pub floor_profile: Profile,
    pub settle_secs: u32,
    pub abort_error_rate: f64,
    pub abort_min_samples: usize,
}

/// Runs `cleanup` no matter how `result` came out, then reports: the phase
/// error first (with a cleanup failure attached when both failed), a
/// cleanup failure on its own when the phases succeeded, else the value.
pub async fn finish<T>(result: Result<T>, cleanup: impl Future<Output = Result<()>>) -> Result<T> {
    match (result, cleanup.await) {
        (Ok(v), Ok(())) => Ok(v),
        (Err(e), Ok(())) => Err(e),
        (Ok(_), Err(c)) => Err(c.context("cleanup failed after a successful run")),
        (Err(e), Err(c)) => Err(e.context(format!("cleanup also failed: {c:#}"))),
    }
}

impl RunConfig {
    /// Reads stack-provided env and applies payload overrides. The defaults
    /// reproduce the issue #6 profile: 1,000 msg/s, 30s warm-up, 420s
    /// measured (~420k publishes), 200 in flight, then a short floor probe
    /// and a settle window before collection.
    pub fn resolve(
        event: &serde_json::Value,
        env: &impl Fn(&str) -> Option<String>,
    ) -> Result<RunConfig> {
        let required = |name: &str| {
            env(name).with_context(|| {
                format!(
                    "missing required environment variable {name} (the benchmark stack sets it)"
                )
            })
        };
        let domain = required("BENCH_DOMAIN")?;
        let api_endpoint = required("BENCH_API_ENDPOINT")?;
        let table_name = required("BENCH_BINDINGS_TABLE")?;
        let domains_table =
            env("BENCH_DOMAINS_TABLE").unwrap_or_else(|| "konductor-domains".to_string());
        let account_id = required("BENCH_ACCOUNT_ID")?;
        let org = required("BENCH_ORG")?;
        let env_name = required("BENCH_ENV")?;
        let region = required("AWS_REGION")?;

        let override_u32 = |key: &str| -> Result<Option<u32>> {
            event
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .map(|v| u32::try_from(v).with_context(|| format!("{key} exceeds the u32 range")))
                .transpose()
        };
        let override_usize = |key: &str| override_u32(key).map(|o| o.map(|v| v as usize));
        let profile = Profile::new(
            override_u32("rate_per_sec")?.unwrap_or(1000),
            override_u32("warmup_secs")?.unwrap_or(30),
            override_u32("measure_secs")?.unwrap_or(420),
            override_usize("max_in_flight")?.unwrap_or(200),
        )
        .context("invalid load profile override")?;
        let floor_profile = Profile::new(
            override_u32("floor_rate_per_sec")?.unwrap_or(100),
            override_u32("floor_warmup_secs")?.unwrap_or(5),
            override_u32("floor_measure_secs")?.unwrap_or(30),
            override_usize("floor_max_in_flight")?.unwrap_or(50),
        )
        .context("invalid floor profile override")?;
        let abort_error_rate = event
            .get("abort_error_rate")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.05);
        if abort_error_rate <= 0.0 || abort_error_rate >= 1.0 {
            anyhow::bail!(
                "abort_error_rate must be between 0 and 1 exclusive, got {abort_error_rate}"
            );
        }
        // Zero would make the load phase's abort guard unconditional
        // (`measured >= 0` is always true), so one early failure could kill
        // the run; the guard exists to demand a minimum of evidence first.
        let abort_min_samples = override_usize("abort_min_samples")?.unwrap_or(100);
        if abort_min_samples == 0 {
            anyhow::bail!("abort_min_samples must be positive, got 0");
        }

        Ok(RunConfig {
            bus_name: format!("{org}-{env_name}-{domain}-event-bus"),
            dlq_name: PUBLISH_DLQ.to_string(),
            settle_secs: override_u32("settle_secs")?.unwrap_or(120),
            abort_error_rate,
            abort_min_samples,
            domain,
            api_endpoint,
            table_name,
            domains_table,
            account_id,
            region,
            profile,
            floor_profile,
        })
    }
}

/// The AWS clients and the platform API signer, built once per warm
/// execution environment in `main`.
pub struct Clients {
    pub ddb: aws_sdk_dynamodb::Client,
    /// Rule and bus management (standard retries are fine for fixtures).
    pub events: aws_sdk_eventbridge::Client,
    /// The floor probe's client: retries disabled.
    pub floor_events: aws_sdk_eventbridge::Client,
    pub cw: aws_sdk_cloudwatch::Client,
    pub logs: aws_sdk_cloudwatchlogs::Client,
    pub sqs: aws_sdk_sqs::Client,
    pub signer: crate::sigv4::ExecuteApiSigner,
}

pub async fn handle(
    clients: &Clients,
    event: lambda_runtime::LambdaEvent<serde_json::Value>,
) -> Result<serde_json::Value, lambda_runtime::Error> {
    let cfg = RunConfig::resolve(&event.payload, &|key| std::env::var(key).ok())?;
    let run_id = format!("bench-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    let platform = std::sync::Arc::new(crate::platform::HttpPlatformApi::new(
        clients.signer.clone(),
        cfg.api_endpoint.clone(),
    ));
    tracing::info!(%run_id, domain = %cfg.domain, profile = ?cfg.profile, "benchmark run starting");

    // The ACTIVE check reads the domains table directly (the describe
    // endpoint requires an owner binding) and fails before any fixture
    // exists; the schema ensure then runs with the publisher binding
    // already seeded, because POST /schemas authorizes on it.
    let status = crate::setup::domain_status(&clients.ddb, &cfg.domains_table, &cfg.domain).await?;

    let binding =
        crate::fixtures::seed_binding(&clients.ddb, &cfg.table_name, &cfg.account_id, &cfg.domain)
            .await?;

    crate::setup::ensure_ready(platform.as_ref(), &cfg.domain, status).await?;

    let log_group_arn = format!(
        "arn:aws:logs:{}:{}:log-group:{}",
        cfg.region,
        cfg.account_id,
        crate::arrival::ARRIVAL_LOG_GROUP
    );

    let phases = async {
        crate::arrival::create_arrival_probe(
            &clients.events,
            &clients.logs,
            &cfg.bus_name,
            &log_group_arn,
        )
        .await?;
        crate::floor::create_floor_bus(&clients.events).await?;

        let dlq_baseline = crate::collect::dlq_depth(&clients.sqs, &cfg.dlq_name).await?;

        let measure_start = chrono::Utc::now().timestamp_millis();
        let load = crate::load::run_load(
            &platform,
            &run_id,
            &cfg.domain,
            &cfg.profile,
            cfg.abort_error_rate,
            cfg.abort_min_samples,
        )
        .await?;
        tracing::info!(
            %run_id,
            accepted = load.accepted(),
            throttled = load.throttled(),
            failed = load.failed(),
            "load phase complete"
        );
        if load.aborted {
            tracing::warn!(%run_id, "load phase aborted early on the failure threshold");
        }

        let floor = crate::floor::run_floor(
            clients.floor_events.clone(),
            crate::floor::FLOOR_BUS,
            &run_id,
            &cfg.floor_profile,
        )
        .await?;

        tracing::info!(settle_secs = cfg.settle_secs, "settling before collection");
        tokio::time::sleep(std::time::Duration::from_secs(u64::from(cfg.settle_secs))).await;
        let window = crate::collect::MeasureWindow::new(
            measure_start,
            chrono::Utc::now().timestamp_millis(),
        )?;
        let collection = crate::collect::collect(
            &clients.cw,
            &clients.logs,
            &clients.sqs,
            &window,
            &run_id,
            &cfg.dlq_name,
        )
        .await?;

        let report = crate::report::assemble(
            &run_id,
            &cfg.domain,
            &cfg.profile,
            &load,
            &floor,
            &collection,
            dlq_baseline,
        );
        if !report.reconciliation.matched {
            tracing::warn!(
                %run_id,
                unaccounted = report.reconciliation.unaccounted,
                "reconciliation mismatch: accepted events never surfaced (or duplicated)"
            );
        }
        Ok(report)
    };

    let cleanup = async {
        let mut failures = Vec::new();
        for (name, result) in [
            (
                "arrival probe",
                crate::arrival::remove_arrival_probe(&clients.events, &clients.logs, &cfg.bus_name)
                    .await,
            ),
            (
                "floor bus",
                crate::floor::remove_floor_bus(&clients.events).await,
            ),
            (
                "publisher binding",
                crate::fixtures::remove_binding(&clients.ddb, &cfg.table_name, &binding).await,
            ),
        ] {
            if let Err(e) = result {
                tracing::warn!(error = %format!("{e:#}"), "cleanup step failed");
                failures.push(format!("{name}: {e:#}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(failures.join("; ")))
        }
    };

    let report = finish(phases.await, cleanup).await?;
    tracing::info!(%run_id, "benchmark run complete");
    Ok(serde_json::to_value(&report)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    fn full_env() -> Vec<(&'static str, &'static str)> {
        vec![
            ("BENCH_DOMAIN", "benchmark"),
            (
                "BENCH_API_ENDPOINT",
                "https://abc123.execute-api.us-east-1.amazonaws.com/dev",
            ),
            ("BENCH_BINDINGS_TABLE", "konductor-authorization-bindings"),
            ("BENCH_ACCOUNT_ID", "111122223333"),
            ("BENCH_ORG", "acme"),
            ("BENCH_ENV", "dev"),
            ("AWS_REGION", "us-east-1"),
        ]
    }

    #[test]
    fn resolve_defaults_to_the_issue6_profile() {
        let cfg = RunConfig::resolve(&json!({}), &env(&full_env())).unwrap();

        assert_eq!(cfg.domain, "benchmark");
        assert_eq!(cfg.bus_name, "acme-dev-benchmark-event-bus");
        assert_eq!(cfg.dlq_name, PUBLISH_DLQ);
        assert_eq!(cfg.profile, Profile::new(1000, 30, 420, 200).unwrap());
        assert_eq!(cfg.floor_profile, Profile::new(100, 5, 30, 50).unwrap());
        assert_eq!(cfg.settle_secs, 120);
        assert!((cfg.abort_error_rate - 0.05).abs() < f64::EPSILON);
        assert_eq!(cfg.abort_min_samples, 100);
    }

    #[test]
    fn resolve_honors_payload_overrides() {
        let event = json!({
            "rate_per_sec": 50,
            "warmup_secs": 5,
            "measure_secs": 60,
            "max_in_flight": 25,
            "settle_secs": 10,
            "floor_rate_per_sec": 20,
            "floor_warmup_secs": 2,
            "floor_measure_secs": 15
        });

        let cfg = RunConfig::resolve(&event, &env(&full_env())).unwrap();

        assert_eq!(cfg.profile, Profile::new(50, 5, 60, 25).unwrap());
        assert_eq!(cfg.floor_profile, Profile::new(20, 2, 15, 50).unwrap());
        assert_eq!(cfg.settle_secs, 10);
    }

    #[test]
    fn resolve_fails_fast_without_the_domain() {
        let vars: Vec<_> = full_env()
            .into_iter()
            .filter(|(k, _)| *k != "BENCH_DOMAIN")
            .collect();

        let err = RunConfig::resolve(&json!({}), &env(&vars)).unwrap_err();

        assert!(format!("{err:#}").contains("BENCH_DOMAIN"));
    }

    #[test]
    fn resolve_rejects_an_invalid_overridden_profile() {
        let err = RunConfig::resolve(&json!({"rate_per_sec": 0}), &env(&full_env())).unwrap_err();

        assert!(format!("{err:#}").contains("rate_per_sec"));
    }

    #[test]
    fn resolve_rejects_zero_abort_min_samples() {
        let err =
            RunConfig::resolve(&json!({"abort_min_samples": 0}), &env(&full_env())).unwrap_err();

        assert!(format!("{err:#}").contains("abort_min_samples"));
    }

    #[test]
    fn resolve_honors_a_positive_abort_min_samples() {
        let cfg = RunConfig::resolve(&json!({"abort_min_samples": 50}), &env(&full_env())).unwrap();

        assert_eq!(cfg.abort_min_samples, 50);
    }

    #[tokio::test]
    async fn finish_returns_the_value_when_cleanup_succeeds() {
        let value: Result<u32> = Ok(7);
        let out = finish(value, async { Ok(()) }).await.unwrap();
        assert_eq!(out, 7);
    }

    #[tokio::test]
    async fn finish_still_runs_cleanup_when_the_phases_failed() {
        let cleaned = Arc::new(AtomicBool::new(false));
        let flag = cleaned.clone();
        let cleanup = async move {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        };

        let err = finish::<()>(Err(anyhow::anyhow!("load blew up")), cleanup)
            .await
            .unwrap_err();

        assert!(
            cleaned.load(Ordering::SeqCst),
            "cleanup ran despite the failure"
        );
        assert!(format!("{err:#}").contains("load blew up"));
    }

    #[tokio::test]
    async fn finish_reports_a_cleanup_failure_after_successful_phases() {
        let err = finish::<()>(Ok(()), async { Err(anyhow::anyhow!("binding stuck")) })
            .await
            .unwrap_err();

        let msg = format!("{err:#}");
        assert!(msg.contains("cleanup failed"));
        assert!(msg.contains("binding stuck"));
    }

    #[tokio::test]
    async fn finish_reports_both_errors_when_everything_fails() {
        let err = finish::<()>(Err(anyhow::anyhow!("load blew up")), async {
            Err(anyhow::anyhow!("binding stuck"))
        })
        .await
        .unwrap_err();

        let msg = format!("{err:#}");
        assert!(msg.contains("load blew up"));
        assert!(
            msg.contains("binding stuck"),
            "the cleanup error is not hidden: {msg}"
        );
    }
    #[test]
    fn resolve_rejects_an_out_of_range_override_instead_of_wrapping() {
        let err = RunConfig::resolve(
            &json!({"rate_per_sec": 4_294_967_296_u64}),
            &env(&full_env()),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("rate_per_sec"));
    }

    #[test]
    fn resolve_rejects_an_abort_threshold_outside_unit_interval() {
        for rate in [1.5, 0.0, -0.1] {
            let err = RunConfig::resolve(&json!({"abort_error_rate": rate}), &env(&full_env()))
                .unwrap_err();
            assert!(
                format!("{err:#}").contains("abort_error_rate"),
                "rate {rate} rejected"
            );
        }
    }
}
