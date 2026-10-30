//! The proxy between a client speaking the Messages API and a fake
//! OpenAI-compatible server.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use berth_proxy::{Proxy, Upstream, list_models};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// What the fake server was sent.
type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// An OpenAI-compatible server whose model takes at most 8192 output
/// tokens and answers with text and a tool call.
async fn fake_upstream() -> (String, Seen) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let log = log.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<Incoming>| {
                    let log = log.clone();
                    async move { Ok::<_, Infallible>(answer(req, &log).await) }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

async fn answer(req: Request<Incoming>, log: &Seen) -> Response<Full<Bytes>> {
    let path = req.uri().path().to_owned();
    let auth = req
        .headers()
        .get("authorization")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = req.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    log.lock().unwrap().push((path.clone(), body.clone()));
    let respond = |status: u16, ty: &str, text: String| {
        Response::builder()
            .status(status)
            .header("content-type", ty)
            .body(Full::new(Bytes::from(text)))
            .unwrap()
    };
    if auth.as_deref() != Some("Bearer sk-test") {
        return respond(
            401,
            "application/json",
            json!({ "error": { "message": "Incorrect API key" } }).to_string(),
        );
    }
    if path == "/v1/models" {
        return respond(
            200,
            "application/json",
            json!({ "data": [{ "id": "fake-coder", "context_length": 65536 }] }).to_string(),
        );
    }
    if body["max_tokens"].as_u64().unwrap_or(0) > 8192 {
        return respond(
            400,
            "application/json",
            json!({ "error": { "message": "Invalid max_tokens value, the valid range of max_tokens is [1, 8192]" } }).to_string(),
        );
    }
    if body["stream"] == true {
        let chunks = [
            json!({ "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "On it." } }] }),
            json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [{ "index": 0, "id": "call_9", "type": "function", "function": { "name": "Bash", "arguments": "{\"command\":" } }] } }] }),
            json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "\"ls\"}" } }] } }] }),
            json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }] }),
            json!({ "choices": [], "usage": { "prompt_tokens": 321, "completion_tokens": 12 } }),
        ];
        let mut text: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        text.push_str("data: [DONE]\n\n");
        return respond(200, "text/event-stream", text);
    }
    respond(
        200,
        "application/json",
        json!({ "id": "x", "choices": [{ "index": 0, "finish_reason": "stop", "message": { "role": "assistant", "content": "Hello." } }],
                "usage": { "prompt_tokens": 5, "completion_tokens": 2 } })
        .to_string(),
    )
}

async fn proxy(base: &str) -> Proxy {
    let proxy = Proxy::start().await.unwrap();
    proxy.configure(Some(Upstream {
        base_url: base.to_owned(),
        api_key: Some("sk-test".to_owned()),
        model: "fake-coder".to_owned(),
        fast_model: None,
        models: vec!["fake-coder".to_owned()],
    }));
    proxy
}

fn request(stream: bool) -> Value {
    json!({
        "model": "claude-opus-5-5",
        "max_tokens": 32000,
        "stream": stream,
        "system": "You are Claude Code.",
        "tools": [{ "name": "Bash", "description": "Run", "input_schema": { "type": "object" } }],
        "messages": [{ "role": "user", "content": "list the files" }],
    })
}

#[tokio::test]
async fn a_streamed_answer_comes_back_as_messages_events() {
    let (base, seen) = fake_upstream().await;
    let proxy = proxy(&base).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages?beta=true", proxy.base_url()))
        .header("x-api-key", proxy.token())
        .json(&request(true))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("event-stream")
    );
    let text = resp.text().await.unwrap();
    let events: Vec<Value> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|d| serde_json::from_str(d).unwrap())
        .collect();
    let kinds: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    assert_eq!(events[0]["message"]["model"], "claude-opus-5-5");
    assert_eq!(events[2]["delta"]["text"], "On it.");
    assert_eq!(events[4]["content_block"]["name"], "Bash");
    assert_eq!(events[4]["content_block"]["id"], "call_9");
    assert_eq!(events[5]["delta"]["partial_json"], r#"{"command":"ls"}"#);
    assert_eq!(events[7]["delta"]["stop_reason"], "tool_use");
    assert_eq!(events[7]["usage"]["input_tokens"], 321);

    // Refused at 32000, sent again within the model's 8192, and the model
    // the configured one.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].0, "/v1/chat/completions");
    assert_eq!(seen[1].1["max_tokens"], 8192);
    assert_eq!(seen[1].1["model"], "fake-coder");
    assert_eq!(seen[1].1["messages"][0]["role"], "system");
}

#[tokio::test]
async fn a_whole_answer_and_what_the_model_learned() {
    let (base, seen) = fake_upstream().await;
    let proxy = proxy(&base).await;
    let client = reqwest::Client::new();
    for _ in 0..2 {
        let v: Value = client
            .post(format!("{}/v1/messages", proxy.base_url()))
            .bearer_auth(proxy.token())
            .json(&request(false))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["content"][0]["text"], "Hello.");
        assert_eq!(v["stop_reason"], "end_turn");
    }
    // The cap learned on the first request holds for the second.
    let max: Vec<u64> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, b)| b["max_tokens"].as_u64().unwrap())
        .collect();
    assert_eq!(max, [32000, 8192, 8192]);
}

#[tokio::test]
async fn only_the_token_gets_in_and_errors_read_as_anthropic_s() {
    let (base, _) = fake_upstream().await;
    let proxy = proxy(&base).await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/messages", proxy.base_url());
    let denied = client
        .post(&url)
        .header("x-api-key", "guess")
        .json(&request(true))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);

    proxy.configure(Some(Upstream {
        base_url: base.clone(),
        api_key: Some("sk-wrong".to_owned()),
        model: "fake-coder".to_owned(),
        ..Upstream::default()
    }));
    let resp = client
        .post(&url)
        .header("x-api-key", proxy.token())
        .json(&request(true))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["error"]["type"], "authentication_error");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("Incorrect API key")
    );

    let count: Value = client
        .post(format!("{}/v1/messages/count_tokens", proxy.base_url()))
        .header("x-api-key", proxy.token())
        .json(&request(false))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(count["input_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn models_list_with_their_windows() {
    let (base, _) = fake_upstream().await;
    let up = Upstream {
        base_url: base.clone(),
        api_key: Some("sk-test".to_owned()),
        ..Upstream::default()
    };
    let models = list_models(&up).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(
        (models[0].id.as_str(), models[0].context),
        ("fake-coder", Some(65536))
    );
    let bad = Upstream {
        api_key: Some("nope".to_owned()),
        ..up
    };
    let err = list_models(&bad).await.unwrap_err().to_string();
    assert!(
        err.contains("401") && err.contains("Incorrect API key"),
        "{err}"
    );
}
