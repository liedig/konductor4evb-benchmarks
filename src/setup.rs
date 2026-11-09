//! Setup verifies rather than provisions (design decision 1): the
//! configured domain must already be ACTIVE, and the benchmark schema is
//! ensured idempotently. Failures name the missing prerequisite instead of
//! writing fixtures on top of it.
//!
//! The ACTIVE check reads the domains table directly: the platform's
//! describe endpoint requires an owner binding, and the harness seeds only
//! a publisher binding (which `ensure_schema` and publishing do need).

use anyhow::{Context, Result};

use aws_sdk_dynamodb::types::AttributeValue;

use crate::platform::{PlatformApi, SchemaEnsure};

/// The domain's status string, or `None` when the domain does not exist.
pub async fn domain_status(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    domain: &str,
) -> Result<Option<String>> {
    let out = ddb
        .get_item()
        .table_name(table)
        .key("domainName", AttributeValue::S(domain.to_string()))
        .send()
        .await
        .with_context(|| format!("reading domain '{domain}' from {table}"))?;
    Ok(out
        .item()
        .and_then(|item| item.get("status"))
        .and_then(|v| v.as_s().ok())
        .cloned())
}

/// What setup established; the report shows whether the schema was fresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupReport {
    pub domain: String,
    pub schema: SchemaEnsure,
}

/// Verifies the configured domain is usable and ensures the benchmark
/// schema exists. Any other domain state fails here, before a single
/// fixture is written, with the operator step named in the error.
pub async fn ensure_ready(
    api: &impl PlatformApi,
    domain: &str,
    status: Option<String>,
) -> Result<SetupReport> {
    match status.as_deref() {
        None => anyhow::bail!(
            "benchmark domain '{domain}' does not exist: register it (POST /domains) and wait \
             for approval before running the benchmark"
        ),
        Some(status) if status != "ACTIVE" => anyhow::bail!(
            "benchmark domain '{domain}' is {status}, not ACTIVE: enable it (or finish its \
             registration approval) before running the benchmark"
        ),
        Some(_) => {}
    }
    let schema = api.ensure_schema(domain).await?;
    Ok(SetupReport {
        domain: domain.to_string(),
        schema,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_dynamodb::types::AttributeValue;
    use aws_smithy_mocks::{mock, mock_client, RuleMode};
    use std::sync::Mutex;

    struct StubApi {
        schema: SchemaEnsure,
        calls: Mutex<Vec<&'static str>>,
    }

    impl StubApi {
        fn new(schema: SchemaEnsure) -> Self {
            Self {
                schema,
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl PlatformApi for StubApi {
        async fn ensure_schema(&self, _domain: &str) -> Result<SchemaEnsure> {
            self.calls.lock().unwrap().push("ensure_schema");
            Ok(self.schema)
        }

        async fn publish(&self, _body: &serde_json::Value) -> Result<u16> {
            self.calls.lock().unwrap().push("publish");
            Ok(202)
        }
    }

    #[tokio::test]
    async fn an_active_domain_proceeds_to_schema_ensure() {
        let api = StubApi::new(SchemaEnsure::Created);
        let report = ensure_ready(&api, "benchmark", Some("ACTIVE".to_string()))
            .await
            .unwrap();
        assert_eq!(
            report,
            SetupReport {
                domain: "benchmark".to_string(),
                schema: SchemaEnsure::Created
            }
        );
        assert_eq!(api.calls.lock().unwrap().as_slice(), ["ensure_schema"]);
    }

    #[tokio::test]
    async fn a_schema_conflict_is_treated_as_already_present() {
        let api = StubApi::new(SchemaEnsure::AlreadyExists);
        let report = ensure_ready(&api, "benchmark", Some("ACTIVE".to_string()))
            .await
            .unwrap();
        assert_eq!(report.schema, SchemaEnsure::AlreadyExists);
    }

    #[tokio::test]
    async fn a_missing_domain_fails_with_registration_guidance_before_any_fixture() {
        let api = StubApi::new(SchemaEnsure::Created);
        let err = ensure_ready(&api, "benchmark", None).await.unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("benchmark"), "names the domain: {message}");
        assert!(
            message.contains("register") && message.contains("approv"),
            "operator guidance: {message}"
        );
        assert!(
            !api.calls.lock().unwrap().contains(&"ensure_schema"),
            "no fixture writes before the domain check passes"
        );
    }

    #[tokio::test]
    async fn a_non_active_domain_fails_naming_its_status() {
        let api = StubApi::new(SchemaEnsure::Created);
        let err = ensure_ready(&api, "benchmark", Some("INACTIVE".to_string()))
            .await
            .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("INACTIVE"), "names the status: {message}");
        assert!(!api.calls.lock().unwrap().contains(&"ensure_schema"));
    }

    #[tokio::test]
    async fn domain_status_reads_the_domains_table_directly() {
        let get = mock!(aws_sdk_dynamodb::Client::get_item)
            .match_requests(|req| {
                req.table_name() == Some("domains-test")
                    && req
                        .key()
                        .and_then(|k| k.get("domainName"))
                        .and_then(|v| v.as_s().ok())
                        .map(String::as_str)
                        == Some("sales")
            })
            .then_output(|| {
                aws_sdk_dynamodb::operation::get_item::GetItemOutput::builder()
                    .item("status", AttributeValue::S("ACTIVE".to_string()))
                    .item("domainName", AttributeValue::S("sales".to_string()))
                    .build()
            });
        let ddb = mock_client!(
            aws_sdk_dynamodb,
            RuleMode::Sequential,
            std::slice::from_ref(&get)
        );

        let status = domain_status(&ddb, "domains-test", "sales").await.unwrap();

        assert_eq!(status, Some("ACTIVE".to_string()));
    }

    #[tokio::test]
    async fn a_missing_item_means_the_domain_does_not_exist() {
        let get = mock!(aws_sdk_dynamodb::Client::get_item).then_output(|| {
            aws_sdk_dynamodb::operation::get_item::GetItemOutput::builder().build()
        });
        let ddb = mock_client!(
            aws_sdk_dynamodb,
            RuleMode::Sequential,
            std::slice::from_ref(&get)
        );

        let status = domain_status(&ddb, "domains-test", "ghost").await.unwrap();

        assert_eq!(status, None);
    }
}
