use ashsa::client::build_http_client;
use ashsa::config::Config;
use ashsa::handler::App;
use lambda_runtime::{Error, LambdaEvent, service_fn};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Error> {
    init_tracing();

    let config = Config::from_env()?;
    let client = build_http_client(&config)?;
    let app = Arc::new(App::new(config, client));

    lambda_runtime::run(service_fn(move |event: LambdaEvent<Value>| {
        let app = Arc::clone(&app);
        let deadline = lambda_deadline(event.context.deadline());
        async move { Ok::<Value, Error>(app.handle_event(event.payload, deadline).await) }
    }))
    .await
}

/// Converts the Lambda wall-clock deadline into a monotonic `Instant`.
/// A deadline of zero means the runtime did not provide one.
fn lambda_deadline(deadline: SystemTime) -> Option<Instant> {
    if deadline == SystemTime::UNIX_EPOCH {
        return None;
    }
    let remaining = deadline
        .duration_since(SystemTime::now())
        .unwrap_or_default();
    Some(Instant::now() + remaining)
}

fn init_tracing() {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    let in_lambda = std::env::var_os("AWS_LAMBDA_FUNCTION_NAME").is_some();

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(!in_lambda)
        .compact()
        .init();
}
