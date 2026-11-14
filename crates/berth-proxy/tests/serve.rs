//! The proxy between a client speaking the Messages API and a fake
//! OpenAI-compatible server.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use berth_proxy::{Api, Effort, Proxy, SessionPrefs, Upstream, list_models};
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
    let full = req.uri().path().to_owned();
    // Under `/chatonly`: a server with no Responses API.
    let chat_only = full.starts_with("/chatonly");
    let path = full.trim_start_matches("/chatonly").to_owned();
    let auth = req
        .headers()
        .get("authorization")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = req.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    log.lock().unwrap().push((full.clone(), body.clone()));
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
    if body["model"] == "busy-model" {
        return Response::builder()
            .status(429)
            .header("retry-after", "7")
            .body(Full::new(Bytes::from(
                json!({ "error": { "code": "429", "message": "Please retry after 7 seconds." } })
                    .to_string(),
            )))
            .unwrap();
    }
    if path == "/v1/models" {
        return respond(
            200,
            "application/json",
            json!({ "data": [{ "id": "fake-coder", "context_length": 65536 }] }).to_string(),
        );
    }
    if path == "/v1/responses" && !chat_only {
        return responses(&body, respond);
    }
    if path != "/v1/chat/completions" {
        return respond(404, "text/plain", "404 page not found".to_owned());
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

/// The Responses API: text and a function call, streamed as its events.
fn responses(
    body: &Value,
    respond: impl Fn(u16, &str, String) -> Response<Full<Bytes>>,
) -> Response<Full<Bytes>> {
    if body["max_output_tokens"].as_u64().unwrap_or(0) > 8192 {
        return respond(
            400,
            "application/json",
            json!({ "error": { "message": "max_output_tokens is too large: this model supports at most 8192" } }).to_string(),
        );
    }
    if body["stream"] != true {
        return respond(
            200,
            "application/json",
            json!({ "id": "resp_2", "object": "response", "status": "completed", "error": null,
                    "incomplete_details": null,
                    "output": [{ "type": "message", "role": "assistant",
                                 "content": [{ "type": "output_text", "text": "Hello.", "annotations": [] }] }],
                    "usage": { "input_tokens": 5, "output_tokens": 2 } })
            .to_string(),
        );
    }
    let events = [
        json!({ "type": "response.created", "response": { "id": "resp_1", "status": "in_progress", "error": null } }),
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "message", "content": [] } }),
        json!({ "type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "On it." }),
        json!({ "type": "response.output_item.added", "output_index": 1, "item": { "type": "function_call", "call_id": "call_r1", "name": "Bash", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "output_index": 1, "delta": "{\"command\":\"ls\"}" }),
        json!({ "type": "response.completed", "response": { "status": "completed", "usage": { "input_tokens": 222, "output_tokens": 9 } } }),
    ];
    let text: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    respond(200, "text/event-stream", text)
}

async fn proxy_on(base: &str, api: Api, window: Option<u64>) -> Proxy {
    let proxy = Proxy::start().await.unwrap();
    proxy.configure(Some(Upstream {
        base_url: base.to_owned(),
        api_key: Some("sk-test".to_owned()),
        model: "fake-coder".to_owned(),
        fast_model: None,
        models: vec!["fake-coder".to_owned()],
        api,
        contexts: window
            .map(|w| [("fake-coder".to_owned(), w)].into())
            .unwrap_or_default(),
    }));
    proxy
}

async fn proxy(base: &str) -> Proxy {
    proxy_on(base, Api::ChatCompletions, None).await
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
        api: Api::ChatCompletions,
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
async fn a_rate_limit_passes_on_how_long_to_wait() {
    let (base, _) = fake_upstream().await;
    let proxy = proxy(&base).await;
    let mut req = request(false);
    req["model"] = json!("busy-model");
    proxy.configure(Some(Upstream {
        base_url: base.clone(),
        api_key: Some("sk-test".to_owned()),
        model: "fake-coder".to_owned(),
        models: vec!["fake-coder".to_owned(), "busy-model".to_owned()],
        api: Api::ChatCompletions,
        ..Upstream::default()
    }));
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", proxy.base_url()))
        .header("x-api-key", proxy.token())
        .json(&req)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 429);
    assert_eq!(resp.headers()["retry-after"], "7");
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

fn events_of(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|d| serde_json::from_str(d).unwrap())
        .collect()
}

#[tokio::test]
async fn a_responses_api_answer_streams_back_and_the_window_cuts_the_budget() {
    let (base, seen) = fake_upstream().await;
    let proxy = proxy_on(&base, Api::Responses, Some(16_384)).await;
    let text = reqwest::Client::new()
        .post(format!("{}/v1/messages", proxy.base_url()))
        .header("x-api-key", proxy.token())
        .json(&request(true))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let events = events_of(&text);
    let tool = events
        .iter()
        .find(|e| e["type"] == "content_block_start" && e["content_block"]["type"] == "tool_use")
        .unwrap();
    assert_eq!(tool["content_block"]["id"], "call_r1");
    let end = events
        .iter()
        .find(|e| e["type"] == "message_delta")
        .unwrap();
    assert_eq!(end["delta"]["stop_reason"], "tool_use");
    assert_eq!(end["usage"]["input_tokens"], 222);

    // 32000 of output can't fit a 16k window: the request went out cut to
    // what the window leaves, then to the 8192 the model takes.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].0, "/v1/responses");
    let first = seen[0].1["max_output_tokens"].as_u64().unwrap();
    assert!(first < 16_384 && first > 8192, "{first}");
    assert_eq!(seen[1].1["max_output_tokens"], 8192);
    assert_eq!(seen[0].1["input"][0]["content"], "list the files");
}

#[tokio::test]
async fn a_whole_responses_api_answer_is_an_answer() {
    // Its `error` is there, and null.
    let (base, _) = fake_upstream().await;
    let proxy = proxy_on(&base, Api::Responses, None).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", proxy.base_url()))
        .bearer_auth(proxy.token())
        .json(&request(false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["content"][0]["text"], "Hello.");
    assert_eq!(v["stop_reason"], "end_turn");
}

#[tokio::test]
async fn a_server_without_the_responses_api_gets_chat_completions() {
    let (base, seen) = fake_upstream().await;
    let proxy = proxy_on(&format!("{base}/chatonly/v1"), Api::Responses, None).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", proxy.base_url()))
        .header("x-api-key", proxy.token())
        .json(&request(false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["content"][0]["text"], "Hello.");
    let paths: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(p, _)| p.clone())
        .collect();
    assert_eq!(paths[0], "/chatonly/v1/responses");
    assert!(
        paths[1..]
            .iter()
            .all(|p| p == "/chatonly/v1/chat/completions")
    );
}

#[tokio::test]
async fn a_session_s_effort_reaches_the_model_when_it_is_asked_to_think() {
    let (base, seen) = fake_upstream().await;
    let proxy = proxy_on(&base, Api::Responses, None).await;
    let token = proxy.session_token(
        "wf-1",
        SessionPrefs {
            effort: Some(Effort::High),
        },
    );
    let client = reqwest::Client::new();
    let mut thinking = request(false);
    thinking["thinking"] = json!({ "type": "adaptive" });
    for (req, token) in [
        (thinking.clone(), token.clone()),
        (request(false), token.clone()),
        (thinking, proxy.token().to_owned()),
    ] {
        let resp = client
            .post(format!("{}/v1/messages", proxy.base_url()))
            .header("x-api-key", token)
            .json(&req)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }
    // Mid-session, the slot's effort changes.
    proxy.set_session(
        "wf-1",
        SessionPrefs {
            effort: Some(Effort::Low),
        },
    );
    let mut again = request(false);
    again["thinking"] = json!({ "type": "adaptive" });
    client
        .post(format!("{}/v1/messages", proxy.base_url()))
        .header("x-api-key", &token)
        .json(&again)
        .send()
        .await
        .unwrap();
    let efforts: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, b)| {
            b["max_output_tokens"].as_u64() == Some(8192) || b["max_output_tokens"].is_null()
        })
        .map(|(_, b)| b["reasoning"]["effort"].clone())
        .collect();
    assert_eq!(efforts.last().cloned(), Some(json!("low")), "{efforts:?}");
    let all: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, b)| b["reasoning"]["effort"].clone())
        .collect();
    assert!(all.contains(&json!("high")));
    // Without thinking asked, or without a session's effort: the provider's own.
    assert!(all.iter().filter(|e| e.is_null()).count() >= 2, "{all:?}");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|(_, b)| b["include"] == json!(["reasoning.encrypted_content"]))
    );
    // Another token is no session's.
    let denied = client
        .post(format!("{}/v1/messages", proxy.base_url()))
        .header("x-api-key", format!("{}x.wf-1", proxy.token()))
        .json(&request(false))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);
    let hello = client
        .head(format!("{}/api/hello", proxy.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(hello.status(), 200);
}
