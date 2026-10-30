//! The local server Claude Code talks to: `127.0.0.1` on a free port, open
//! only to a token handed to Berth's agents.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use bytes::Bytes;
use futures::channel::mpsc;
use futures::{SinkExt as _, StreamExt as _};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt as _, Full, Limited, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{CACHE_CONTROL, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::Instant;

use crate::errors::{ApiError, Overflow, upstream_message};
use crate::models::listing;
use crate::request::{Outgoing, estimate_request, translate};
use crate::response;
use crate::shape::Shape;
use crate::stream::{Event, SseLines, Translator, events_for};
use crate::{Api, Upstream};

type Body = UnsyncBoxBody<Bytes, Infallible>;

/// A ping when nothing went to Claude Code for this long (a model reasoning
/// before it writes), so the stream never looks stalled.
const PING_EVERY: Duration = Duration::from_secs(10);

/// The largest request Claude Code sends (images included).
const MAX_BODY: usize = 64 << 20;

/// The server, for as long as this value lives.
pub struct Proxy {
    addr: SocketAddr,
    state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
}

struct State {
    token: String,
    upstream: RwLock<Option<Upstream>>,
    client: reqwest::Client,
    /// Each upstream model's shape, as learned ([`Shape`]).
    shapes: Mutex<HashMap<String, Shape>>,
}

impl State {
    fn upstream(&self) -> Option<Upstream> {
        self.upstream
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn shape(&self, model: &str) -> Shape {
        self.shapes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(model)
            .cloned()
            .unwrap_or_default()
    }

    fn remember(&self, model: &str, shape: &Shape) {
        self.shapes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(model.to_owned(), shape.clone());
    }
}

impl Proxy {
    /// Listen on a free port of `127.0.0.1`. Requests answer "not set up"
    /// until [`Self::configure`] names an upstream.
    ///
    /// # Errors
    /// No port could be had, or the HTTP client couldn't be made (the Mac's
    /// certificates unreadable).
    pub async fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            // Between bytes, not overall: an answer may stream for minutes.
            .read_timeout(Duration::from_secs(600))
            .build()
            .map_err(std::io::Error::other)?;
        let state = Arc::new(State {
            token: format!("berth-{}", uuid::Uuid::new_v4().simple()),
            upstream: RwLock::new(None),
            client,
            shapes: Mutex::default(),
        });
        let serving = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let stream = match listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(e) => {
                        tracing::warn!(error = %e, "proxy accept");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                };
                let state = serving.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req| {
                        let state = state.clone();
                        async move { Ok::<_, Infallible>(handle(state, req).await) }
                    });
                    if let Err(e) = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await
                    {
                        tracing::debug!(error = %e, "proxy connection");
                    }
                });
            }
        });
        tracing::info!(%addr, "model provider proxy listening");
        Ok(Self { addr, state, task })
    }

    /// `ANTHROPIC_BASE_URL` for Claude Code.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// `ANTHROPIC_AUTH_TOKEN` for Claude Code: what the server takes, and
    /// nothing else does.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.state.token
    }

    /// The upstream from the next request on; `None` refuses them. A new
    /// server forgets what the old one's models refused.
    pub fn configure(&self, upstream: Option<Upstream>) {
        let mut current = self
            .state
            .upstream
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let moved = current.as_ref().map(|u| &u.base_url) != upstream.as_ref().map(|u| &u.base_url);
        if moved {
            self.state
                .shapes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        }
        *current = upstream;
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(state: Arc<State>, req: Request<Incoming>) -> Response<Body> {
    let path = req.uri().path().to_owned();
    if matches!(*req.method(), Method::GET | Method::HEAD) && path == "/" {
        return json_response(200, &json!({}));
    }
    if !authorized(&state.token, &req) {
        return error_response(&ApiError::new(
            401,
            "authentication_error",
            "This is Berth's model provider proxy: only Berth's agents can use it.",
        ));
    }
    let upstream = state.upstream().unwrap_or_default();
    match (req.method().clone(), path.as_str()) {
        (Method::POST, "/v1/messages") => messages(&state, req).await,
        (Method::POST, "/v1/messages/count_tokens") => match read_json(req).await {
            Ok(body) => json_response(200, &json!({ "input_tokens": estimate_request(&body) })),
            Err(e) => error_response(&e),
        },
        (Method::GET, "/v1/models") => json_response(200, &listing(&upstream)),
        (Method::GET, p) if p.starts_with("/v1/models/") => {
            let id = &p["/v1/models/".len()..];
            json_response(
                200,
                &json!({ "type": "model", "id": id, "display_name": id, "created_at": "2025-01-01T00:00:00Z" }),
            )
        }
        _ => error_response(&ApiError::new(
            404,
            "not_found_error",
            format!("Berth's model provider proxy has no {path}"),
        )),
    }
}

fn authorized(token: &str, req: &Request<Incoming>) -> bool {
    let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok());
    header("x-api-key") == Some(token)
        || header("authorization").and_then(|a| a.strip_prefix("Bearer ")) == Some(token)
}

async fn read_json(req: Request<Incoming>) -> Result<Value, ApiError> {
    let bytes = Limited::new(req.into_body(), MAX_BODY)
        .collect()
        .await
        .map_err(|e| ApiError::new(413, "request_too_large", e.to_string()))?
        .to_bytes();
    serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::new(400, "invalid_request_error", format!("not JSON: {e}")))
}

async fn messages(state: &State, req: Request<Incoming>) -> Response<Body> {
    let body = match read_json(req).await {
        Ok(v) => v,
        Err(e) => return error_response(&e),
    };
    let Some(up) = state.upstream() else {
        return error_response(&ApiError::new(
            401,
            "authentication_error",
            "No OpenAI-compatible API is set up: choose one in Berth's Settings › Model provider.",
        ));
    };
    let requested = body.get("model").and_then(Value::as_str).unwrap_or("");
    let model = up.resolve(requested);
    if model.is_empty() {
        return error_response(&ApiError::new(
            400,
            "invalid_request_error",
            "No model is chosen for the OpenAI-compatible API: pick one in Berth's Settings › Model provider.",
        ));
    }
    // What Claude Code asked for is the model it hears back from.
    let echoed = if requested.is_empty() {
        model.as_str()
    } else {
        requested
    };
    let out = translate(&body, &model, up.api);
    tracing::debug!(%model, stream = out.stream, "proxy request");
    let host = up.host();
    let resp = match send(state, &up, &model, &out).await {
        Ok(r) => r,
        Err(e) => return error_response(&e),
    };
    let streamed = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains("event-stream"));
    if out.stream && streamed {
        return stream_response(
            resp,
            Translator::new(up.api, echoed, out.names, out.estimate),
            host,
        );
    }
    // A whole answer: asked for, or what the server sends regardless.
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return error_response(&ApiError::unreachable(&host, &describe(&e))),
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return error_response(&ApiError::new(
            502,
            "api_error",
            format!("{host} answered with something that isn't JSON"),
        ));
    };
    if let (Some(err), None) = (v.get("error"), v.get("choices")) {
        return error_response(&ApiError::in_stream(err));
    }
    let message = response::message(&v, up.api, echoed, &out.names, out.estimate);
    if out.stream {
        sse_response(&events_for(&message))
    } else {
        json_response(200, &message)
    }
}

/// The request upstream, sent again in the shape the model takes when it
/// refuses one (at most a few times).
async fn send(
    state: &State,
    up: &Upstream,
    model: &str,
    out: &Outgoing,
) -> Result<reqwest::Response, ApiError> {
    let host = up.host();
    let mut shape = state.shape(model);
    let window = up.window(model);
    // A smaller max_tokens for this request alone: what the model's window
    // leaves after the prompt, as far as it is known.
    let mut budget = window.and_then(|w| fit(out.max_tokens?, out.estimate, w));
    for _ in 0..4 {
        let max_tokens = budget.or(out.max_tokens);
        let mut body = out.body.clone();
        shape.apply(&mut body, max_tokens, up.api);
        let mut req = state.client.post(up.endpoint(up.api.path())).json(&body);
        if let Some(key) = up.api_key.as_deref().filter(|k| !k.is_empty()) {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ApiError::unreachable(&host, &describe(&e)))?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(resp);
        }
        let text = resp.text().await.unwrap_or_default();
        if matches!(status, 400 | 422) {
            let (message, param) = upstream_message(&text);
            let sent = max_tokens.map(|k| shape.max_tokens.map_or(k, |cap| k.min(cap)));
            if let Some(next) = shape.adjust(&message, param.as_deref(), sent) {
                tracing::info!(
                    model,
                    ?next,
                    "the model refused the request's shape; sending it again"
                );
                state.remember(model, &next);
                shape = next;
                continue;
            }
            if let (Some(o), Some(sent)) = (Overflow::read(&message), sent)
                && let Some(room) = o.budget(sent)
            {
                tracing::info!(model, room, "a smaller completion budget fits the window");
                budget = Some(room);
                continue;
            }
        }
        tracing::warn!(model, status, "the provider refused a request");
        if up.api == Api::Responses && matches!(status, 404 | 405) {
            let (message, _) = upstream_message(&text);
            // "The model `x` does not exist" is about the model, not the API.
            if !message.to_ascii_lowercase().contains("model") {
                return Err(ApiError::new(
                    404,
                    "not_found_error",
                    format!(
                        "{host} has no Responses API ({}): choose Chat Completions in Berth's Settings › Model provider.",
                        up.endpoint(up.api.path())
                    ),
                ));
            }
        }
        return Err(ApiError::from_upstream(
            status,
            &text,
            &host,
            out.estimate,
            window,
        ));
    }
    Err(ApiError::new(
        400,
        "invalid_request_error",
        format!("{host} kept refusing the request"),
    ))
}

/// An output budget that leaves the prompt room in the model's `window`: the
/// estimate, with a margin for its roughness. `None` when the budget fits
/// already, or when what is left is too little to be worth asking for (the
/// provider then says the prompt is too long, and Claude Code compacts).
fn fit(max_tokens: u64, estimate: u64, window: u64) -> Option<u64> {
    let room = window.checked_sub(estimate + estimate / 10 + 256)?;
    (room >= 1024 && room < max_tokens).then_some(room)
}

fn stream_response(resp: reqwest::Response, mut t: Translator, host: String) -> Response<Body> {
    let (mut tx, rx) = mpsc::channel::<Result<Frame<Bytes>, Infallible>>(64);
    tokio::spawn(async move {
        let mut upstream = resp.bytes_stream();
        let mut lines = SseLines::default();
        if !emit(&mut tx, &t.start()).await {
            return;
        }
        let mut last = Instant::now();
        loop {
            let chunk = match tokio::time::timeout_at(last + PING_EVERY, upstream.next()).await {
                Err(_) => {
                    if !emit(&mut tx, &Event::ping()).await {
                        return;
                    }
                    last = Instant::now();
                    continue;
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    let e = ApiError::new(
                        529,
                        "overloaded_error",
                        format!("{host}'s answer broke off: {}", describe(&e)),
                    );
                    emit(&mut tx, &Event::error(&e)).await;
                    return;
                }
                Ok(Some(Ok(bytes))) => bytes,
            };
            for data in lines.push(&chunk) {
                match t.data(&data) {
                    Ok(events) => {
                        for e in &events {
                            if !emit(&mut tx, e).await {
                                // Claude Code went away (an interrupt): so does the request.
                                return;
                            }
                            last = Instant::now();
                        }
                    }
                    Err(e) => {
                        emit(&mut tx, &Event::error(&e)).await;
                        return;
                    }
                }
            }
            if t.done() {
                break;
            }
        }
        if let Some(rest) = lines.finish()
            && let Err(e) = t.data(&rest)
        {
            emit(&mut tx, &Event::error(&e)).await;
            return;
        }
        if t.ended_early() {
            let e = ApiError::new(
                529,
                "overloaded_error",
                format!("{host}'s answer ended before it was done"),
            );
            emit(&mut tx, &Event::error(&e)).await;
            return;
        }
        for e in &t.finish() {
            if !emit(&mut tx, e).await {
                return;
            }
        }
    });
    Response::builder()
        .status(200)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .body(StreamBody::new(rx).boxed_unsync())
        .unwrap_or_else(|_| error_response(&ApiError::new(500, "api_error", "response")))
}

async fn emit(tx: &mut mpsc::Sender<Result<Frame<Bytes>, Infallible>>, e: &Event) -> bool {
    tx.send(Ok(Frame::data(Bytes::from(e.sse())))).await.is_ok()
}

fn sse_response(events: &[Event]) -> Response<Body> {
    let text: String = events.iter().map(Event::sse).collect();
    Response::builder()
        .status(200)
        .header(CONTENT_TYPE, "text/event-stream")
        .body(Full::new(Bytes::from(text)).boxed_unsync())
        .unwrap_or_else(|_| error_response(&ApiError::new(500, "api_error", "response")))
}

fn json_response(status: u16, v: &Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(v.to_string())).boxed_unsync())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed_unsync()))
}

fn error_response(e: &ApiError) -> Response<Body> {
    json_response(e.status, &e.body())
}

/// A client error with what lies under it ("Connection refused"), which
/// reqwest's own message leaves out.
pub(crate) fn describe(e: &reqwest::Error) -> String {
    let mut parts = vec![if e.is_timeout() {
        "timed out".to_owned()
    } else if e.is_connect() {
        "couldn't connect".to_owned()
    } else {
        e.to_string()
    }];
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        let text = s.to_string();
        if !parts.iter().any(|p| p.contains(&text)) {
            parts.push(text);
        }
        source = s.source();
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn the_output_budget_leaves_the_prompt_room_in_the_window() {
        // 20k of prompt in a 32k window: 32k of output can't fit.
        assert_eq!(fit(32_000, 20_000, 32_768), Some(10_512));
        // Room enough already.
        assert_eq!(fit(8_000, 20_000, 128_000), None);
        // Next to nothing left: no use asking.
        assert_eq!(fit(32_000, 31_000, 32_768), None);
    }
}
