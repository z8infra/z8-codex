//! Native Z8 usage client.
//!
//! API keys are accepted only by this native module.  The renderer receives
//! the bounded, redacted usage snapshot returned by the Manager command.

use futures_util::StreamExt;
use reqwest::redirect::Policy;
use serde::Serialize;
use serde_json::{Map, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::z8_provisioning::Z8_BASE_URL;

// The usage response includes daily and per-model statistics.  A busy key can
// legitimately exceed the old 512 KiB limit even though the balance itself is
// small, which made the whole card appear unavailable.  Keep a bounded body,
// but match the gateway's one-megabyte usage response contract.
const MAX_RESPONSE_BYTES: usize = 1 << 20;
const MAX_TEXT_BYTES: usize = 128;
const MAX_API_KEY_BYTES: usize = 4096;
const USAGE_USER_AGENT: &str = "Z8-Codex-Usage/1";

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub schema_version: u32,
    pub mode: String,
    pub is_valid: Option<bool>,
    pub is_unlimited: bool,
    /// Status reported by the usage service (for example, `active`).
    ///
    /// This intentionally serializes as `accountStatus`: manager command
    /// responses flatten their payload into the same object as the command
    /// envelope, whose `status` field is reserved for `ok`/`failed`. Keeping
    /// the names distinct prevents a successful usage response from being
    /// mistaken for a failed command in the renderer.
    pub account_status: Option<String>,
    pub plan_name: Option<String>,
    pub unit: Option<String>,
    pub remaining: Option<f64>,
    pub balance: Option<f64>,
    pub quota: Option<QuotaSummary>,
    pub usage: Option<UsageSummary>,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaSummary {
    pub limit: Option<f64>,
    pub used: Option<f64>,
    pub remaining: Option<f64>,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub today: Option<UsageMetrics>,
    pub total: Option<UsageMetrics>,
    pub average_duration_ms: Option<f64>,
    pub rpm: Option<f64>,
    pub tpm: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetrics {
    pub requests: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub actual_cost: Option<f64>,
}

/// Fetch usage for an already selected native API key.
pub async fn fetch_with_api_key(mut api_key: String) -> Result<UsageSnapshot, &'static str> {
    if !api_key_is_valid(&api_key) {
        clear_sensitive_string(&mut api_key);
        return Err("usage_credential_invalid");
    }
    let url = format!("{}/usage", Z8_BASE_URL.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .redirect(Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            clear_sensitive_string(&mut api_key);
            return Err("usage_client_unavailable");
        }
    };
    // Keep the same explicit reporting window as Z8 Launch and the Sub2API
    // usage contract. Some gateway deployments return only a summary when the
    // `days` parameter is omitted, which leaves the detail card empty.
    let response = client
        .get(url)
        .query(&[("days", "30")])
        .bearer_auth(&api_key)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, USAGE_USER_AGENT)
        .send()
        .await;
    clear_sensitive_string(&mut api_key);
    let response = response.map_err(|error| {
        if error.is_timeout() {
            "usage_timeout"
        } else {
            "usage_unavailable"
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(map_http_status(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("usage_response_too_large");
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "usage_response_read_failed")?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("usage_response_too_large");
        }
        bytes.extend_from_slice(&chunk);
    }
    let parsed = serde_json::from_slice::<Value>(&bytes).map_err(|_| "usage_invalid_response");
    clear_bytes(&mut bytes);
    let parsed = parsed?;
    parse_usage_json(parsed)
}

fn parse_usage_json(value: Value) -> Result<UsageSnapshot, &'static str> {
    // The gateway normally returns the usage snapshot directly. Some Z8 edge
    // deployments wrap successful responses in `{ data: { ... } }`; accept
    // that envelope as well so a compatible upstream does not become an
    // unexplained "暂不可用" card in the Manager.
    let object = usage_object(&value).ok_or("usage_invalid_response")?;
    let mode = match text_field(object, "mode") {
        Some(mode) if matches!(mode.as_str(), "quota_limited" | "unrestricted") => mode,
        Some(_) => return Err("usage_invalid_response"),
        None if has_usage_shape(object) => {
            // Older Z8 gateway responses expose only balance/statistics. They
            // are still valid snapshots; infer the display mode from their
            // presence rather than turning a usable account into an error card.
            "unrestricted".to_string()
        }
        None => return Err("usage_invalid_response"),
    };
    let is_unlimited = is_unlimited_marker(first_field(
        object,
        &["remaining", "availableBalance", "available_balance"],
    )) || is_unlimited_marker(first_field(
        object,
        &["balance", "creditBalance", "credit_balance"],
    )) || field(object, "quota")
        .and_then(Value::as_object)
        .and_then(|quota| field(quota, "remaining"))
        .is_some_and(|value| is_unlimited_marker(Some(value)));
    Ok(UsageSnapshot {
        schema_version: 1,
        mode,
        is_valid: field(object, "isValid").and_then(Value::as_bool),
        is_unlimited,
        account_status: text_field(object, "status"),
        plan_name: text_field(object, "planName"),
        unit: text_field(object, "unit").or_else(|| text_field(object, "currency")),
        remaining: nonnegative_number(first_field(
            object,
            &[
                "remaining",
                "availableBalance",
                "available_balance",
                "creditsRemaining",
                "credits_remaining",
            ],
        )),
        balance: nonnegative_number(first_field(
            object,
            &[
                "balance",
                "creditBalance",
                "credit_balance",
                "availableBalance",
                "available_balance",
                "credits",
            ],
        )),
        quota: field(object, "quota").and_then(parse_quota),
        usage: field(object, "usage")
            .or_else(|| field(object, "statistics"))
            .and_then(parse_usage_summary)
            .or_else(|| parse_top_level_usage(object)),
        observed_at_ms: current_timestamp_ms(),
    })
}

fn usage_object(value: &Value) -> Option<&Map<String, Value>> {
    let mut current = value;
    for _ in 0..4 {
        let object = current.as_object()?;
        if has_usage_shape(object) {
            return Some(object);
        }
        current = object
            .get("data")
            .or_else(|| object.get("result"))
            .or_else(|| object.get("payload"))?;
    }
    None
}

fn has_usage_shape(object: &Map<String, Value>) -> bool {
    [
        "mode",
        "remaining",
        "balance",
        "availableBalance",
        "available_balance",
        "quota",
        "usage",
        "statistics",
        "requestCount",
        "request_count",
        "totalRequests",
        "total_requests",
        "totalTokens",
        "total_tokens",
        "actualCost",
        "actual_cost",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
}

fn parse_quota(value: &Value) -> Option<QuotaSummary> {
    let object = value.as_object()?;
    Some(QuotaSummary {
        limit: nonnegative_number(field(object, "limit")),
        used: nonnegative_number(field(object, "used")),
        remaining: nonnegative_number(field(object, "remaining")),
        unit: text_field(object, "unit"),
    })
}

fn parse_usage_summary(value: &Value) -> Option<UsageSummary> {
    let object = value.as_object()?;
    Some(UsageSummary {
        today: field(object, "today").and_then(parse_usage_metrics),
        total: field(object, "total").and_then(parse_usage_metrics),
        average_duration_ms: nonnegative_number(field(object, "average_duration_ms")),
        rpm: nonnegative_number(field(object, "rpm")),
        tpm: nonnegative_number(field(object, "tpm")),
    })
}

fn parse_top_level_usage(object: &Map<String, Value>) -> Option<UsageSummary> {
    let today = parse_usage_metrics_from_fields(
        object,
        &["todayRequests", "today_requests"],
        &["todayTokens", "today_tokens"],
        &[
            "todayCost",
            "today_cost",
            "todayActualCost",
            "today_actual_cost",
        ],
    );
    let total = parse_usage_metrics_from_fields(
        object,
        &[
            "totalRequests",
            "total_requests",
            "requestCount",
            "request_count",
        ],
        &["totalTokens", "total_tokens"],
        &["totalCost", "total_cost", "actualCost", "actual_cost"],
    );
    (today.is_some() || total.is_some()).then_some(UsageSummary {
        today,
        total,
        average_duration_ms: nonnegative_number(first_field(
            object,
            &["averageDurationMs", "average_duration_ms"],
        )),
        rpm: nonnegative_number(field(object, "rpm")),
        tpm: nonnegative_number(field(object, "tpm")),
    })
}

fn parse_usage_metrics_from_fields(
    object: &Map<String, Value>,
    request_keys: &[&str],
    token_keys: &[&str],
    cost_keys: &[&str],
) -> Option<UsageMetrics> {
    let requests = first_field(object, request_keys).and_then(|value| value.as_u64());
    let total_tokens = first_field(object, token_keys).and_then(|value| value.as_u64());
    let actual_cost = nonnegative_number(first_field(object, cost_keys));
    (requests.is_some() || total_tokens.is_some() || actual_cost.is_some()).then_some(
        UsageMetrics {
            requests: requests.filter(|value| *value <= 1_000_000_000_000_000),
            input_tokens: None,
            output_tokens: None,
            total_tokens: total_tokens.filter(|value| *value <= 1_000_000_000_000_000),
            cost: None,
            actual_cost,
        },
    )
}

fn parse_usage_metrics(value: &Value) -> Option<UsageMetrics> {
    let object = value.as_object()?;
    Some(UsageMetrics {
        requests: nonnegative_integer(field(object, "requests")),
        input_tokens: nonnegative_integer(field(object, "input_tokens")),
        output_tokens: nonnegative_integer(field(object, "output_tokens")),
        total_tokens: nonnegative_integer(field(object, "total_tokens")),
        cost: nonnegative_number(field(object, "cost")),
        actual_cost: nonnegative_number(field(object, "actual_cost")),
    })
}

fn field<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object.get(key).or_else(|| object.get(&to_snake_case(key)))
}

fn first_field<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| field(object, key))
}

fn to_snake_case(key: &str) -> String {
    key.chars().enumerate().fold(
        String::with_capacity(key.len() + 4),
        |mut output, (index, character)| {
            if character.is_ascii_uppercase() {
                if index > 0 {
                    output.push('_');
                }
                output.push(character.to_ascii_lowercase());
            } else {
                output.push(character);
            }
            output
        },
    )
}

fn text_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    let value = field(object, key)?.as_str()?;
    if value.is_empty() || value.len() > MAX_TEXT_BYTES || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.to_string())
}

fn nonnegative_number(value: Option<&Value>) -> Option<f64> {
    let number = value?.as_f64()?;
    (number.is_finite() && number >= 0.0 && number <= 1_000_000_000_000.0).then_some(number)
}

fn nonnegative_integer(value: Option<&Value>) -> Option<u64> {
    let number = value?.as_u64()?;
    (number <= 1_000_000_000_000_000).then_some(number)
}

fn is_unlimited_marker(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_f64)
        .is_some_and(|number| number == -1.0)
}

fn api_key_is_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_API_KEY_BYTES
        && value.is_ascii()
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

fn map_http_status(status: u16) -> &'static str {
    match status {
        401 | 403 => "usage_invalid_key",
        429 => "usage_rate_limited",
        408 | 500..=599 => "usage_unavailable",
        _ => "usage_request_failed",
    }
}

fn clear_sensitive_string(value: &mut String) {
    // SAFETY: zeroing UTF-8 bytes preserves validity for the final empty string.
    unsafe {
        for byte in value.as_mut_vec() {
            std::ptr::write_volatile(byte, 0);
        }
    }
    value.clear();
}

fn clear_bytes(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY: volatile writes prevent the optimizer from removing the clear.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quota_and_usage_without_secrets() {
        let snapshot = parse_usage_json(serde_json::json!({
            "mode": "quota_limited",
            "isValid": true,
            "remaining": 9.0,
            "quota": {"limit": 10.0, "used": 1.0, "remaining": 9.0, "unit": "USD"},
            "usage": {"today": {"requests": 1, "total_tokens": 30, "actual_cost": 0.01}, "total": {"requests": 12, "total_tokens": 340}}
        })).unwrap();
        assert_eq!(snapshot.remaining, Some(9.0));
        assert_eq!(
            snapshot
                .quota
                .as_ref()
                .and_then(|quota| quota.unit.as_deref()),
            Some("USD")
        );
        assert_eq!(
            snapshot
                .usage
                .as_ref()
                .and_then(|usage| usage.today.as_ref())
                .and_then(|today| today.requests),
            Some(1)
        );
        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains("api_key"));
    }

    #[test]
    fn supports_unrestricted_balance_and_unlimited_marker() {
        let balance =
            parse_usage_json(serde_json::json!({"mode": "unrestricted", "balance": 4.2})).unwrap();
        assert_eq!(balance.balance, Some(4.2));
        let unlimited =
            parse_usage_json(serde_json::json!({"mode": "unrestricted", "remaining": -1})).unwrap();
        assert!(unlimited.is_unlimited);
        assert_eq!(unlimited.remaining, None);
    }

    #[test]
    fn rejects_unknown_mode_and_unsafe_values() {
        assert!(parse_usage_json(serde_json::json!({"mode": "other"})).is_err());
        let snapshot = parse_usage_json(serde_json::json!({
            "mode": "quota_limited",
            "remaining": -4,
            "usage": {"today": {"requests": -1, "actual_cost": "bad"}}
        }))
        .unwrap();
        assert_eq!(snapshot.remaining, None);
        assert_eq!(snapshot.usage.unwrap().today.unwrap().requests, None);
    }

    #[test]
    fn accepts_a_success_envelope_from_compatible_gateway_edges() {
        let snapshot = parse_usage_json(serde_json::json!({
            "code": 0,
            "data": {
                "mode": "unrestricted",
                "balance": 2.02,
                "unit": "USD"
            }
        }))
        .unwrap();
        assert_eq!(snapshot.balance, Some(2.02));
    }

    #[test]
    fn accepts_plain_balance_and_top_level_statistics_without_mode() {
        let snapshot = parse_usage_json(serde_json::json!({
            "available_balance": 2.02,
            "today_requests": 3,
            "today_tokens": 120,
            "today_actual_cost": 0.04,
            "total_requests": 19,
            "total_tokens": 900,
            "actual_cost": 0.31,
            "currency": "USD"
        }))
        .unwrap();
        assert_eq!(snapshot.mode, "unrestricted");
        assert_eq!(snapshot.remaining, Some(2.02));
        assert_eq!(snapshot.unit.as_deref(), Some("USD"));
        assert_eq!(
            snapshot
                .usage
                .as_ref()
                .and_then(|usage| usage.today.as_ref())
                .and_then(|today| today.requests),
            Some(3)
        );
        assert_eq!(
            snapshot
                .usage
                .as_ref()
                .and_then(|usage| usage.total.as_ref())
                .and_then(|total| total.total_tokens),
            Some(900)
        );
    }

    #[test]
    fn accepts_nested_payload_envelope() {
        let snapshot = parse_usage_json(serde_json::json!({
            "code": 200,
            "payload": {"data": {"balance": 1.5}}
        }))
        .unwrap();
        assert_eq!(snapshot.balance, Some(1.5));
    }

    #[test]
    fn maps_http_errors_and_uses_the_configured_usage_path() {
        assert_eq!(map_http_status(401), "usage_invalid_key");
        assert_eq!(map_http_status(429), "usage_rate_limited");
        assert_eq!(map_http_status(503), "usage_unavailable");
        assert_eq!(map_http_status(418), "usage_request_failed");
        assert_eq!(
            format!("{}/usage", Z8_BASE_URL.trim_end_matches('/')),
            "https://z8.hk/v1/usage"
        );
    }
}
