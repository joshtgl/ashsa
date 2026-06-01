use crate::config::Config;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tracing::{debug, error, info};

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

    pub async fn handle_event(&self, event: Value) -> Value {
        info!("processing Alexa request");

        if self.config.debug {
            debug!(event = %sanitize_json(&event), "received Alexa event");
        }

        match self.handle_event_inner(event).await {
            Ok(response) => response,
            Err(HandlerError::InvalidRequest(err)) => {
                error!(error = %err, "invalid request");
                alexa_error("INVALID_REQUEST", err.to_string())
            }
            Err(HandlerError::Authorization(message)) => {
                error!(message, "downstream authorization failure");
                alexa_error("INVALID_AUTHORIZATION_CREDENTIAL", message)
            }
            Err(HandlerError::Internal(message)) => {
                error!(message, "internal handler error");
                alexa_error("INTERNAL_ERROR", message)
            }
        }
    }

    async fn handle_event_inner(&self, event: Value) -> Result<Value, HandlerError> {
        validate_payload_version(&event).map_err(HandlerError::InvalidRequest)?;
        let token = extract_token(&event, &self.config).map_err(HandlerError::InvalidRequest)?;
        let url = format!("{}/api/alexa/smart_home", self.config.base_url);

        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(&event)
            .send()
            .await
            .map_err(|err| {
                HandlerError::Internal(format!("failed to call downstream endpoint: {err}"))
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
            Err(HandlerError::Internal(format!(
                "downstream endpoint returned {}: {}",
                status.as_u16(),
                body
            )))
        }
    }
}

enum HandlerError {
    InvalidRequest(RequestError),
    Authorization(String),
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

    if let Some(token) = scope.get("token").and_then(Value::as_str) {
        return Ok(token);
    }

    if config.debug {
        if let Some(token) = config.fallback_bearer_token.as_deref() {
            return Ok(token);
        }
    }

    Err(RequestError::MissingToken)
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

    fn config(base_url: String) -> Config {
        Config {
            base_url,
            aws_default_region: Some("us-east-1".to_owned()),
            debug: false,
            insecure_skip_tls_verify: false,
            fallback_bearer_token: None,
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
    fn debug_mode_allows_fallback_token() {
        let event = sample_event(json!({
            "type": "BearerToken"
        }));
        let mut cfg = config("https://example.com".to_owned());
        cfg.debug = true;
        cfg.fallback_bearer_token = Some("fallback-token".to_owned());

        let token = extract_token(&event, &cfg).unwrap();
        assert_eq!(token, "fallback-token");
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
        let actual = app.handle_event(event).await;

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
        let actual = app.handle_event(event).await;

        mock.assert_async().await;
        assert_eq!(
            actual,
            alexa_error("INVALID_AUTHORIZATION_CREDENTIAL", "denied".to_owned())
        );
    }

    #[tokio::test]
    async fn downstream_server_error_maps_to_internal_error() {
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
        let actual = app.handle_event(event).await;

        mock.assert_async().await;
        assert_eq!(
            actual,
            alexa_error(
                "INTERNAL_ERROR",
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
        let actual = app.handle_event(event).await;

        mock.assert_async().await;
        assert!(
            actual["event"]["payload"]["message"]
                .as_str()
                .unwrap()
                .contains("downstream returned invalid JSON")
        );
    }

    #[tokio::test]
    async fn transport_failure_maps_to_internal_error() {
        let app = App::new(config("http://127.0.0.1:9".to_owned()), Client::new());
        let event = sample_event(json!({
            "type": "BearerToken",
            "token": "test-token"
        }));

        let actual = app.handle_event(event).await;

        assert_eq!(actual["event"]["payload"]["type"], "INTERNAL_ERROR");
        assert!(
            actual["event"]["payload"]["message"]
                .as_str()
                .unwrap()
                .contains("failed to call downstream endpoint")
        );
    }
}
