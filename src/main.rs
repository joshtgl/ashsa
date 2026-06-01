use ashsa::client::build_http_client;
use ashsa::config::Config;
use ashsa::handler::App;
use lambda_runtime::{Error, LambdaEvent, service_fn};
use serde_json::Value;
use std::sync::Arc;
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
        async move { Ok::<Value, Error>(app.handle_event(event.payload).await) }
    }))
    .await
}

fn init_tracing() {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .without_time()
        .compact()
        .init();
}
