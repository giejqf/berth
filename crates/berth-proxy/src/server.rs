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
use crate::{Api, Effort, Upstream};

type Body = UnsyncBoxBody<Bytes, Infallible>;

/// A ping when nothing went to Claude Code for this long (a model reasoning
/// before it writes), so the stream never looks stalled.
const PING_EVERY: Duration = Duration::from_secs(10);

/// The largest request Claude Code sends (images included).
const MAX_BODY: usize = 64 << 20;

/// How many times a request goes again in a shape the model takes.
const ATTEMPTS: usize = 8;

/// What one of Berth's sessions asks of its requests (ADR 0037).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPrefs {
    /// The slot's effort, as the model's reasoning effort; `None` leaves it
    /// to the provider.
    pub effort: Option<Effort>,
}

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
    /// Each session's wishes, by the id its token ends with.
    sessions: RwLock<HashMap<String, SessionPrefs>>,
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
            sessions: RwLock::default(),
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

    /// The token for one of Berth's sessions, `id`: its requests carry
    /// `prefs`, which [`Self::set_session`] changes mid-session.
    #[must_use]
    pub fn session_token(&self, id: &str, prefs: SessionPrefs) -> String {
        self.set_session(id, prefs);
        format!("{}.{id}", self.state.token)
    }

    /// What session `id` asks of its requests from the next one on.
    pub fn set_session(&self, id: &str, prefs: SessionPrefs) {
        self.state
            .sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned(), prefs);
    }

    /// Session `id` is over.
    pub fn end_session(&self, id: &str) {
        self.state
            .sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
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
    // Claude Code's look at whether its API is up.
    if matches!(*req.method(), Method::GET | Method::HEAD)
        && matches!(path.as_str(), "/" | "/api/hello")
    {
        return json_response(200, &json!({}));
    }
    let Some(session) = authorized(&state.token, &req) else {
        return error_response(&ApiError::new(
            401,
            "authentication_error",
            "This is Berth's model provider proxy: only Berth's agents can use it.",
        ));
    };
    let prefs = session
        .and_then(|id| {
            state
                .sessions
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&id)
                .cloned()
        })
        .unwrap_or_default();
    let upstream = state.upstream().unwrap_or_default();
    match (req.method().clone(), path.as_str()) {
        (Method::POST, "/v1/messages") => messages(&state, req, &prefs).await,
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

/// Whether the request carries the token (`Some`), and the session it
/// names (`token.<session>`), if any.
fn authorized(token: &str, req: &Request<Incoming>) -> Option<Option<String>> {
    let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok());
    let given = header("x-api-key")
        .or_else(|| header("authorization").and_then(|a| a.strip_prefix("Bearer ")))?;
    let rest = given.strip_prefix(token)?;
    if rest.is_empty() {
        return Some(None);
    }
    rest.strip_prefix('.')
        .filter(|id| !id.is_empty())
        .map(|id| Some(id.to_owned()))
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

async fn messages(state: &State, req: Request<Incoming>, prefs: &SessionPrefs) -> Response<Body> {
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
    let host = up.host();
    let (resp, out, api) = match send(state, &up, &model, &body, prefs).await {
        Ok(r) => r,
        Err(e) => return error_response(&e),
    };
    tracing::debug!(%model, ?api, stream = out.stream, "proxy request");
    let streamed = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains("event-stream"));
    if out.stream && streamed {
        return stream_response(
            resp,
            Translator::new(api, echoed, out.names, out.estimate),
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
    // A Responses API answer always has `error`: null when all went well.
    let err = v.get("error").filter(|e| !e.is_null());
    if let (Some(err), None) = (err, v.get("choices")) {
        return error_response(&ApiError::in_stream(err));
    }
    let message = response::message(&v, api, echoed, &out.names, out.estimate);
    if out.stream {
        sse_response(&events_for(&message))
    } else {
        json_response(200, &message)
    }
}

/// The request upstream in the API and shape the model takes, sent again
/// in another when it refuses one (a few times). What was learned is kept
/// for the model's next requests once a request in it went through.
async fn send(
    state: &State,
    up: &Upstream,
    model: &str,
    req: &Value,
    prefs: &SessionPrefs,
) -> Result<(reqwest::Response, Outgoing, Api), ApiError> {
    let host = up.host();
    let known = state.shape(model);
    let mut shape = known.clone();
    let window = up.window(model);
    // A smaller max_tokens for this request alone: what the model's window
    // leaves after the prompt, as far as it is known.
    let mut budget = None;
    let mut fitted = false;
    for _ in 0..ATTEMPTS {
        let api = shape.api.unwrap_or(up.api);
        let out = translate(req, model, api, &shape);
        if !fitted {
            budget = window.and_then(|w| fit(out.max_tokens?, out.estimate, w));
            fitted = true;
        }
        // The slot's effort, for a request Claude Code asks the model to
        // think in; others are left to the provider.
        let effort = prefs.effort.filter(|_| out.thinking);
        let max_tokens = budget.or(out.max_tokens);
        let mut body = out.body.clone();
        shape.apply(&mut body, max_tokens, api, effort);
        let request = up.authorize(state.client.post(up.endpoint(api.path())).json(&body));
        let resp = request
            .send()
            .await
            .map_err(|e| ApiError::unreachable(&host, &describe(&e)))?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            if shape != known {
                state.remember(model, &shape);
            }
            return Ok((resp, out, api));
        }
        let retry_after = ["retry-after", "retry-after-ms"]
            .iter()
            .find_map(|h| resp.headers().get(*h))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let text = resp.text().await.unwrap_or_default();
        let (message, param) = upstream_message(&text);
        if matches!(status, 400 | 404 | 422) && !filtered(&text) {
            let sent = max_tokens.map(|k| shape.max_tokens.map_or(k, |cap| k.min(cap)));
            if let Some(next) = shape.adjust(&message, param.as_deref(), sent, api) {
                tracing::info!(
                    model,
                    ?next,
                    "the model refused the request's shape; sending it again"
                );
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
        tracing::warn!(model, status, ?api, "the provider refused a request");
        if status == 404 && text.contains("DeploymentNotFound") {
            return Err(ApiError::new(
                404,
                "not_found_error",
                format!(
                    "{host} has no deployment named \u{201c}{model}\u{201d}. On Azure a model is its deployment's name: add that name in Berth's Settings › Model provider."
                ),
            ));
        }
        // A server with no Responses API (404 for the path, not the model):
        // this model goes by Chat Completions, as the next request will.
        if api == Api::Responses
            && matches!(status, 404 | 405)
            && !message.to_ascii_lowercase().contains("model")
            && shape.api.is_none()
        {
            tracing::info!(model, "no Responses API there; Chat Completions instead");
            shape.api = Some(Api::ChatCompletions);
            continue;
        }
        let mut e = ApiError::from_upstream(status, &text, &host, estimate_request(req), window);
        e.retry_after = retry_after;
        return Err(e);
    }
    Err(ApiError::new(
        400,
        "invalid_request_error",
        format!("{host} kept refusing the request"),
    ))
}

/// Azure's content filter refused the prompt: nothing about its shape.
fn filtered(body: &str) -> bool {
    body.contains("content_filter") || body.contains("content management policy")
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
    let mut resp = json_response(e.status, &e.body());
    if let Some(wait) = e
        .retry_after
        .as_deref()
        .and_then(|w| hyper::header::HeaderValue::from_str(w).ok())
    {
        resp.headers_mut().insert("retry-after", wait);
    }
    resp
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
