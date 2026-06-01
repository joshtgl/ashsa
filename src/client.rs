use crate::config::Config;
use reqwest::Client;
use std::time::Duration;

pub fn build_http_client(config: &Config) -> Result<Client, reqwest::Error> {
    let user_agent = match config.aws_default_region.as_deref() {
        Some(region) => format!("Alexa Smart Home Skill Adapter - {region}"),
        None => "Alexa Smart Home Skill Adapter".to_owned(),
    };

    reqwest::Client::builder()
        .use_rustls_tls()
        .pool_idle_timeout(Duration::from_secs(90))
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(10))
        .user_agent(user_agent)
        .danger_accept_invalid_certs(config.insecure_skip_tls_verify)
        .build()
}
