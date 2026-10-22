//! Provider account / quota snapshots from `account.connectors`.
//!
//! CCR's `http-json` connector is a mapping description, not code: an endpoint,
//! an auth mode, and per-meter expressions such as
//! `"100 - $.usage.rolling.percent"`. We evaluate that small grammar instead of
//! hard-coding OpenCode Go's shape. A fetch failure returns the cache with an
//! error status; it never invents a number.

use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::app::App;
use crate::store::CredentialRow;

const CACHE_SECONDS: i64 = 300;
const MAX_CREDENTIALS: usize = 6;

pub async fn snapshots(app: &App, provider_filter: Option<&str>, force: bool) -> Vec<Value> {
    let providers = match app.store.providers() {
        Ok(providers) => providers,
        Err(error) => {
            tracing::warn!("quota: cannot list providers: {error}");
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for provider in providers {
        if !provider.enabled {
            continue;
        }
        if let Some(wanted) = provider_filter {
            if !wanted.eq_ignore_ascii_case(&provider.name)
                && !wanted.eq_ignore_ascii_case(&provider.id)
            {
                continue;
            }
        }
        let raw: Value = serde_json::from_str(&provider.raw_json).unwrap_or(Value::Null);
        let Some(account) = raw.get("account").filter(|value| !value.is_null()) else {
            continue;
        };
        if account.get("enabled").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let Some(connectors) = account.get("connectors").and_then(Value::as_array) else {
            continue;
        };

        let credentials = match app.store.credentials_for(&provider.id) {
            Ok(credentials) => credentials,
            Err(error) => {
                tracing::warn!("quota: credentials for {}: {error}", provider.name);
                continue;
            }
        };
        let mut credentials = credentials;
        if credentials.is_empty() {
            if let Ok(Some(key)) = app.store.fallback_key(&provider.id) {
                credentials.push(CredentialRow {
                    id: format!("{}:default", provider.id),
                    provider_id: provider.id.clone(),
                    name: "default".to_string(),
                    api_key: key,
                    priority: 0,
                    weight: 1,
                });
            }
        }
        if credentials.is_empty() {
            continue;
        }

        for connector in connectors {
            if connector.get("type").and_then(Value::as_str) != Some("http-json") {
                continue;
            }
            let Some(endpoint) = connector.get("endpoint").and_then(Value::as_str) else {
                continue;
            };
            let meters_spec = connector
                .get("mapping")
                .and_then(|mapping| mapping.get("meters"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if meters_spec.is_empty() {
                continue;
            }

            for credential in credentials.iter().take(MAX_CREDENTIALS) {
                let cached = app.store.quota_snapshot(&credential.id).ok().flatten();
                if !force {
                    if let Some((meters, fetched_at)) = &cached {
                        if fresh(fetched_at) {
                            out.push(snapshot(
                                &provider.name,
                                credential,
                                "ok",
                                Some(meters.clone()),
                                None,
                                Some(fetched_at.clone()),
                            ));
                            continue;
                        }
                    }
                }

                match app.upstream.get_json(endpoint, &credential.api_key).await {
                    Ok(body) => {
                        let meters: Vec<Value> =
                            meters_spec.iter().map(|spec| meter(spec, &body)).collect();
                        let meters = Value::Array(meters);
                        let _ = app.store.save_quota_snapshot(&credential.id, &meters);
                        out.push(snapshot(
                            &provider.name,
                            credential,
                            "ok",
                            Some(meters),
                            None,
                            Some(crate::config::now()),
                        ));
                    }
                    Err(error) => {
                        tracing::warn!(
                            "quota: {} / {} failed: {}",
                            provider.name,
                            credential.name,
                            error.message
                        );
                        let (meters, fetched_at) = cached
                            .map(|(meters, at)| (Some(meters), Some(at)))
                            .unwrap_or((None, None));
                        out.push(snapshot(
                            &provider.name,
                            credential,
                            "error",
                            meters,
                            Some(error.message),
                            fetched_at,
                        ));
                    }
                }
            }
        }
    }
    out
}

fn snapshot(
    provider: &str,
    credential: &CredentialRow,
    status: &str,
    meters: Option<Value>,
    message: Option<String>,
    updated_at: Option<String>,
) -> Value {
    let mut value = json!({
        "provider": provider,
        "credentialId": credential.id,
        "credentialLabel": credential.name,
        "status": status,
        "source": "http-json",
    });
    if let Some(meters) = meters {
        value["meters"] = meters;
    }
    if let Some(message) = message {
        value["message"] = Value::String(message);
    }
    if let Some(updated_at) = updated_at {
        value["updatedAt"] = Value::String(updated_at);
    }
    value
}

fn fresh(fetched_at: &str) -> bool {
    let Ok(then) = OffsetDateTime::parse(fetched_at, &Rfc3339) else {
        return false;
    };
    (OffsetDateTime::now_utc() - then).whole_seconds() < CACHE_SECONDS
}

fn meter(spec: &Value, root: &Value) -> Value {
    let mut out = json!({
        "id": spec.get("id").cloned().unwrap_or(Value::Null),
        "kind": spec.get("kind").cloned().unwrap_or_else(|| json!("quota")),
        "label": spec.get("label").cloned().unwrap_or(Value::Null),
        "unit": spec.get("unit").cloned().unwrap_or_else(|| json!("%")),
    });
    for key in ["window"] {
        if let Some(value) = spec.get(key) {
            out[key] = value.clone();
        }
    }
    if let Some(limit) = spec.get("limit") {
        out["limit"] = number_from(limit.clone(), root);
    }
    for key in ["used", "remaining", "resetAt"] {
        if let Some(expression) = spec.get(key).and_then(Value::as_str) {
            if let Some(value) = eval(expression, root) {
                out[key] = if key == "resetAt" {
                    value
                } else {
                    coerce_number(value)
                };
            }
        }
    }
    out
}

fn number_from(value: Value, root: &Value) -> Value {
    match value {
        Value::String(expression) => eval(&expression, root)
            .map(coerce_number)
            .unwrap_or(Value::Null),
        other => other,
    }
}

fn coerce_number(value: Value) -> Value {
    match value {
        Value::String(text) => text
            .parse::<f64>()
            .map(|number| json!(number))
            .unwrap_or(Value::String(text)),
        other => other,
    }
}

/// Evaluate the subset used by the connectors:
/// `$.a.b`, a number, and one binary `+ - * /` between those.
fn eval(expression: &str, root: &Value) -> Option<Value> {
    let expression = expression.trim();
    if expression.is_empty() {
        return None;
    }
    for operator in [" - ", " + ", " * ", " / "] {
        if let Some(at) = expression.find(operator) {
            let left = expression[..at].trim();
            let right = expression[at + operator.len()..].trim();
            let left = eval(left, root)?.as_f64()?;
            let right = eval(right, root)?.as_f64()?;
            let value = match operator {
                " - " => left - right,
                " + " => left + right,
                " * " => left * right,
                " / " if right != 0.0 => left / right,
                _ => return None,
            };
            return Some(json!(value));
        }
    }
    if let Some(path) = expression.strip_prefix("$.") {
        return json_path(root, path);
    }
    expression.parse::<f64>().ok().map(|number| json!(number))
}

fn json_path(root: &Value, path: &str) -> Option<Value> {
    let mut current = root;
    for part in path.split('.') {
        if part.is_empty() {
            continue;
        }
        current = if let Ok(index) = part.parse::<usize>() {
            current.get(index)?
        } else {
            current.get(part)?
        };
    }
    Some(current.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_the_opencode_mapping() {
        let body = json!({
            "usage": {
                "rolling": {"percent": 37.5, "resetsAt": "2026-10-22T12:00:00Z"},
                "weekly": {"percent": 12, "resetsAt": "2026-10-25T00:00:00Z"}
            }
        });
        let spec = json!({
            "id": "opencode_go_5h",
            "kind": "quota",
            "label": "5h limit",
            "limit": 100,
            "remaining": "100 - $.usage.rolling.percent",
            "resetAt": "$.usage.rolling.resetsAt",
            "unit": "%",
            "used": "$.usage.rolling.percent",
            "window": "5h"
        });
        let meter = meter(&spec, &body);
        assert_eq!(meter["used"], json!(37.5));
        assert_eq!(meter["remaining"], json!(62.5));
        assert_eq!(meter["limit"], json!(100));
        assert_eq!(meter["resetAt"], json!("2026-10-22T12:00:00Z"));
    }

    #[test]
    fn missing_path_is_null_not_zero() {
        let meter = meter(
            &json!({"id": "x", "used": "$.nope", "remaining": "100 - $.nope"}),
            &json!({}),
        );
        assert!(meter.get("used").is_none());
        assert!(meter.get("remaining").is_none());
    }

    #[test]
    fn rejects_division_by_zero() {
        assert!(eval("10 / 0", &json!({})).is_none());
    }
}
