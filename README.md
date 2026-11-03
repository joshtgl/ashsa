# ashsa

`ashsa` is an Alexa Smart Home Skill Adapter for Home Assistant. It is based on the python skill code
recommended by home assistant but modified to run as a single rust binary.

## Deploying to AWS Lambda

1. Build or download a Lambda package for the architecture you want to run (recommended to arm64):
   - `x86_64`: `ashsa-lambda-x86_64.zip`
   - `arm64`: `ashsa-lambda-arm64.zip`
2. In the AWS Lambda console, create a new function with these settings:
   - `Author from scratch`
   - Runtime: `Provide your own bootstrap on Amazon Linux 2023` (`provided.al2023`)
   - Architecture: match the zip you are uploading (`x86_64` or `arm64`)
   - Execution role: a role with at least CloudWatch Logs permissions
3. Upload the release zip for the matching architecture as the function code package.
4. If AWS asks for a handler value, use `bootstrap`.
5. In `Configuration` -> `General configuration`, set the timeout to `8` seconds. The Lambda default of
   3 seconds is too short for downstream retries, and Alexa itself gives up after roughly 8 seconds.
6. In `Configuration` -> `Environment variables`, add the runtime settings described below.
7. Save and deploy the function, then point your Alexa Smart Home skill backend at the Lambda ARN.

## Configuration

The Lambda reads configuration from environment variables.

| Variable | Required | Description |
| --- | --- | --- |
| `BASE_URL` | Yes | Base URL for the downstream smart home endpoint. Trailing `/` is removed automatically. |
| `DEBUG` | No | Enables debug logging. |
| `NOT_VERIFY_SSL` | No | Disables TLS certificate verification. Intended only for controlled environments. |
| `LONG_LIVED_ACCESS_TOKEN` | No | Bearer token to use for downstream requests. When set, it overrides any token carried in the Alexa event. |
| `MAX_RETRIES` | No | Maximum number of retries for a failed downstream request. Defaults to `2`. Set to `0` to disable retries. |
| `REQUEST_BUDGET_MS` | No | Total time in milliseconds allowed for all downstream attempts combined. Defaults to `7000`. |

### Example Lambda Environment

Use environment variables like the following in the Lambda configuration:

```text
BASE_URL=https://homeassistant.example.com
LONG_LIVED_ACCESS_TOKEN=your-home-assistant-token
DEBUG=true
NOT_VERIFY_SSL=false
```

`BASE_URL` is required. `LONG_LIVED_ACCESS_TOKEN` is optional if the Alexa event already contains the token you want to forward downstream.

## HTTP Behavior

- Connect timeout: 2 seconds
- Total time budget: `REQUEST_BUDGET_MS` (default 7 seconds), shared across all attempts and further
  limited by the Lambda invocation deadline (minus a 500 ms margin to return a response)
- TLS: `rustls`
- Connection pooling: enabled through a shared `reqwest::Client`

### Retries

Some Alexa directives are not idempotent (for example `AdjustBrightness` applies a relative change), so
requests are only retried when they most likely never reached Home Assistant:

- Connection failures (refused, DNS, TLS handshake, connect timeout)
- HTTP `429`, `502`, `503`, and `504` responses

Read timeouts and other `5xx` responses are not retried. Retries use exponential backoff starting at
100 ms (capped at 1 second, with jitter), and stop early if the remaining time budget is too small for
another attempt.

### Error Mapping

| Downstream result | Alexa error type |
| --- | --- |
| `401` / `403` | `INVALID_AUTHORIZATION_CREDENTIAL` |
| Connection failure, timeout, or `5xx` | `ENDPOINT_UNREACHABLE` |
| Invalid JSON or other unexpected status | `INTERNAL_ERROR` |
