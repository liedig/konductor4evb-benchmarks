//! The platform's IAM-authenticated API surface as the benchmark uses it:
//! domain lookup, schema ensure, and publish. `HttpPlatformApi` signs every
//! call with the function's execution role (see `sigv4.rs`); tests stub the
//! trait instead of standing up an HTTP server.

use anyhow::{Context, Result};
use serde_json::{json, Value};

/// The benchmark event's detail-type; must be feasible in an EventBridge
/// rule name and match the schema registered by setup.
pub const BENCH_EVENT: &str = "BenchmarkEvent";

/// A permissive authoring contract: admission validation still runs, but
/// the benchmark payload is free-form so it can carry trace fields.
pub const BENCH_SCHEMA: &str = r#"{
    "$schema": "http://json-schema.org/draft-04/schema#",
    "type": "object"
}"#;

/// Outcome of ensuring the benchmark schema exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaEnsure {
    Created,
    AlreadyExists,
}

/// The platform calls the benchmark phases need.
// Send futures: the load phase spawns publishes through the pacer.
#[trait_variant::make(PlatformApi: Send)]
// Only the generated Send variant is used; the local definition is the
// macro's source.
#[allow(dead_code)]
pub trait LocalPlatformApi: Send + Sync {
    /// Creates the benchmark schema; a conflict means a previous run (or an
    /// operator) already created it.
    async fn ensure_schema(&self, domain: &str) -> Result<SchemaEnsure>;
    /// Publishes one event, returning the HTTP status code.
    async fn publish(&self, body: &Value) -> Result<u16>;
}

/// `POST /schemas` body, matching the platform's CreateSchemaRequest.
pub fn create_schema_body(domain: &str, event_name: &str, content: &str) -> Value {
    json!({
        "domain": domain,
        "eventName": event_name,
        "schemaType": "JSONSchemaDraft4",
        "content": content,
    })
}

/// `POST /publish` body, matching the platform's PublishRequest. The trace
/// fields ride inside `data` so arrival latency can be computed downstream.
pub fn publish_body(domain: &str, trace_id: &str, sent_at_ms: i64, idempotency_key: &str) -> Value {
    json!({
        "domain": domain,
        "eventName": BENCH_EVENT,
        "idempotencyKey": idempotency_key,
        "data": {
            "traceId": trace_id,
            "sentAtMs": sent_at_ms,
        }
    })
}

/// SigV4-signed HTTP implementation against a deployed platform.
/// Bounds for platform calls; the slowest legitimate publish is well
/// under a second even at p99, so 30s is generous.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub struct HttpPlatformApi {
    client: reqwest::Client,
    signer: crate::sigv4::ExecuteApiSigner,
    base_url: String,
}

impl HttpPlatformApi {
    pub fn new(signer: crate::sigv4::ExecuteApiSigner, base_url: impl Into<String>) -> Self {
        Self::new_with_timeout(signer, base_url, REQUEST_TIMEOUT)
    }

    /// A hung publish must error the request: the default reqwest client
    /// has no overall timeout, and a stalled send would park a pacer task
    /// until the Lambda's hard timeout killed the whole run, fixtures
    /// included.
    pub fn new_with_timeout(
        signer: crate::sigv4::ExecuteApiSigner,
        base_url: impl Into<String>,
        timeout: std::time::Duration,
    ) -> Self {
        // The builder only fails on invalid TLS/proxy configuration; this
        // client sets neither, so the fallback is unreachable.
        Self {
            client: reqwest::Client::builder()
                .timeout(timeout)
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            signer,
            base_url: base_url.into(),
        }
    }

    /// Sends one signed request and returns the status and parsed body.
    /// Transport errors surface; non-2xx statuses are data, not errors.
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value)> {
        let url = format!("{}{}", self.base_url, path);
        let payload = body.map(serde_json::to_vec).transpose()?;
        let payload = payload.unwrap_or_default();
        let headers = self.signer.sign_headers(method, &url, &payload)?;
        let mut request = match method {
            "GET" => self.client.get(&url),
            "POST" => self.client.post(&url),
            other => anyhow::bail!("unsupported method {other}"),
        };
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if body.is_some() {
            request = request
                .header("content-type", "application/json")
                .body(payload);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        let parsed = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok((status, parsed))
    }
}

impl PlatformApi for HttpPlatformApi {
    async fn ensure_schema(&self, domain: &str) -> Result<SchemaEnsure> {
        let body = create_schema_body(domain, BENCH_EVENT, BENCH_SCHEMA);
        let (status, body) = self.request("POST", "/schemas", Some(&body)).await?;
        match status {
            201 => Ok(SchemaEnsure::Created),
            409 => Ok(SchemaEnsure::AlreadyExists),
            other => {
                anyhow::bail!("POST /schemas answered {other}: {body}")
            }
        }
    }

    async fn publish(&self, body: &Value) -> Result<u16> {
        let (status, _) = self
            .request("POST", "/publish", Some(body))
            .await
            .context("publish request failed")?;
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> crate::sigv4::ExecuteApiSigner {
        crate::sigv4::ExecuteApiSigner::new("akid", "secret", None, "us-east-1")
    }

    #[tokio::test]
    async fn a_hung_server_fails_the_request_instead_of_hanging_the_run() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sink = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let api = HttpPlatformApi::new_with_timeout(
            signer(),
            format!("http://{addr}"),
            std::time::Duration::from_millis(500),
        );

        let started = std::time::Instant::now();
        let result = PlatformApi::publish(&api, &publish_body("benchmark", "t-1", 0, "k-1")).await;
        sink.abort();

        assert!(result.is_err(), "the hung server errors the request");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the request times out promptly instead of hanging the run"
        );
    }
}
