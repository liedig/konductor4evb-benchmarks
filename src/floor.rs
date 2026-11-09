//! The native floor probe: the same paced schedule driven straight at
//! Amazon EventBridge `PutEvents` on a function-owned throwaway bus, SDK
//! retries disabled, so the report can hold the platform's publish path up
//! against the native floor (the issue #6 comparison). The throwaway bus
//! keeps probe traffic off the domain's bus, so domain subscribers never
//! see benchmark floor events (design decision 4). Floor trace ids are
//! `{run_id}-floor-{tick}` and the probe runs after the publish window's
//! settle period, so it never pollutes the publish reconciliation.

use std::time::Instant;

use anyhow::{Context, Result};

use crate::pacer::{self, Profile, Sample};
use crate::platform::BENCH_EVENT;

/// The event source for floor-probe entries. Not a platform source; the
/// arrival rule matches on detail-type only.
pub const FLOOR_SOURCE: &str = "konductor-benchmarks.floor";

/// One native put attempt.
#[derive(Debug, PartialEq)]
pub struct FloorSample {
    pub trace_id: String,
    /// `None` when EventBridge accepted the entry; otherwise the service
    /// error code (`ThrottlingException`, ...) or `SdkFailure` when the
    /// call itself failed.
    pub error_code: Option<String>,
    pub latency_us: u64,
}

impl FloorSample {
    pub fn delivered(&self) -> bool {
        self.error_code.is_none()
    }
}

/// The finished floor probe, shaped to feed `report::percentiles` exactly
/// like the publish probe does.
#[derive(Debug)]
pub struct FloorOutcome {
    pub samples: Vec<Sample<FloorSample>>,
}

impl FloorOutcome {
    /// Entries EventBridge rejected or the SDK failed to deliver.
    pub fn failed(&self) -> usize {
        pacer::measured_samples(&self.samples)
            .iter()
            .filter(|s| !s.outcome.delivered())
            .count()
    }

    /// Latencies of delivered puts in the measured window, mirroring the
    /// publish surface's accepted-only rule.
    pub fn delivered_latencies_us(&self) -> Vec<u64> {
        pacer::measured_samples(&self.samples)
            .iter()
            .filter(|s| s.outcome.delivered())
            .map(|s| s.outcome.latency_us)
            .collect()
    }
}

/// Mirrors `LoadOutcome`: a floor probe that never ran reports empty.
impl Default for FloorOutcome {
    fn default() -> Self {
        Self {
            samples: Vec::new(),
        }
    }
}
/// An EventBridge client with retries disabled: the floor must measure
/// native `PutEvents`, including its throttles, not the SDK's patience.
pub fn floor_client(config: &aws_config::SdkConfig) -> aws_sdk_eventbridge::Client {
    let retry = aws_sdk_eventbridge::config::retry::RetryConfig::standard().with_max_attempts(1);
    aws_sdk_eventbridge::Client::from_conf(
        aws_sdk_eventbridge::config::Builder::from(config)
            .retry_config(retry)
            .build(),
    )
}

/// One entry per tick carrying the same `data{traceId, sentAtMs}` shape as
/// platform-published events, so the arrival log parses both uniformly.
fn floor_entry(
    bus_name: &str,
    trace_id: &str,
    sent_at_ms: i64,
) -> aws_sdk_eventbridge::types::PutEventsRequestEntry {
    aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
        .event_bus_name(bus_name)
        .source(FLOOR_SOURCE)
        .detail_type(BENCH_EVENT)
        .detail(
            serde_json::json!({
                "data": { "traceId": trace_id, "sentAtMs": sent_at_ms }
            })
            .to_string(),
        )
        .trace_header(trace_id)
        .build()
}

/// Drives `profile` straight at `PutEvents`. Entry rejections and SDK
/// failures become failed samples (the floor probe reports them); the probe
/// itself only errors when it cannot run at all.
pub async fn run_floor(
    eb: aws_sdk_eventbridge::Client,
    bus_name: &str,
    run_id: &str,
    profile: &Profile,
) -> Result<FloorOutcome> {
    let bus = bus_name.to_string();
    let run = run_id.to_string();
    let client = eb;
    let samples = pacer::run(profile, move |tick| {
        let bus = bus.clone();
        let run = run.clone();
        let client = client.clone();
        async move {
            let trace_id = format!("{run}-floor-{tick}");
            let entry = floor_entry(&bus, &trace_id, chrono::Utc::now().timestamp_millis());
            let started = Instant::now();
            let error_code = match client.put_events().entries(entry).send().await {
                Ok(output) if output.failed_entry_count() == 0 => None,
                Ok(output) => Some(
                    output
                        .entries()
                        .first()
                        .and_then(|e| e.error_code())
                        .unwrap_or("UnknownEntryFailure")
                        .to_string(),
                ),
                Err(e) => {
                    tracing::debug!(error = %e, %trace_id, "floor put failed at the SDK");
                    Some("SdkFailure".to_string())
                }
            };
            FloorSample {
                trace_id,
                error_code,
                latency_us: started.elapsed().as_micros() as u64,
            }
        }
    })
    .await;
    Ok(FloorOutcome { samples })
}

/// The throwaway bus for the floor probe: fixed name, function-owned,
/// created during setup and deleted during cleanup. Fixed rather than
/// per-run so a leftover from an interrupted run is recognizable and
/// reused.
pub const FLOOR_BUS: &str = "konductor-benchmark-floor";

/// Creates the floor bus; reuses one an interrupted run left behind.
pub async fn create_floor_bus(eb: &aws_sdk_eventbridge::Client) -> Result<()> {
    match eb.create_event_bus().name(FLOOR_BUS).send().await {
        Ok(_) => Ok(()),
        Err(e)
            if matches!(
                e.as_service_error(),
                Some(aws_sdk_eventbridge::operation::create_event_bus::CreateEventBusError::ResourceAlreadyExistsException(_))
            ) =>
        {
            tracing::debug!("floor bus already exists; reusing it");
            Ok(())
        }
        Err(e) => Err(e).context("creating the floor bus"),
    }
}

/// Deletes the floor bus; tolerates one already gone. The SDK models no
/// not-found variant for `DeleteEventBus`, so the unmodeled code is matched
/// on the error metadata.
pub async fn remove_floor_bus(eb: &aws_sdk_eventbridge::Client) -> Result<()> {
    use aws_sdk_eventbridge::error::ProvideErrorMetadata;
    match eb.delete_event_bus().name(FLOOR_BUS).send().await {
        Ok(_) => Ok(()),
        Err(e) if e.code() == Some("ResourceNotFoundException") => {
            tracing::debug!("floor bus already gone");
            Ok(())
        }
        Err(e) => Err(e).context("deleting the floor bus"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_mocks::{mock, mock_client, Rule, RuleMode};

    const BUS: &str = "acme-prod-benchmark-event-bus";

    fn put_ok() -> aws_smithy_mocks::Rule {
        mock!(aws_sdk_eventbridge::Client::put_events)
            .match_requests(|req| {
                req.entries().len() == 1
                    && req.entries()[0].event_bus_name() == Some(BUS)
                    && req.entries()[0].source() == Some(FLOOR_SOURCE)
                    && req.entries()[0].detail_type() == Some(BENCH_EVENT)
            })
            .then_output(|| {
                aws_sdk_eventbridge::operation::put_events::PutEventsOutput::builder()
                    .failed_entry_count(0)
                    .build()
            })
    }

    #[test]
    fn the_floor_client_disables_sdk_retries() {
        let creds = aws_credential_types::Credentials::new("akid", "secret", None, None, "test");
        let sdk_config = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new("us-east-1"))
            .credentials_provider(
                aws_credential_types::provider::SharedCredentialsProvider::new(creds),
            )
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();

        let client = floor_client(&sdk_config);

        assert_eq!(
            client.config().retry_config().map(|r| r.max_attempts()),
            Some(1),
            "the floor measures native EventBridge, not SDK retry patience"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn delivered_entries_yield_percentile_ready_samples_shaped_like_the_publish_probe() {
        let put = put_ok();
        let eb = mock_client!(
            aws_sdk_eventbridge,
            RuleMode::MatchAny,
            std::slice::from_ref(&put)
        );
        let profile = Profile::new(4, 1, 1, 10).unwrap();

        let outcome = run_floor(eb.clone(), BUS, "run", &profile).await.unwrap();

        assert_eq!(outcome.samples.len(), 8, "warm-up plus measured ticks");
        assert_eq!(put.num_calls(), 8);
        assert_eq!(outcome.failed(), 0);
        let latencies = outcome.delivered_latencies_us();
        assert_eq!(latencies.len(), 4, "warm-up excluded from measurement");
        assert!(
            crate::report::percentiles(&latencies).is_some(),
            "the floor feeds the same Percentiles shape as the publish probe"
        );
        let mut ids: Vec<&str> = outcome
            .samples
            .iter()
            .map(|s| s.outcome.trace_id.as_str())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 8, "trace ids are unique per tick");
        assert!(ids.iter().all(|id| id.starts_with("run-floor-")));
    }

    #[tokio::test(start_paused = true)]
    async fn failed_entries_record_the_service_error_code() {
        let put = mock!(aws_sdk_eventbridge::Client::put_events).then_output(|| {
            aws_sdk_eventbridge::operation::put_events::PutEventsOutput::builder()
                .failed_entry_count(1)
                .entries(
                    aws_sdk_eventbridge::types::PutEventsResultEntry::builder()
                        .error_code("ThrottlingException")
                        .error_message("rate exceeded")
                        .build(),
                )
                .build()
        });
        let eb = mock_client!(aws_sdk_eventbridge, RuleMode::MatchAny, &[put]);
        let profile = Profile::new(2, 0, 1, 10).unwrap();

        let outcome = run_floor(eb.clone(), BUS, "run", &profile).await.unwrap();

        assert_eq!(outcome.failed(), 2);
        assert!(outcome
            .samples
            .iter()
            .all(|s| s.outcome.error_code.as_deref() == Some("ThrottlingException")));
    }

    #[tokio::test(start_paused = true)]
    async fn sdk_failures_record_a_sample_instead_of_aborting_the_probe() {
        let put = mock!(aws_sdk_eventbridge::Client::put_events).then_error(|| {
            aws_sdk_eventbridge::operation::put_events::PutEventsError::generic(
                aws_sdk_eventbridge::error::ErrorMetadata::builder()
                    .message("connection reset")
                    .build(),
            )
        });
        let eb = mock_client!(aws_sdk_eventbridge, RuleMode::MatchAny, &[put]);
        let profile = Profile::new(2, 0, 1, 10).unwrap();

        let outcome = run_floor(eb.clone(), BUS, "run", &profile).await.unwrap();

        assert_eq!(outcome.samples.len(), 2);
        assert_eq!(outcome.failed(), 2);
        assert!(outcome
            .samples
            .iter()
            .all(|s| s.outcome.error_code.as_deref() == Some("SdkFailure")));
    }
    fn create_bus_rule() -> Rule {
        mock!(aws_sdk_eventbridge::Client::create_event_bus)
            .match_requests(|req| req.name() == Some(FLOOR_BUS))
            .then_output(|| {
                aws_sdk_eventbridge::operation::create_event_bus::CreateEventBusOutput::builder()
                    .event_bus_arn("arn:bus")
                    .build()
            })
    }

    #[tokio::test]
    async fn the_floor_bus_fixture_creates_then_deletes_a_throwaway_bus() {
        let create = create_bus_rule();
        let delete = mock!(aws_sdk_eventbridge::Client::delete_event_bus)
            .match_requests(|req| req.name() == Some(FLOOR_BUS))
            .then_output(|| {
                aws_sdk_eventbridge::operation::delete_event_bus::DeleteEventBusOutput::builder()
                    .build()
            });
        let eb = mock_client!(
            aws_sdk_eventbridge,
            RuleMode::Sequential,
            &[create.clone(), delete.clone()]
        );

        create_floor_bus(&eb).await.unwrap();
        remove_floor_bus(&eb).await.unwrap();

        assert_eq!(create.num_calls(), 1);
        assert_eq!(delete.num_calls(), 1);
    }

    #[tokio::test]
    async fn the_floor_bus_fixture_tolerates_leftovers_in_both_directions() {
        let exists = mock!(aws_sdk_eventbridge::Client::create_event_bus).then_error(|| {
            aws_sdk_eventbridge::operation::create_event_bus::CreateEventBusError::ResourceAlreadyExistsException(
                aws_sdk_eventbridge::types::error::ResourceAlreadyExistsException::builder()
                    .message("exists")
                    .build(),
            )
        });
        let gone = mock!(aws_sdk_eventbridge::Client::delete_event_bus).then_error(|| {
            aws_sdk_eventbridge::operation::delete_event_bus::DeleteEventBusError::generic(
                aws_sdk_eventbridge::error::ErrorMetadata::builder()
                    .code("ResourceNotFoundException")
                    .message("gone")
                    .build(),
            )
        });
        let eb = mock_client!(aws_sdk_eventbridge, RuleMode::Sequential, &[exists, gone]);

        create_floor_bus(&eb).await.unwrap();
        remove_floor_bus(&eb).await.unwrap();
    }
}
