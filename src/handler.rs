use crate::config::Config;
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info, warn};

/// Time reserved before the Lambda deadline so a response can still be returned.
const DEADLINE_MARGIN: Duration = Duration::from_millis(500);
const BASE_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(1);
/// Minimum time left after a backoff for another attempt to be worthwhile.
const MIN_ATTEMPT_TIME: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct App {
    config: Config,
    client: Client,
}

#[derive(Debug, PartialEq, Eq)]
enum RequestError {
    MissingDirective,
    UnsupportedPayloadVersion(String),
    MissingScope,
    UnsupportedScopeType(String),
    MissingToken,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDirective => write!(f, "request missing required directive field"),
            Self::UnsupportedPayloadVersion(version) => {
                write!(f, "only payloadVersion 3 is supported, got {version}")
            }
            Self::MissingScope => write!(f, "request missing scope in endpoint or payload"),
            Self::UnsupportedScopeType(scope_type) => {
                write!(f, "only BearerToken scope is supported, got {scope_type}")
            }
            Self::MissingToken => write!(f, "authentication token is required"),
        }
    }
}

impl std::error::Error for RequestError {}

impl App {
    pub fn new(config: Config, client: Client) -> Self {
        Self { config, client }
    }

    /// Handles an Alexa event. `deadline` is the Lambda invocation deadline, if known;
    /// downstream retries never run past it (less a small margin) or past the
    /// configured request budget.
    pub async fn handle_event(&self, event: Value, deadline: Option<Instant>) -> Value {
        info!("processing Alexa request");

        if self.config.debug {
            debug!(event = %sanitize_json(&event), "received Alexa event");
        }

        let deadline = self.effective_deadline(deadline);

        match self.handle_event_inner(event, deadline).await {
            Ok(response) => response,
            Err(HandlerError::InvalidRequest(err)) => {
                error!(error = %err, "invalid request");
                alexa_error("INVALID_REQUEST", err.to_string())
            }
            Err(HandlerError::Authorization(message)) => {
                error!(message, "downstream authorization failure");
                alexa_error("INVALID_AUTHORIZATION_CREDENTIAL", message)
            }
            Err(HandlerError::Unreachable(message)) => {
                error!(message, "downstream endpoint unreachable");
                alexa_error("ENDPOINT_UNREACHABLE", message)
            }
            Err(HandlerError::Internal(message)) => {
                error!(message, "internal handler error");
                alexa_error("INTERNAL_ERROR", message)
            }
        }
    }

    fn effective_deadline(&self, lambda_deadline: Option<Instant>) -> Instant {
        let budget_deadline = Instant::now() + self.config.request_budget;
        match lambda_deadline.and_then(|deadline| deadline.checked_sub(DEADLINE_MARGIN)) {
            Some(deadline) => deadline.min(budget_deadline),
            None => budget_deadline,
        }
    }

    async fn handle_event_inner(
        &self,
        event: Value,
        deadline: Instant,
    ) -> Result<Value, HandlerError> {
        validate_payload_version(&event).map_err(HandlerError::InvalidRequest)?;
        let token = extract_token(&event, &self.config).map_err(HandlerError::InvalidRequest)?;
        let url = format!("{}/api/alexa/smart_home", self.config.base_url);

        let response = self
            .send_with_retries(&url, token, &event, deadline)
            .await
            .map_err(|err| {
                HandlerError::Unreachable(format!("failed to call downstream endpoint: {err}"))
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|err| {
            HandlerError::Internal(format!("failed to read downstream response: {err}"))
        })?;

        debug!(status = status.as_u16(), "received downstream response");

        if status.is_success() {
            serde_json::from_str(&body).map_err(|err| {
                HandlerError::Internal(format!("downstream returned invalid JSON: {err}"))
            })
        } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            Err(HandlerError::Authorization(body))
        } else {
            let message = format!("downstream endpoint returned {}: {}", status.as_u16(), body);
            if status.is_server_error() {
                Err(HandlerError::Unreachable(message))
            } else {
                Err(HandlerError::Internal(message))
            }
        }
    }

    /// Sends the request, retrying only failures where the request most likely never
    /// reached Home Assistant, since some directives (e.g. AdjustBrightness) are not
    /// idempotent.
    async fn send_with_retries(
        &self,
        url: &str,
        token: &str,
        event: &Value,
        deadline: Instant,
    ) -> Result<Response, reqwest::Error> {
        let mut attempt = 0;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let result = self
                .client
                .post(url)
                .bearer_auth(token)
                .json(event)
                .timeout(remaining.max(Duration::from_millis(1)))
                .send()
                .await;

            if attempt >= self.config.max_retries || !is_retryable(&result) {
                return result;
            }

            let backoff = backoff_delay(attempt);
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining < backoff + MIN_ATTEMPT_TIME {
                debug!(attempt, "request budget exhausted, not retrying");
                return result;
            }

            attempt += 1;
            match &result {
                Ok(response) => warn!(
                    attempt,
                    status = response.status().as_u16(),
                    backoff_ms = backoff.as_millis() as u64,
                    "retrying downstream request"
                ),
                Err(err) => warn!(
                    attempt,
                    error = %err,
                    backoff_ms = backoff.as_millis() as u64,
                    "retrying downstream request"
                ),
            }
            tokio::time::sleep(backoff).await;
        }
    }
}

fn is_retryable(result: &Result<Response, reqwest::Error>) -> bool {
    match result {
        Ok(response) => matches!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS
                | StatusCode::BAD_GATEWAY
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::GATEWAY_TIMEOUT
        ),
        Err(err) => err.is_connect(),
    }
}

/// Exponential backoff capped at `MAX_BACKOFF`, with up to 50% jitter added.
fn backoff_delay(attempt: u32) -> Duration {
    let base = BASE_BACKOFF
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(MAX_BACKOFF);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    let jitter_range = base.as_millis() as u64 / 2 + 1;
    base + Duration::from_millis(u64::from(nanos) % jitter_range)
}

enum HandlerError {
    InvalidRequest(RequestError),
    Authorization(String),
    Unreachable(String),
    Internal(String),
}

fn validate_payload_version(event: &Value) -> Result<(), RequestError> {
    let directive = event
        .get("directive")
        .ok_or(RequestError::MissingDirective)?;

    let payload_version = directive
        .get("header")
        .and_then(Value::as_object)
        .and_then(|header| header.get("payloadVersion"))
        .and_then(Value::as_str)
        .ok_or_else(|| RequestError::UnsupportedPayloadVersion("missing".to_owned()))?;

    if payload_version == "3" {
        Ok(())
    } else {
        Err(RequestError::UnsupportedPayloadVersion(
            payload_version.to_owned(),
        ))
    }
}

fn extract_token<'a>(event: &'a Value, config: &'a Config) -> Result<&'a str, RequestError> {
    let scope = validated_scope(event)?;

    if let Some(token) = config.fallback_bearer_token.as_deref() {
        info!("Using long lived token");
        return Ok(token);
    }

    if let Some(token) = scope.get("token").and_then(Value::as_str) {
        info!("Using token from event");
        return Ok(token);
    }

    Err(RequestError::MissingToken)
}

fn validated_scope(event: &Value) -> Result<&Value, RequestError> {
    let directive = event
        .get("directive")
        .ok_or(RequestError::MissingDirective)?;

    let scope = directive
        .get("endpoint")
        .and_then(|endpoint| endpoint.get("scope"))
        .or_else(|| {
            directive
                .get("payload")
                .and_then(|payload| payload.get("grantee"))
        })
        .or_else(|| {
            directive
                .get("payload")
                .and_then(|payload| payload.get("scope"))
        })
        .ok_or(RequestError::MissingScope)?;

    let scope_type = scope
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| RequestError::UnsupportedScopeType("missing".to_owned()))?;

    if scope_type != "BearerToken" {
        return Err(RequestError::UnsupportedScopeType(scope_type.to_owned()));
    }

    Ok(scope)
}

fn alexa_error(error_type: &str, message: String) -> Value {
    json!({
        "event": {
            "payload": {
                "type": error_type,
                "message": message,
            }
        }
    })
}

fn sanitize_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sanitized = serde_json::Map::with_capacity(map.len());
            for (key, value) in map {
                if key.eq_ignore_ascii_case("token") {
                    sanitized.insert(key.clone(), Value::String("<redacted>".to_owned()));
                } else {
                    sanitized.insert(key.clone(), sanitize_json(value));
                }
            }
            Value::Object(sanitized)
        }
        Value::Array(values) => Value::Array(values.iter().map(sanitize_json).collect()),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{App, RequestError, alexa_error, extract_token, validate_payload_version};
    use crate::client::build_http_client;
    use crate::config::Config;
    use mockito::{Matcher, Server};
    use reqwest::Client;
    use serde_json::{Value, json};
    use std::time::{Duration, Instant};

    fn config(base_url: String) -> Config {
        Config {
            base_url,
            aws_default_region: Some("us-east-1".to_owned()),
            debug: false,
            insecure_skip_tls_verify: false,
            fallback_bearer_token: None,
            max_retries: 2,
            request_budget: Duration::from_secs(2),
        }
    }

    fn sample_event(scope: Value) -> Value {
        json!({
            "directive": {
                "header": {
                    "payloadVersion": "3"
                },
                "endpoint": {
                    "scope": scope
                }
            }
        })
    }

    #[test]
    fn payload_version_must_be_three() {
        let err = validate_payload_version(&json!({
            "directive": {
                "header": {
                    "payloadVersion": "2"
                }
            }
        }))
        .unwrap_err();

        assert_eq!(err, RequestError::UnsupportedPayloadVersion("2".to_owned()));
    }

    #[test]
    fn extracts_endpoint_scope_token() {
        let event = sample_event(json!({
            "type": "BearerToken",
            "token": "endpoint-token"
        }));

        let cfg = config("https://example.com".to_owned());
        let token = extract_token(&event, &cfg).unwrap();
        assert_eq!(token, "endpoint-token");
    }

    #[test]
    fn long_lived_token_takes_precedence_over_event_token() {
        let event = sample_event(json!({
            "type": "BearerToken",
            "token": "endpoint-token"
        }));

        let mut cfg = config("https://example.com".to_owned());
        cfg.fallback_bearer_token = Some("fallback-token".to_owned());

        let token = extract_token(&event, &cfg).unwrap();
        assert_eq!(token, "fallback-token");
    }

    #[test]
    fn extracts_linking_grantee_token() {
        let event = json!({
            "directive": {
                "header": {
                    "payloadVersion": "3"
                },
                "payload": {
                    "grantee": {
                        "type": "BearerToken",
                        "token": "grantee-token"
                    }
                }
            }
        });

        let cfg = config("https://example.com".to_owned());
        let token = extract_token(&event, &cfg).unwrap();
        assert_eq!(token, "grantee-token");
    }

    #[test]
    fn extracts_discovery_scope_token() {
        let event = json!({
            "directive": {
                "header": {
                    "payloadVersion": "3"
                },
                "payload": {
                    "scope": {
                        "type": "BearerToken",
                        "token": "discovery-token"
                    }
                }
            }
        });

        let cfg = config("https://example.com".to_owned());
        let token = extract_token(&event, &cfg).unwrap();
        assert_eq!(token, "discovery-token");
    }

    #[test]
    fn missing_directive_is_rejected() {
        let err = validate_payload_version(&json!({})).unwrap_err();
        assert_eq!(err, RequestError::MissingDirective);
    }

    #[test]
    fn unsupported_scope_type_is_rejected() {
        let event = sample_event(json!({
            "type": "AccessToken",
            "token": "bad-token"
        }));

        let err = extract_token(&event, &config("https://example.com".to_owned())).unwrap_err();
        assert_eq!(
            err,
            RequestError::UnsupportedScopeType("AccessToken".to_owned())
        );
    }

    #[test]
    fn fallback_token_is_used_even_when_debug_is_disabled() {
        let event = sample_event(json!({
            "type": "BearerToken"
        }));
        let mut cfg = config("https://example.com".to_owned());
        cfg.fallback_bearer_token = Some("fallback-token".to_owned());

        let token = extract_token(&event, &cfg).unwrap();
        assert_eq!(token, "fallback-token");
    }

    #[test]
    fn fallback_token_still_requires_bearer_scope_type() {
        let event = sample_event(json!({
            "type": "AccessToken"
        }));
        let mut cfg = config("https://example.com".to_owned());
        cfg.fallback_bearer_token = Some("fallback-token".to_owned());

        let err = extract_token(&event, &cfg).unwrap_err();
        assert_eq!(
            err,
            RequestError::UnsupportedScopeType("AccessToken".to_owned())
        );
    }

    #[tokio::test]
    async fn successful_downstream_json_is_passed_through() {
        let mut server = Server::new_async().await;
        let response_body = json!({"event": {"header": {"name": "Response"}}});
        let event = sample_event(json!({
            "type": "BearerToken",
            "token": "test-token"
        }));

        let mock = server
            .mock("POST", "/api/alexa/smart_home")
            .match_header("authorization", "Bearer test-token")
            .match_header(
                "content-type",
                Matcher::Regex("application/json.*".to_owned()),
            )
            .match_header("user-agent", "Alexa Smart Home Skill Adapter - us-east-1")
            .with_status(200)
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let cfg = config(server.url());
        let client = build_http_client(&cfg).unwrap();
        let app = App::new(cfg, client);
        let actual = app.handle_event(event, None).await;

        mock.assert_async().await;
        assert_eq!(actual, response_body);
    }

    #[tokio::test]
    async fn downstream_request_uses_long_lived_token_when_configured() {
        let mut server = Server::new_async().await;
        let response_body = json!({"event": {"header": {"name": "Response"}}});
        let event = sample_event(json!({
            "type": "BearerToken",
            "token": "event-token"
        }));

        let mock = server
            .mock("POST", "/api/alexa/smart_home")
            .match_header("authorization", "Bearer fallback-token")
            .with_status(200)
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let mut cfg = config(server.url());
        cfg.fallback_bearer_token = Some("fallback-token".to_owned());
        let client = build_http_client(&cfg).unwrap();
        let app = App::new(cfg, client);
        let actual = app.handle_event(event, None).await;

        mock.assert_async().await;
        assert_eq!(actual, response_body);
    }

    #[tokio::test]
    async fn downstream_auth_failure_maps_to_alexa_error() {
        let mut server = Server::new_async().await;
        let event = sample_event(json!({
            "type": "BearerToken",
            "token": "bad-token"
        }));

        let mock = server
            .mock("POST", "/api/alexa/smart_home")
            .with_status(401)
            .with_body("denied")
            .create_async()
            .await;

        let app = App::new(config(server.url()), Client::new());
        let actual = app.handle_event(event, None).await;

        mock.assert_async().await;
        assert_eq!(
            actual,
            alexa_error("INVALID_AUTHORIZATION_CREDENTIAL", "denied".to_owned())
        );
    }

    #[tokio::test]
    async fn downstream_server_error_maps_to_endpoint_unreachable() {
        let mut server = Server::new_async().await;
        let event = sample_event(json!({
            "type": "BearerToken",
            "token": "test-token"
        }));

        let mock = server
            .mock("POST", "/api/alexa/smart_home")
            .with_status(500)
            .with_body("boom")
            .create_async()
            .await;

        let app = App::new(config(server.url()), Client::new());
        let actual = app.handle_event(event, None).await;

        mock.assert_async().await;
        assert_eq!(
            actual,
            alexa_error(
                "ENDPOINT_UNREACHABLE",
                "downstream endpoint returned 500: boom".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn invalid_downstream_json_maps_to_internal_error() {
        let mut server = Server::new_async().await;
        let event = sample_event(json!({
            "type": "BearerToken",
            "token": "test-token"
        }));

        let mock = server
            .mock("POST", "/api/alexa/smart_home")
            .with_status(200)
            .with_body("not-json")
            .create_async()
            .await;

        let app = App::new(config(server.url()), Client::new());
        let actual = app.handle_event(event, None).await;

        mock.assert_async().await;
        assert!(
            actual["event"]["payload"]["message"]
                .as_str()
                .unwrap()
                .contains("downstream returned invalid JSON")
        );
    }

    #[tokio::test]
    async fn transport_failure_maps_to_endpoint_unreachable() {
        let app = App::new(config("http://127.0.0.1:9".to_owned()), Client::new());
        let event = sample_event(json!({
            "type": "BearerToken",
            "token": "test-token"
        }));

        let actual = app.handle_event(event, None).await;

        assert_eq!(actual["event"]["payload"]["type"], "ENDPOINT_UNREACHABLE");
        assert!(
            actual["event"]["payload"]["message"]
                .as_str()
                .unwrap()
                .contains("failed to call downstream endpoint")
        );
    }

    fn bearer_event() -> Value {
        sample_event(json!({
            "type": "BearerToken",
            "token": "test-token"
        }))
    }

    #[tokio::test]
    async fn service_unavailable_is_retried_until_success() {
        let mut server = Server::new_async().await;
        let response_body = json!({"event": {"header": {"name": "Response"}}});

        let unavailable = server
            .mock("POST", "/api/alexa/smart_home")
            .with_status(503)
            .expect(1)
            .create_async()
            .await;
        let success = server
            .mock("POST", "/api/alexa/smart_home")
            .with_status(200)
            .with_body(response_body.to_string())
            .expect(1)
            .create_async()
            .await;

        let app = App::new(config(server.url()), Client::new());
        let actual = app.handle_event(bearer_event(), None).await;

        unavailable.assert_async().await;
        success.assert_async().await;
        assert_eq!(actual, response_body);
    }

    #[tokio::test]
    async fn internal_server_error_is_not_retried() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/api/alexa/smart_home")
            .with_status(500)
            .with_body("boom")
            .expect(1)
            .create_async()
            .await;

        let app = App::new(config(server.url()), Client::new());
        app.handle_event(bearer_event(), None).await;

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn retries_stop_after_max_retries() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/api/alexa/smart_home")
            .with_status(502)
            .with_body("bad gateway")
            .expect(3)
            .create_async()
            .await;

        let app = App::new(config(server.url()), Client::new());
        let actual = app.handle_event(bearer_event(), None).await;

        mock.assert_async().await;
        assert_eq!(
            actual,
            alexa_error(
                "ENDPOINT_UNREACHABLE",
                "downstream endpoint returned 502: bad gateway".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn exhausted_budget_prevents_retry() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/api/alexa/smart_home")
            .with_status(503)
            .expect(1)
            .create_async()
            .await;

        let mut cfg = config(server.url());
        cfg.request_budget = Duration::from_millis(200);
        let app = App::new(cfg, Client::new());
        app.handle_event(bearer_event(), None).await;

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn lambda_deadline_limits_retries() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/api/alexa/smart_home")
            .with_status(503)
            .expect(1)
            .create_async()
            .await;

        let app = App::new(config(server.url()), Client::new());
        let deadline = Instant::now() + Duration::from_millis(700);
        app.handle_event(bearer_event(), Some(deadline)).await;

        mock.assert_async().await;
    }
}
