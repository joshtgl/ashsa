# ashsa

`ashsa` is an Alexa Smart Home Skill Adapter for Home Assistant. It is based on the python skill code
recommended by home assistant but modified to run as a single rust binary.

## Configuration

The Lambda reads configuration from environment variables.

| Variable | Required | Description |
| --- | --- | --- |
| `BASE_URL` | Yes | Base URL for the downstream smart home endpoint. Trailing `/` is removed automatically. |
| `DEBUG` | No | Enables debug logging. |
| `NOT_VERIFY_SSL` | No | Disables TLS certificate verification. Intended only for controlled environments. |
| `LONG_LIVED_ACCESS_TOKEN` | No | Fallback bearer token used when the request token is missing. |
| `AWS_DEFAULT_REGION` | No | Used to suffix the HTTP user agent string. |

## HTTP Behavior

- Connect timeout: 2 seconds
- Total request timeout: 10 seconds
- TLS: `rustls`
- Connection pooling: enabled through a shared `reqwest::Client`
