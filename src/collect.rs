//! Post-run collection. The handler applies the settle window first (the
//! measure window's end passed in here already includes it), then this
//! phase pulls: per-stage publish-path latency percentiles from the
//! platform's metric filters, the publish DLQ depth, and the arrival
//! probe's Logs Insights rollup (count + ingestion-time latency). Missing
//! metric datapoints are `None`, never zero: an absent metric means the
//! platform emitted nothing, and the report must say so.

use anyhow::{Context, Result};
use serde::Serialize;

/// The platform's publish-path metric namespace (konductor-core-stack
/// metric filters).
pub const METRIC_NAMESPACE: &str = "Konductor/EventStreams";

/// The three publish-path stage metrics, in path order.
pub const STAGE_METRICS: [&str; 3] = [
    "PublishAdmissionLatency",
    "PublishQueueWait",
    "PublishEnqueueToDelivery",
];

/// Conventional publish DLQ name from konductor-core-stack.
pub const PUBLISH_DLQ: &str = "konductor-publish-dlq";

/// The measured window extended by the settle period: `[start_ms, end_ms]`
/// epoch milliseconds. Metric periods round up to whole minutes (CloudWatch
/// requirement); the arrival query uses the exact bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeasureWindow {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Extended statistics for one stage metric, milliseconds. `None` when the
/// window produced no datapoints.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StageStats {
    pub metric: String,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

/// Arrival probe rollup: how many published events reached the domain bus
/// and their send-to-ingestion latency (ingestion minus the client's
/// `sentAtMs`), milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArrivalStats {
    pub arrivals: u64,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

/// Everything the report needs that the run itself did not measure.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Collection {
    pub stages: Vec<StageStats>,
    pub dlq_depth: u64,
    pub arrivals: ArrivalStats,
}

impl MeasureWindow {
    pub fn new(start_ms: i64, end_ms: i64) -> Result<Self> {
        if end_ms <= start_ms {
            anyhow::bail!("the measure window ends before it starts: {start_ms}..{end_ms}");
        }
        Ok(Self { start_ms, end_ms })
    }

    /// The largest minute-multiple period no longer than the window
    /// (minimum 60s). Rounding down guarantees a period-aligned timestamp
    /// lands inside [start, end]; rounding up could exceed the window and
    /// CloudWatch would return no datapoints at all.
    fn period_secs(&self) -> i32 {
        let secs = (self.end_ms - self.start_ms) / 1000;
        ((secs / 60).max(1) * 60) as i32
    }
}

/// The regex scoping arrival rows to this run's publish trace ids
/// (`{run_id}-{digits}`). Floor ids carry a `-floor-` segment and never
/// match, so the floor probe cannot pollute the publish reconciliation
/// even if clock skew pulls its events into the window.
pub fn trace_id_scope(run_id: &str) -> String {
    format!("^{}-[0-9]+$", regex::escape(run_id))
}

/// Logs Insights rollup over the arrival log: one row with the arrival
/// count and ingestion-time latency percentiles (event ingestion minus the
/// client-recorded `sentAtMs`). Parsed fields arrive as strings; Insights
/// coerces numeric strings in arithmetic.
pub fn arrival_query(run_id: &str) -> String {
    format!(
        "fields @ingestionTime, @message \
         | parse @message /\"traceId\":\"(?<traceId>[^\"]+)\"/ \
         | parse @message /\"sentAtMs\":(?<sentMs>[0-9]+)/ \
         | filter isPresent(traceId) \
         | filter traceId like /{scope}/ \
         | stats count(*) as arrivals, \
           pct(toMillis(@ingestionTime) - sentMs, 50) as p50_ms, \
           pct(toMillis(@ingestionTime) - sentMs, 95) as p95_ms, \
           pct(toMillis(@ingestionTime) - sentMs, 99) as p99_ms",
        scope = trace_id_scope(run_id)
    )
}

const QUERY_POLL_SECS: u64 = 2;
const QUERY_MAX_POLLS: u32 = 30;

fn percentile_from(datapoint: &aws_sdk_cloudwatch::types::Datapoint, name: &str) -> Option<f64> {
    datapoint
        .extended_statistics()
        .and_then(|stats| stats.get(name).copied())
}

async fn stage_stats(
    cw: &aws_sdk_cloudwatch::Client,
    window: &MeasureWindow,
) -> Result<Vec<StageStats>> {
    let mut stages = Vec::with_capacity(STAGE_METRICS.len());
    for metric in STAGE_METRICS {
        let output = cw
            .get_metric_statistics()
            .namespace(METRIC_NAMESPACE)
            .metric_name(metric)
            .start_time(aws_smithy_types::DateTime::from_millis(window.start_ms))
            .end_time(aws_smithy_types::DateTime::from_millis(window.end_ms))
            .period(window.period_secs())
            .extended_statistics("p50")
            .extended_statistics("p95")
            .extended_statistics("p99")
            .send()
            .await
            .with_context(|| format!("reading the {metric} metric"))?;
        // The window rarely aligns to minute boundaries, so CloudWatch can
        // split it into two periods; the period holding more samples is the
        // window's core.
        let dominant = output.datapoints().iter().max_by(|a, b| {
            a.sample_count()
                .partial_cmp(&b.sample_count())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        stages.push(match dominant {
            Some(d) => StageStats {
                metric: metric.to_string(),
                p50_ms: percentile_from(d, "p50"),
                p95_ms: percentile_from(d, "p95"),
                p99_ms: percentile_from(d, "p99"),
            },
            None => StageStats {
                metric: metric.to_string(),
                p50_ms: None,
                p95_ms: None,
                p99_ms: None,
            },
        });
    }
    Ok(stages)
}

/// Visible publish-DLQ depth. Called before the load phase and after the
/// settle window; reconciliation uses the delta (the queue is shared and
/// platform-wide, so the absolute depth is not run-scoped).
pub async fn dlq_depth(sqs: &aws_sdk_sqs::Client, queue_name: &str) -> Result<u64> {
    let url = sqs
        .get_queue_url()
        .queue_name(queue_name)
        .send()
        .await
        .with_context(|| format!("resolving the publish DLQ '{queue_name}'"))?
        .queue_url()
        .with_context(|| format!("the publish DLQ '{queue_name}' resolved to no URL"))?
        .to_string();
    let output = sqs
        .get_queue_attributes()
        .queue_url(&url)
        .attribute_names(aws_sdk_sqs::types::QueueAttributeName::ApproximateNumberOfMessages)
        .send()
        .await
        .context("reading the publish DLQ depth")?;
    let depth = output
        .attributes()
        .and_then(|attrs| {
            attrs
                .get(&aws_sdk_sqs::types::QueueAttributeName::ApproximateNumberOfMessages)
                .map(String::as_str)
        })
        .context("the DLQ attributes carried no ApproximateNumberOfMessages")?
        .parse()
        .context("parsing the publish DLQ depth")?;
    Ok(depth)
}

async fn arrival_stats(
    logs: &aws_sdk_cloudwatchlogs::Client,
    window: &MeasureWindow,
    run_id: &str,
) -> Result<ArrivalStats> {
    let query_id = logs
        .start_query()
        .log_group_name(crate::arrival::ARRIVAL_LOG_GROUP)
        .start_time(window.start_ms / 1000)
        .end_time(window.end_ms / 1000)
        .query_string(arrival_query(run_id))
        .send()
        .await
        .context("starting the arrival query")?
        .query_id()
        .context("the arrival query returned no query id")?
        .to_string();
    for _ in 0..QUERY_MAX_POLLS {
        let output = logs
            .get_query_results()
            .query_id(&query_id)
            .send()
            .await
            .context("polling the arrival query")?;
        match output.status() {
            Some(aws_sdk_cloudwatchlogs::types::QueryStatus::Complete) => {
                return parse_arrival_row(output.results());
            }
            Some(aws_sdk_cloudwatchlogs::types::QueryStatus::Running)
            | Some(aws_sdk_cloudwatchlogs::types::QueryStatus::Scheduled) => {
                tokio::time::sleep(std::time::Duration::from_secs(QUERY_POLL_SECS)).await;
            }
            other => anyhow::bail!("the arrival query ended in status {other:?}"),
        }
    }
    anyhow::bail!(
        "the arrival query did not complete within {}s",
        QUERY_POLL_SECS * u64::from(QUERY_MAX_POLLS)
    )
}

fn parse_arrival_row(
    results: &[Vec<aws_sdk_cloudwatchlogs::types::ResultField>],
) -> Result<ArrivalStats> {
    let Some(row) = results.first() else {
        return Ok(ArrivalStats {
            arrivals: 0,
            p50_ms: None,
            p95_ms: None,
            p99_ms: None,
        });
    };
    let value = |name: &str| {
        row.iter()
            .find(|f| f.field() == Some(name))
            .and_then(|f| f.value())
    };
    Ok(ArrivalStats {
        arrivals: value("arrivals")
            .unwrap_or("0")
            .parse()
            .context("parsing the arrival count")?,
        p50_ms: value("p50_ms").and_then(|v| v.parse().ok()),
        p95_ms: value("p95_ms").and_then(|v| v.parse().ok()),
        p99_ms: value("p99_ms").and_then(|v| v.parse().ok()),
    })
}

/// Runs all three collectors over `window` for `run_id`.
pub async fn collect(
    cw: &aws_sdk_cloudwatch::Client,
    logs: &aws_sdk_cloudwatchlogs::Client,
    sqs: &aws_sdk_sqs::Client,
    window: &MeasureWindow,
    run_id: &str,
    dlq_name: &str,
) -> Result<Collection> {
    Ok(Collection {
        stages: stage_stats(cw, window).await?,
        dlq_depth: dlq_depth(sqs, dlq_name).await?,
        arrivals: arrival_stats(logs, window, run_id).await?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_mocks::{mock, mock_client, Rule, RuleMode};

    const RUN: &str = "bench-123";

    fn window() -> MeasureWindow {
        MeasureWindow {
            start_ms: 1_700_000_000_000,
            end_ms: 1_700_000_360_000,
        }
    }

    fn metric_rule(metric: &'static str, p50: f64, p95: f64, p99: f64) -> Rule {
        let expected_start = aws_smithy_types::DateTime::from_millis(window().start_ms);
        let expected_end = aws_smithy_types::DateTime::from_millis(window().end_ms);
        mock!(aws_sdk_cloudwatch::Client::get_metric_statistics)
            .match_requests(move |req| {
                req.namespace() == Some(METRIC_NAMESPACE)
                    && req.metric_name() == Some(metric)
                    && req.dimensions().is_empty()
                    && req.start_time() == Some(&expected_start)
                    && req.end_time() == Some(&expected_end)
                    && req.period() == Some(360)
                    && req.extended_statistics().len() == 3
            })
            .then_output(move || {
                aws_sdk_cloudwatch::operation::get_metric_statistics::GetMetricStatisticsOutput::builder()
                    .datapoints(
                        aws_sdk_cloudwatch::types::Datapoint::builder()
                            .timestamp(expected_start)
                            .extended_statistics("p50", p50)
                            .extended_statistics("p95", p95)
                            .extended_statistics("p99", p99)
                            .build(),
                    )
                    .build()
            })
    }

    fn start_query_rule() -> Rule {
        mock!(aws_sdk_cloudwatchlogs::Client::start_query)
            .match_requests(|req| {
                req.log_group_name() == Some(crate::arrival::ARRIVAL_LOG_GROUP)
                    && req.start_time() == Some(window().start_ms / 1000)
                    && req.end_time() == Some(window().end_ms / 1000)
                    && req
                        .query_string()
                        .unwrap_or_default()
                        .contains(&trace_id_scope(RUN))
            })
            .then_output(|| {
                aws_sdk_cloudwatchlogs::operation::start_query::StartQueryOutput::builder()
                    .query_id("q-1")
                    .build()
            })
    }

    fn results_rule(status: aws_sdk_cloudwatchlogs::types::QueryStatus) -> Rule {
        mock!(aws_sdk_cloudwatchlogs::Client::get_query_results)
            .match_requests(|req| req.query_id() == Some("q-1"))
            .then_output(move || {
                let mut out =
                    aws_sdk_cloudwatchlogs::operation::get_query_results::GetQueryResultsOutput::builder()
                        .status(status.clone());
                if status == aws_sdk_cloudwatchlogs::types::QueryStatus::Complete {
                    let row: Vec<_> = [
                        ("arrivals", "12"),
                        ("p50_ms", "100.5"),
                        ("p95_ms", "180.25"),
                        ("p99_ms", "260.75"),
                    ]
                    .into_iter()
                    .map(|(field, value)| {
                        aws_sdk_cloudwatchlogs::types::ResultField::builder()
                            .field(field)
                            .value(value)
                            .build()
                    })
                    .collect();
                    out = out.results(row);
                }
                out.build()
            })
    }

    fn sqs_rules(depth: &str) -> Vec<Rule> {
        vec![
            mock!(aws_sdk_sqs::Client::get_queue_url)
                .match_requests(|req| req.queue_name() == Some(PUBLISH_DLQ))
                .then_output(|| {
                    aws_sdk_sqs::operation::get_queue_url::GetQueueUrlOutput::builder()
                        .queue_url("https://sqs.us-east-1.amazonaws.com/111122223333/konductor-publish-dlq")
                        .build()
                }),
            mock!(aws_sdk_sqs::Client::get_queue_attributes)
                .then_output({
                    let depth = depth.to_string();
                    move || {
                        aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput::builder()
                            .attributes(
                                aws_sdk_sqs::types::QueueAttributeName::ApproximateNumberOfMessages,
                                &depth,
                            )
                            .build()
                    }
                }),
        ]
    }

    fn clients(
        cw_rules: Vec<Rule>,
        logs_rules: Vec<Rule>,
        sqs_rules: Vec<Rule>,
    ) -> (
        aws_sdk_cloudwatch::Client,
        aws_sdk_cloudwatchlogs::Client,
        aws_sdk_sqs::Client,
    ) {
        (
            mock_client!(aws_sdk_cloudwatch, RuleMode::Sequential, &cw_rules),
            mock_client!(aws_sdk_cloudwatchlogs, RuleMode::Sequential, &logs_rules),
            mock_client!(aws_sdk_sqs, RuleMode::Sequential, &sqs_rules),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn collects_stage_percentiles_dlq_depth_and_arrivals() {
        let (cw, logs, sqs) = clients(
            vec![
                metric_rule("PublishAdmissionLatency", 1.5, 2.5, 3.5),
                metric_rule("PublishQueueWait", 10.0, 20.0, 30.0),
                metric_rule("PublishEnqueueToDelivery", 100.0, 200.0, 300.0),
            ],
            vec![
                start_query_rule(),
                results_rule(aws_sdk_cloudwatchlogs::types::QueryStatus::Complete),
            ],
            sqs_rules("3"),
        );

        let collection = collect(&cw, &logs, &sqs, &window(), RUN, PUBLISH_DLQ)
            .await
            .unwrap();

        assert_eq!(collection.stages.len(), 3);
        assert_eq!(
            collection.stages[0],
            StageStats {
                metric: "PublishAdmissionLatency".to_string(),
                p50_ms: Some(1.5),
                p95_ms: Some(2.5),
                p99_ms: Some(3.5),
            }
        );
        assert_eq!(collection.stages[2].p99_ms, Some(300.0));
        assert_eq!(collection.dlq_depth, 3);
        assert_eq!(
            collection.arrivals,
            ArrivalStats {
                arrivals: 12,
                p50_ms: Some(100.5),
                p95_ms: Some(180.25),
                p99_ms: Some(260.75),
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_metric_with_no_datapoints_reports_none_not_zero() {
        let silent = mock!(aws_sdk_cloudwatch::Client::get_metric_statistics).then_output(|| {
            aws_sdk_cloudwatch::operation::get_metric_statistics::GetMetricStatisticsOutput::builder()
                .build()
        });
        let cw = mock_client!(
            aws_sdk_cloudwatch,
            RuleMode::MatchAny,
            std::slice::from_ref(&silent)
        );
        let logs = mock_client!(
            aws_sdk_cloudwatchlogs,
            RuleMode::Sequential,
            &[
                start_query_rule(),
                results_rule(aws_sdk_cloudwatchlogs::types::QueryStatus::Complete)
            ]
        );
        let sqs = mock_client!(aws_sdk_sqs, RuleMode::Sequential, &sqs_rules("0"));
        let (cw, logs, sqs) = (cw, logs, sqs);

        let collection = collect(&cw, &logs, &sqs, &window(), RUN, PUBLISH_DLQ)
            .await
            .unwrap();

        assert_eq!(
            collection.stages[0],
            StageStats {
                metric: "PublishAdmissionLatency".to_string(),
                p50_ms: None,
                p95_ms: None,
                p99_ms: None,
            }
        );
    }

    #[test]
    fn the_arrival_query_scopes_to_the_run_and_excludes_floor_ids() {
        let query = arrival_query(RUN);

        assert!(
            query.contains("traceId") && query.contains("sentAtMs"),
            "the query parses both fields out of the event: {query}"
        );
        assert!(
            query.contains(&trace_id_scope(RUN)),
            "the query is scoped to this run's trace-id prefix: {query}"
        );
        assert!(
            query.contains("p50_ms") && query.contains("p99_ms"),
            "the query rolls up ingestion-time latency percentiles: {query}"
        );
    }

    #[test]
    fn publish_trace_ids_match_the_run_scope_but_floor_ids_do_not() {
        let scope = trace_id_scope(RUN);

        let publish = regex::Regex::new(&scope).unwrap();
        assert!(publish.is_match("bench-123-17"));
        assert!(
            !publish.is_match("bench-123-floor-17"),
            "floor ids never enter the publish reconciliation"
        );
        assert!(
            !publish.is_match("bench-1230-17"),
            "the scope ends at digits"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn polls_until_the_insights_query_completes() {
        let (cw, logs, sqs) = clients(
            vec![
                metric_rule("PublishAdmissionLatency", 1.0, 2.0, 3.0),
                metric_rule("PublishQueueWait", 1.0, 2.0, 3.0),
                metric_rule("PublishEnqueueToDelivery", 1.0, 2.0, 3.0),
            ],
            vec![
                start_query_rule(),
                results_rule(aws_sdk_cloudwatchlogs::types::QueryStatus::Running),
                results_rule(aws_sdk_cloudwatchlogs::types::QueryStatus::Complete),
            ],
            sqs_rules("0"),
        );

        let collection = collect(&cw, &logs, &sqs, &window(), RUN, PUBLISH_DLQ)
            .await
            .unwrap();

        assert_eq!(collection.arrivals.arrivals, 12);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_insights_query_is_an_error() {
        let (cw, logs, sqs) = clients(
            vec![
                metric_rule("PublishAdmissionLatency", 1.0, 2.0, 3.0),
                metric_rule("PublishQueueWait", 1.0, 2.0, 3.0),
                metric_rule("PublishEnqueueToDelivery", 1.0, 2.0, 3.0),
            ],
            vec![
                start_query_rule(),
                results_rule(aws_sdk_cloudwatchlogs::types::QueryStatus::Failed),
            ],
            sqs_rules("0"),
        );

        let err = collect(&cw, &logs, &sqs, &window(), RUN, PUBLISH_DLQ)
            .await
            .unwrap_err();

        assert!(format!("{err:#}").contains("arrival"));
    }
    #[test]
    fn the_metric_period_never_exceeds_the_window() {
        // 605s window (not minute-aligned): rounding up would yield 660s,
        // longer than the window, and CloudWatch could return no datapoints.
        let w = MeasureWindow {
            start_ms: 1_700_000_000_000,
            end_ms: 1_700_000_605_000,
        };
        assert_eq!(w.period_secs(), 600);
        let short = MeasureWindow {
            start_ms: 0,
            end_ms: 45_000,
        };
        assert_eq!(
            short.period_secs(),
            60,
            "sub-minute windows keep the 60s minimum"
        );
    }
}
