//! The provider's models (`GET /models`), and the listing Claude Code reads.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::errors::upstream_message;
use crate::{ProxyError, Upstream};

/// One model the provider offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    /// What the provider calls it, when that isn't its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its context window in tokens, for the providers that say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
}

/// The models the provider lists, by id.
///
/// # Errors
/// The provider can't be reached, refuses (a bad key), or answers with
/// something that isn't a list of models.
pub async fn list_models(upstream: &Upstream) -> Result<Vec<CatalogModel>, ProxyError> {
    let host = upstream.host();
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| ProxyError::Unreachable {
            host: host.clone(),
            detail: e.to_string(),
        })?;
    let mut req = client.get(upstream.endpoint("/models"));
    if let Some(key) = upstream.api_key.as_deref().filter(|k| !k.is_empty()) {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.map_err(|e| ProxyError::Unreachable {
        host: host.clone(),
        detail: crate::server::describe(&e),
    })?;
    let status = resp.status().as_u16();
    let body = resp.text().await.map_err(|e| ProxyError::Unreachable {
        host: host.clone(),
        detail: e.to_string(),
    })?;
    if !(200..300).contains(&status) {
        return Err(ProxyError::Status {
            host,
            status,
            message: upstream_message(&body).0,
        });
    }
    let v: Value =
        serde_json::from_str(&body).map_err(|_| ProxyError::Invalid { host: host.clone() })?;
    parse_models(&v).ok_or(ProxyError::Invalid { host })
}

/// `{"data":[{"id":…}]}` as OpenAI writes it, a bare array, or Ollama's
/// `{"models":[…]}`.
fn parse_models(v: &Value) -> Option<Vec<CatalogModel>> {
    let list = v
        .get("data")
        .or_else(|| v.get("models"))
        .unwrap_or(v)
        .as_array()?;
    let num = |m: &Value, keys: &[&str]| {
        keys.iter()
            .find_map(|k| m.pointer(k).and_then(Value::as_u64))
            .filter(|&n| n > 0)
    };
    let mut models: Vec<CatalogModel> = list
        .iter()
        .filter_map(|m| {
            let id = m
                .get("id")
                .or_else(|| m.get("model"))
                .or_else(|| m.get("name"))?
                .as_str()?
                .to_owned();
            let name = ["name", "display_name"]
                .iter()
                .find_map(|k| m.get(*k).and_then(Value::as_str))
                .filter(|n| !n.is_empty() && *n != id)
                .map(str::to_owned);
            let context = num(
                m,
                &[
                    "/context_length",
                    "/context_window",
                    "/max_model_len",
                    "/max_context_length",
                    "/top_provider/context_length",
                ],
            );
            Some(CatalogModel { id, name, context })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    Some(models)
}

/// The models on offer as the Messages API lists them (`GET /v1/models`).
pub(crate) fn listing(upstream: &Upstream) -> Value {
    let mut ids: Vec<&str> = upstream.models.iter().map(String::as_str).collect();
    if ids.is_empty() && !upstream.model.is_empty() {
        ids.push(&upstream.model);
    }
    let data: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({ "type": "model", "id": id, "display_name": id, "created_at": "2025-01-01T00:00:00Z" })
        })
        .collect();
    json!({
        "data": data,
        "has_more": false,
        "first_id": ids.first(),
        "last_id": ids.last(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_read_as_each_provider_writes_them() {
        let openai = json!({ "object": "list", "data": [
            { "id": "gpt-5", "object": "model", "owned_by": "openai" },
            { "id": "gpt-4.1", "object": "model" }
        ]});
        let ids: Vec<String> = parse_models(&openai)
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["gpt-4.1", "gpt-5"]);
        let openrouter = json!({ "data": [
            { "id": "qwen/qwen3-coder", "name": "Qwen: Qwen3 Coder", "context_length": 262_144 }
        ]});
        assert_eq!(
            parse_models(&openrouter),
            Some(vec![CatalogModel {
                id: "qwen/qwen3-coder".into(),
                name: Some("Qwen: Qwen3 Coder".into()),
                context: Some(262_144),
            }])
        );
        let vllm = json!({ "data": [{ "id": "Qwen/Qwen3-32B", "max_model_len": 32768 }] });
        assert_eq!(parse_models(&vllm).and_then(|m| m[0].context), Some(32768));
        assert!(parse_models(&json!({ "error": "nope" })).is_none());
    }
}
