//! Fixture items the benchmark seeds into the deployed platform's tables and
//! removes after every run.
//!
//! INTENTIONAL DUPLICATION: the binding item format below is copied from the
//! platform's `lib/konductor-common/src/grant_store.rs` (`binding_to_item`)
//! and `lib/konductor-common/src/grant.rs` (`RoleBinding::pk/sk/gsi1pk/
//! gsi1sk`), pinned at upstream commit
//! 4e3240e0b82e5f0369edceba33c96a28243e2a67. This repo is deliberately
//! standalone (user decision 2026-11-02): if the platform changes the stored
//! shape, update this file and the pinning test below.

use std::collections::HashMap;

use anyhow::Context;
use aws_sdk_dynamodb::types::AttributeValue;

/// Fixture provenance marker, so a leftover row is attributable at a glance.
pub const CREATED_BY: &str = "konductor-benchmarks";

fn s(value: &str) -> AttributeValue {
    AttributeValue::S(value.to_string())
}

/// The stored item for an IAM-account publisher binding, in exactly the
/// platform's format (see the module doc for the pinned source).
pub fn publisher_binding_item(
    account_id: &str,
    domain: &str,
    binding_id: &str,
    created_at: &str,
) -> HashMap<String, AttributeValue> {
    let principal_segment = format!("aws-account#{account_id}");
    let mut item = HashMap::new();
    item.insert(
        "PK".to_string(),
        s(&format!("PRINCIPAL#{principal_segment}")),
    );
    item.insert(
        "SK".to_string(),
        s(&format!("ROLE#publisher#DOMAIN#{domain}")),
    );
    item.insert("GSI1PK".to_string(), s(&format!("DOMAIN#{domain}")));
    item.insert(
        "GSI1SK".to_string(),
        s(&format!("ROLE#publisher#PRINCIPAL#{principal_segment}")),
    );
    item.insert("itemType".to_string(), s("RoleBinding"));
    item.insert("bindingId".to_string(), s(binding_id));
    item.insert("principalType".to_string(), s("aws-account"));
    item.insert("accountId".to_string(), s(account_id));
    item.insert("role".to_string(), s("publisher"));
    item.insert("domain".to_string(), s(domain));
    item.insert("state".to_string(), s("active"));
    item.insert("createdAt".to_string(), s(created_at));
    item.insert("createdBy".to_string(), s(CREATED_BY));
    item
}

/// The fixed fixture binding id: deterministic, so reseeding is idempotent
/// and a leftover is attributable.
pub const BINDING_ID: &str = "konductor-benchmarks-publisher";

fn binding_keys(account_id: &str, domain: &str) -> (String, String) {
    (
        format!("PRINCIPAL#aws-account#{account_id}"),
        format!("ROLE#publisher#DOMAIN#{domain}"),
    )
}

/// A seeded binding's key, carried so cleanup deletes exactly what setup
/// wrote.
#[derive(Debug)]
pub struct SeededBinding {
    pub pk: String,
    pub sk: String,
}

/// Seeds the IAM-account publisher binding. `POST /schemas` authorizes the
/// same publisher action as `POST /publish` (verified against the
/// platform's schema-admin handler), so this one binding is the whole set.
/// PutItem overwrites by key, so reseeding after an interrupted run is safe.
pub async fn seed_binding(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    account_id: &str,
    domain: &str,
) -> anyhow::Result<SeededBinding> {
    let created_at = chrono::Utc::now().to_rfc3339();
    let item = publisher_binding_item(account_id, domain, BINDING_ID, &created_at);
    ddb.put_item()
        .table_name(table)
        .set_item(Some(item))
        .send()
        .await
        .with_context(|| {
            format!("seeding the publisher binding for account {account_id} on domain '{domain}'")
        })?;
    let (pk, sk) = binding_keys(account_id, domain);
    Ok(SeededBinding { pk, sk })
}

/// Removes a seeded binding by key. DynamoDB deletes are idempotent, so a
/// repeated cleanup after an interrupted run is safe.
pub async fn remove_binding(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    binding: &SeededBinding,
) -> anyhow::Result<()> {
    ddb.delete_item()
        .table_name(table)
        .key("PK", s(&binding.pk))
        .key("SK", s(&binding.sk))
        .send()
        .await
        .context("removing the seeded publisher binding")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_mocks::{mock, mock_client, RuleMode};

    /// Pins the duplication against the platform's stored binding shape (see
    /// the module doc): every key and attribute, byte for byte. If the
    /// platform's `binding_to_item` or key patterns change, this test is the
    /// deliberate one-file update point.
    #[test]
    fn publisher_binding_item_matches_the_platform_shape() {
        let item = publisher_binding_item(
            "111122223333",
            "benchmark",
            "rb-benchmark",
            "2026-11-02T00:00:00+00:00",
        );

        let expected: HashMap<String, AttributeValue> = HashMap::from([
            ("PK".to_string(), s("PRINCIPAL#aws-account#111122223333")),
            ("SK".to_string(), s("ROLE#publisher#DOMAIN#benchmark")),
            ("GSI1PK".to_string(), s("DOMAIN#benchmark")),
            (
                "GSI1SK".to_string(),
                s("ROLE#publisher#PRINCIPAL#aws-account#111122223333"),
            ),
            ("itemType".to_string(), s("RoleBinding")),
            ("bindingId".to_string(), s("rb-benchmark")),
            ("principalType".to_string(), s("aws-account")),
            ("accountId".to_string(), s("111122223333")),
            ("role".to_string(), s("publisher")),
            ("domain".to_string(), s("benchmark")),
            ("state".to_string(), s("active")),
            ("createdAt".to_string(), s("2026-11-02T00:00:00+00:00")),
            ("createdBy".to_string(), s("konductor-benchmarks")),
        ]);
        assert_eq!(item, expected);
    }

    #[tokio::test]
    async fn seed_writes_the_publisher_binding_to_the_bindings_table() {
        let put = mock!(aws_sdk_dynamodb::Client::put_item)
            .match_requests(|req| {
                if req.table_name() != Some("bindings-test") {
                    return false;
                }
                let item = req.item().cloned().unwrap_or_default();
                item.get("PK") == Some(&s("PRINCIPAL#aws-account#111122223333"))
                    && item.get("SK") == Some(&s("ROLE#publisher#DOMAIN#benchmark"))
                    && item.get("principalType") == Some(&s("aws-account"))
                    && item.get("role") == Some(&s("publisher"))
                    && item.get("state") == Some(&s("active"))
                    && item.get("createdBy") == Some(&s("konductor-benchmarks"))
            })
            .then_output(|| {
                aws_sdk_dynamodb::operation::put_item::PutItemOutput::builder().build()
            });
        let ddb = mock_client!(
            aws_sdk_dynamodb,
            RuleMode::Sequential,
            std::slice::from_ref(&put)
        );

        let seeded = seed_binding(&ddb, "bindings-test", "111122223333", "benchmark")
            .await
            .unwrap();
        assert_eq!(put.num_calls(), 1);
        assert_eq!(seeded.pk, "PRINCIPAL#aws-account#111122223333");
        assert_eq!(seeded.sk, "ROLE#publisher#DOMAIN#benchmark");
    }

    #[tokio::test]
    async fn remove_deletes_the_binding_by_its_key_only() {
        let delete = mock!(aws_sdk_dynamodb::Client::delete_item)
            .match_requests(|req| {
                req.table_name() == Some("bindings-test")
                    && req.key().map(|k| k.len()) == Some(2)
                    && req.key().and_then(|k| k.get("PK"))
                        == Some(&s("PRINCIPAL#aws-account#111122223333"))
                    && req.key().and_then(|k| k.get("SK"))
                        == Some(&s("ROLE#publisher#DOMAIN#benchmark"))
            })
            .then_output(|| {
                aws_sdk_dynamodb::operation::delete_item::DeleteItemOutput::builder().build()
            });
        let ddb = mock_client!(
            aws_sdk_dynamodb,
            RuleMode::Sequential,
            std::slice::from_ref(&delete)
        );

        let seeded = SeededBinding {
            pk: "PRINCIPAL#aws-account#111122223333".to_string(),
            sk: "ROLE#publisher#DOMAIN#benchmark".to_string(),
        };
        remove_binding(&ddb, "bindings-test", &seeded)
            .await
            .unwrap();
        assert_eq!(delete.num_calls(), 1);
    }

    #[tokio::test]
    async fn seed_propagates_store_errors_so_setup_fails_visibly() {
        let err_rule = mock!(aws_sdk_dynamodb::Client::put_item).then_error(|| {
            aws_sdk_dynamodb::operation::put_item::PutItemError::generic(
                aws_sdk_dynamodb::error::ErrorMetadata::builder()
                    .message("access denied")
                    .build(),
            )
        });
        let ddb = mock_client!(aws_sdk_dynamodb, RuleMode::Sequential, &[err_rule]);
        let err = seed_binding(&ddb, "bindings-test", "111122223333", "benchmark")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("access denied"));
    }
}
