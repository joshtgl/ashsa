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
5. In `Configuration` -> `Environment variables`, add the runtime settings described below.
6. Save and deploy the function, then point your Alexa Smart Home skill backend at the Lambda ARN.

## Configuration

The Lambda reads configuration from environment variables.

| Variable | Required | Description |
| --- | --- | --- |
| `BASE_URL` | Yes | Base URL for the downstream smart home endpoint. Trailing `/` is removed automatically. |
| `DEBUG` | No | Enables debug logging. |
| `NOT_VERIFY_SSL` | No | Disables TLS certificate verification. Intended only for controlled environments. |
| `LONG_LIVED_ACCESS_TOKEN` | No | Bearer token to use for downstream requests. When set, it overrides any token carried in the Alexa event. |

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
- Total request timeout: 10 seconds
- TLS: `rustls`
- Connection pooling: enabled through a shared `reqwest::Client`
