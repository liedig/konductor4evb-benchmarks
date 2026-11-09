//! The arrival probe: a temporary EventBridge rule on the benchmark
//! domain's bus delivering `BenchmarkEvent` events to a CloudWatch log
//! group, so the report can reconcile accepted publishes against arrivals
//! and measure end-to-end latency. Function-managed because the operator's
//! deploy credentials should only ever touch this stack's own resources
//! (design decision 2).

use anyhow::{Context, Result};
use aws_sdk_cloudwatchlogs::operation::create_log_group::CreateLogGroupError;
use aws_sdk_cloudwatchlogs::operation::delete_log_group::DeleteLogGroupError;
use aws_sdk_cloudwatchlogs::operation::delete_resource_policy::DeleteResourcePolicyError;
use aws_sdk_eventbridge::operation::delete_rule::DeleteRuleError;
use aws_sdk_eventbridge::operation::remove_targets::RemoveTargetsError;

/// Fixed rule name; deterministic so a leftover from an interrupted run is
/// recognizable and re-creatable.
pub const ARRIVAL_RULE: &str = "konductor-benchmark-arrival";

/// Log groups under `/aws/events/` are the documented EventBridge target
/// convention; the function still sets an explicit resource policy.
pub const ARRIVAL_LOG_GROUP: &str = "/aws/events/konductor-benchmark-arrival";

/// Identifiers the collect phase needs later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrivalProbe {
    pub rule_name: String,
    pub bus_name: String,
    pub log_group: String,
}

/// The event pattern matching only benchmark events on the domain bus.
pub fn arrival_event_pattern() -> String {
    serde_json::json!({"detail-type": [crate::platform::BENCH_EVENT]}).to_string()
}

const RESOURCE_POLICY_NAME: &str = "konductor-benchmark-arrival";

/// Creates the log group, grants EventBridge delivery, and wires the rule.
/// Already-existing pieces from an interrupted run are tolerated; anything
/// else fails the phase (and cleanup removes what did get created).
pub async fn create_arrival_probe(
    eb: &aws_sdk_eventbridge::Client,
    logs: &aws_sdk_cloudwatchlogs::Client,
    bus_name: &str,
    log_group_arn: &str,
) -> Result<ArrivalProbe> {
    match logs
        .create_log_group()
        .log_group_name(ARRIVAL_LOG_GROUP)
        .send()
        .await
    {
        Ok(_) => {}
        Err(e)
            if matches!(
                e.as_service_error(),
                Some(CreateLogGroupError::ResourceAlreadyExistsException(_))
            ) =>
        {
            tracing::debug!("arrival log group already exists; reusing it");
        }
        Err(e) => return Err(e).context("creating the arrival log group"),
    }

    let policy = serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "events.amazonaws.com"},
            "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": format!("{log_group_arn}:*"),
        }]
    });
    logs.put_resource_policy()
        .policy_name(RESOURCE_POLICY_NAME)
        .policy_document(policy.to_string())
        .send()
        .await
        .context("granting EventBridge delivery to the arrival log group")?;

    eb.put_rule()
        .name(ARRIVAL_RULE)
        .event_bus_name(bus_name)
        .event_pattern(arrival_event_pattern())
        .state(aws_sdk_eventbridge::types::RuleState::Enabled)
        .send()
        .await
        .context("creating the arrival rule")?;

    let output = eb
        .put_targets()
        .rule(ARRIVAL_RULE)
        .event_bus_name(bus_name)
        .targets(
            aws_sdk_eventbridge::types::Target::builder()
                .id("logs")
                .arn(log_group_arn)
                .build()?,
        )
        .send()
        .await
        .context("attaching the arrival log target")?;
    if output.failed_entry_count() > 0 {
        let reasons: Vec<&str> = output
            .failed_entries()
            .iter()
            .filter_map(|f| f.error_message())
            .collect();
        anyhow::bail!(
            "attaching the arrival log target failed: {}",
            reasons.join("; ")
        );
    }

    Ok(ArrivalProbe {
        rule_name: ARRIVAL_RULE.to_string(),
        bus_name: bus_name.to_string(),
        log_group: ARRIVAL_LOG_GROUP.to_string(),
    })
}

/// Removes the rule, its targets, the resource policy, and the log group,
/// tolerating anything already gone (interrupted cleanup, or teardown after
/// a failed setup).
pub async fn remove_arrival_probe(
    eb: &aws_sdk_eventbridge::Client,
    logs: &aws_sdk_cloudwatchlogs::Client,
    bus_name: &str,
) -> Result<()> {
    if let Err(e) = eb
        .remove_targets()
        .rule(ARRIVAL_RULE)
        .event_bus_name(bus_name)
        .ids("logs")
        .send()
        .await
    {
        if !matches!(
            e.as_service_error(),
            Some(RemoveTargetsError::ResourceNotFoundException(_))
        ) {
            return Err(e).context("removing the arrival target");
        }
    }
    if let Err(e) = eb
        .delete_rule()
        .name(ARRIVAL_RULE)
        .event_bus_name(bus_name)
        .send()
        .await
    {
        if !matches!(
            e.as_service_error(),
            Some(DeleteRuleError::ResourceNotFoundException(_))
        ) {
            return Err(e).context("deleting the arrival rule");
        }
    }
    if let Err(e) = logs
        .delete_resource_policy()
        .policy_name(RESOURCE_POLICY_NAME)
        .send()
        .await
    {
        if !matches!(
            e.as_service_error(),
            Some(DeleteResourcePolicyError::ResourceNotFoundException(_))
        ) {
            return Err(e).context("deleting the arrival resource policy");
        }
    }
    if let Err(e) = logs
        .delete_log_group()
        .log_group_name(ARRIVAL_LOG_GROUP)
        .send()
        .await
    {
        if !matches!(
            e.as_service_error(),
            Some(DeleteLogGroupError::ResourceNotFoundException(_))
        ) {
            return Err(e).context("deleting the arrival log group");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_mocks::{mock, mock_client, Rule, RuleMode};

    const BUS: &str = "acme-prod-benchmark-event-bus";
    const LOG_ARN: &str =
        "arn:aws:logs:us-east-1:111122223333:log-group:/aws/events/konductor-benchmark-arrival";

    fn create_log_group_rule() -> Rule {
        mock!(aws_sdk_cloudwatchlogs::Client::create_log_group)
            .match_requests(|req| req.log_group_name() == Some(ARRIVAL_LOG_GROUP))
            .then_output(|| {
                aws_sdk_cloudwatchlogs::operation::create_log_group::CreateLogGroupOutput::builder()
                    .build()
            })
    }

    fn put_resource_policy_rule() -> Rule {
        mock!(aws_sdk_cloudwatchlogs::Client::put_resource_policy)
            .match_requests(|req| {
                let doc = req.policy_document().unwrap_or_default();
                doc.contains("events.amazonaws.com")
                    && doc.contains("logs:PutLogEvents")
                    && doc.contains(LOG_ARN)
            })
            .then_output(|| {
                aws_sdk_cloudwatchlogs::operation::put_resource_policy::PutResourcePolicyOutput::builder()
                    .build()
            })
    }

    fn put_rule_rule() -> Rule {
        mock!(aws_sdk_eventbridge::Client::put_rule)
            .match_requests(|req| {
                req.name() == Some(ARRIVAL_RULE)
                    && req.event_bus_name() == Some(BUS)
                    && req
                        .event_pattern()
                        .unwrap_or_default()
                        .contains("BenchmarkEvent")
            })
            .then_output(|| {
                aws_sdk_eventbridge::operation::put_rule::PutRuleOutput::builder()
                    .rule_arn("arn:rule")
                    .build()
            })
    }

    fn put_targets_rule() -> Rule {
        mock!(aws_sdk_eventbridge::Client::put_targets)
            .match_requests(|req| {
                req.rule() == Some(ARRIVAL_RULE)
                    && req.event_bus_name() == Some(BUS)
                    && req.targets().len() == 1
                    && req.targets()[0].arn() == LOG_ARN
            })
            .then_output(|| {
                aws_sdk_eventbridge::operation::put_targets::PutTargetsOutput::builder()
                    .failed_entry_count(0)
                    .build()
            })
    }

    fn clients(
        eb_rules: Vec<Rule>,
        logs_rules: Vec<Rule>,
    ) -> (aws_sdk_eventbridge::Client, aws_sdk_cloudwatchlogs::Client) {
        (
            mock_client!(aws_sdk_eventbridge, RuleMode::Sequential, &eb_rules),
            mock_client!(aws_sdk_cloudwatchlogs, RuleMode::Sequential, &logs_rules),
        )
    }

    #[tokio::test]
    async fn create_wires_the_rule_to_the_log_group_on_the_domain_bus() {
        let (eb, logs) = clients(
            vec![put_rule_rule(), put_targets_rule()],
            vec![create_log_group_rule(), put_resource_policy_rule()],
        );

        let probe = create_arrival_probe(&eb, &logs, BUS, LOG_ARN)
            .await
            .unwrap();

        assert_eq!(
            probe,
            ArrivalProbe {
                rule_name: ARRIVAL_RULE.to_string(),
                bus_name: BUS.to_string(),
                log_group: ARRIVAL_LOG_GROUP.to_string(),
            }
        );
    }

    #[tokio::test]
    async fn create_tolerates_an_existing_log_group_from_an_interrupted_run() {
        let exists = mock!(aws_sdk_cloudwatchlogs::Client::create_log_group).then_error(|| {
            aws_sdk_cloudwatchlogs::operation::create_log_group::CreateLogGroupError::ResourceAlreadyExistsException(
                aws_sdk_cloudwatchlogs::types::error::ResourceAlreadyExistsException::builder()
                    .message("exists")
                    .build(),
            )
        });
        let (eb, logs) = clients(
            vec![put_rule_rule(), put_targets_rule()],
            vec![exists, put_resource_policy_rule()],
        );

        create_arrival_probe(&eb, &logs, BUS, LOG_ARN)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn remove_tolerates_everything_already_gone() {
        let not_found_targets = mock!(aws_sdk_eventbridge::Client::remove_targets).then_error(|| {
            aws_sdk_eventbridge::operation::remove_targets::RemoveTargetsError::ResourceNotFoundException(
                aws_sdk_eventbridge::types::error::ResourceNotFoundException::builder()
                    .message("gone")
                    .build(),
            )
        });
        let not_found_rule = mock!(aws_sdk_eventbridge::Client::delete_rule).then_error(|| {
            aws_sdk_eventbridge::operation::delete_rule::DeleteRuleError::ResourceNotFoundException(
                aws_sdk_eventbridge::types::error::ResourceNotFoundException::builder()
                    .message("gone")
                    .build(),
            )
        });
        let not_found_policy = mock!(aws_sdk_cloudwatchlogs::Client::delete_resource_policy)
            .match_requests(|req| req.policy_name() == Some(RESOURCE_POLICY_NAME))
            .then_error(|| {
                aws_sdk_cloudwatchlogs::operation::delete_resource_policy::DeleteResourcePolicyError::ResourceNotFoundException(
                    aws_sdk_cloudwatchlogs::types::error::ResourceNotFoundException::builder()
                        .message("gone")
                        .build(),
                )
            });
        let not_found_group = mock!(aws_sdk_cloudwatchlogs::Client::delete_log_group).then_error(|| {
            aws_sdk_cloudwatchlogs::operation::delete_log_group::DeleteLogGroupError::ResourceNotFoundException(
                aws_sdk_cloudwatchlogs::types::error::ResourceNotFoundException::builder()
                    .message("gone")
                    .build(),
            )
        });
        let (eb, logs) = clients(
            vec![not_found_targets.clone(), not_found_rule.clone()],
            vec![not_found_policy.clone(), not_found_group.clone()],
        );

        remove_arrival_probe(&eb, &logs, BUS).await.unwrap();

        assert_eq!(not_found_targets.num_calls(), 1);
        assert_eq!(not_found_rule.num_calls(), 1);
        assert_eq!(not_found_policy.num_calls(), 1);
        assert_eq!(not_found_group.num_calls(), 1);
    }

    #[tokio::test]
    async fn an_unexpected_create_error_propagates() {
        let broken = mock!(aws_sdk_eventbridge::Client::put_rule).then_error(|| {
            aws_sdk_eventbridge::operation::put_rule::PutRuleError::generic(
                aws_sdk_eventbridge::error::ErrorMetadata::builder()
                    .message("limit exceeded")
                    .build(),
            )
        });
        let (eb, logs) = clients(
            vec![broken],
            vec![create_log_group_rule(), put_resource_policy_rule()],
        );

        let err = create_arrival_probe(&eb, &logs, BUS, LOG_ARN)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("limit exceeded"));
    }
}
