//! In-region benchmark driver Lambda: measures the platform's publish path
//! against the native EventBridge floor and reports all four surfaces.
//! See docs/plan/design.md.

mod arrival;
mod collect;
mod fixtures;
mod floor;
mod handler;
mod load;
mod pacer;
mod platform;
mod report;
mod setup;
mod sigv4;

use aws_config::BehaviorVersion;
use aws_credential_types::provider::ProvideCredentials;
use lambda_runtime::{service_fn, Error};

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
    let region = config
        .region()
        .map(|r| r.to_string())
        .unwrap_or_else(|| "us-east-1".to_string());
    let credentials = config
        .credentials_provider()
        .ok_or_else(|| Error::from("no credentials provider in the Lambda environment"))?
        .provide_credentials()
        .await?;

    let clients = handler::Clients {
        ddb: aws_sdk_dynamodb::Client::new(&config),
        events: aws_sdk_eventbridge::Client::new(&config),
        floor_events: floor::floor_client(&config),
        cw: aws_sdk_cloudwatch::Client::new(&config),
        logs: aws_sdk_cloudwatchlogs::Client::new(&config),
        sqs: aws_sdk_sqs::Client::new(&config),
        signer: sigv4::ExecuteApiSigner::new(
            credentials.access_key_id(),
            credentials.secret_access_key(),
            credentials.session_token().map(str::to_string),
            region,
        ),
    };
    let clients_ref = &clients;

    lambda_runtime::run(service_fn(move |event| async move {
        handler::handle(clients_ref, event).await
    }))
    .await
}
