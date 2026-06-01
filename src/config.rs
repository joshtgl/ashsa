use std::collections::HashMap;
use std::env;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub base_url: String,
    pub aws_default_region: Option<String>,
    pub debug: bool,
    pub insecure_skip_tls_verify: bool,
    pub fallback_bearer_token: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    Missing(&'static str),
    InvalidBool { key: &'static str, value: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(key) => write!(f, "missing required environment variable {key}"),
            Self::InvalidBool { key, value } => {
                write!(f, "invalid boolean value for {key}: {value}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let vars = env::vars().collect::<HashMap<_, _>>();
        Self::from_map(&vars)
    }

    pub fn from_map(vars: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let base_url = vars
            .get("BASE_URL")
            .map(|value| value.trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty())
            .ok_or(ConfigError::Missing("BASE_URL"))?;
        let aws_default_region = vars
            .get("AWS_DEFAULT_REGION")
            .filter(|value| !value.is_empty())
            .cloned();

        let debug = parse_bool(vars, "DEBUG")?.unwrap_or(false);
        let insecure_skip_tls_verify = parse_bool(vars, "NOT_VERIFY_SSL")?.unwrap_or(false);
        let fallback_bearer_token = vars
            .get("LONG_LIVED_ACCESS_TOKEN")
            .filter(|value| !value.is_empty())
            .cloned();

        Ok(Self {
            base_url,
            aws_default_region,
            debug,
            insecure_skip_tls_verify,
            fallback_bearer_token,
        })
    }
}

fn parse_bool(
    vars: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<bool>, ConfigError> {
    let Some(value) = vars.get(key) else {
        return Ok(None);
    };

    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => Err(ConfigError::InvalidBool {
            key,
            value: value.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigError};
    use std::collections::HashMap;

    fn vars(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn missing_base_url_is_rejected() {
        let err = Config::from_map(&vars(&[])).unwrap_err();
        assert_eq!(err, ConfigError::Missing("BASE_URL"));
    }

    #[test]
    fn missing_aws_region_is_allowed() {
        let config = Config::from_map(&vars(&[("BASE_URL", "https://example.com")])).unwrap();
        assert_eq!(config.aws_default_region, None);
    }

    #[test]
    fn tls_verification_flag_is_parsed() {
        let config = Config::from_map(&vars(&[
            ("BASE_URL", "https://example.com/"),
            ("AWS_DEFAULT_REGION", "us-east-1"),
            ("NOT_VERIFY_SSL", "true"),
        ]))
        .unwrap();

        assert_eq!(config.base_url, "https://example.com");
        assert_eq!(config.aws_default_region.as_deref(), Some("us-east-1"));
        assert!(config.insecure_skip_tls_verify);
    }

    #[test]
    fn debug_and_fallback_token_are_loaded() {
        let config = Config::from_map(&vars(&[
            ("BASE_URL", "https://example.com"),
            ("AWS_DEFAULT_REGION", "us-east-1"),
            ("DEBUG", "1"),
            ("LONG_LIVED_ACCESS_TOKEN", "fallback-token"),
        ]))
        .unwrap();

        assert!(config.debug);
        assert_eq!(
            config.fallback_bearer_token.as_deref(),
            Some("fallback-token")
        );
    }
}
