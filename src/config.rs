use std::collections::HashMap;
use std::env;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub base_url: String,
    pub aws_default_region: Option<String>,
    pub debug: bool,
    pub insecure_skip_tls_verify: bool,
    pub fallback_bearer_token: Option<String>,
    pub max_retries: u32,
    pub request_budget: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    Missing(&'static str),
    InvalidBool { key: &'static str, value: String },
    InvalidNumber { key: &'static str, value: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(key) => write!(f, "missing required environment variable {key}"),
            Self::InvalidBool { key, value } => {
                write!(f, "invalid boolean value for {key}: {value}")
            }
            Self::InvalidNumber { key, value } => {
                write!(f, "invalid numeric value for {key}: {value}")
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
        let max_retries = parse_u64(vars, "MAX_RETRIES")?
            .map(|value| {
                u32::try_from(value).map_err(|_| ConfigError::InvalidNumber {
                    key: "MAX_RETRIES",
                    value: value.to_string(),
                })
            })
            .transpose()?
            .unwrap_or(2);
        let request_budget =
            Duration::from_millis(parse_u64(vars, "REQUEST_BUDGET_MS")?.unwrap_or(7000));

        Ok(Self {
            base_url,
            aws_default_region,
            debug,
            insecure_skip_tls_verify,
            fallback_bearer_token,
            max_retries,
            request_budget,
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

fn parse_u64(
    vars: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<u64>, ConfigError> {
    let Some(value) = vars.get(key) else {
        return Ok(None);
    };

    value
        .trim()
        .parse()
        .map(Some)
        .map_err(|_| ConfigError::InvalidNumber {
            key,
            value: value.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigError};
    use std::collections::HashMap;
    use std::time::Duration;

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
    fn debug_and_fallback_token_are_loaded_independently() {
        let config = Config::from_map(&vars(&[
            ("BASE_URL", "https://example.com"),
            ("AWS_DEFAULT_REGION", "us-east-1"),
            ("DEBUG", "0"),
            ("LONG_LIVED_ACCESS_TOKEN", "fallback-token"),
        ]))
        .unwrap();

        assert!(!config.debug);
        assert_eq!(
            config.fallback_bearer_token.as_deref(),
            Some("fallback-token")
        );
    }

    #[test]
    fn retry_settings_default_when_unset() {
        let config = Config::from_map(&vars(&[("BASE_URL", "https://example.com")])).unwrap();
        assert_eq!(config.max_retries, 2);
        assert_eq!(config.request_budget, Duration::from_millis(7000));
    }

    #[test]
    fn retry_settings_are_parsed() {
        let config = Config::from_map(&vars(&[
            ("BASE_URL", "https://example.com"),
            ("MAX_RETRIES", "0"),
            ("REQUEST_BUDGET_MS", "5000"),
        ]))
        .unwrap();

        assert_eq!(config.max_retries, 0);
        assert_eq!(config.request_budget, Duration::from_millis(5000));
    }

    #[test]
    fn invalid_retry_settings_are_rejected() {
        let err = Config::from_map(&vars(&[
            ("BASE_URL", "https://example.com"),
            ("MAX_RETRIES", "lots"),
        ]))
        .unwrap_err();
        assert_eq!(
            err,
            ConfigError::InvalidNumber {
                key: "MAX_RETRIES",
                value: "lots".to_owned()
            }
        );

        let err = Config::from_map(&vars(&[
            ("BASE_URL", "https://example.com"),
            ("REQUEST_BUDGET_MS", "-1"),
        ]))
        .unwrap_err();
        assert_eq!(
            err,
            ConfigError::InvalidNumber {
                key: "REQUEST_BUDGET_MS",
                value: "-1".to_owned()
            }
        );
    }
}
