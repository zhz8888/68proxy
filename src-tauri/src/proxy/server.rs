use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum::response::IntoResponse;
use bytes::Bytes;
use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use super::cc_client;
use super::config::Config;
use super::convert::{self, DEFAULT_MODEL};
use super::errors;
use super::log;
use super::sse::{AnthropicTranslator, OpenAiTranslator};
use super::state::{now_millis, now_secs, AppState, RequestInfo};

const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const NONSTREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const TIMEOUT_REDUCE_CONTEXT_THRESHOLD: u32 = 3;

static KEY_RE: OnceLock<regex::Regex> = OnceLock::new();

fn key_re() -> &'static regex::Regex {
    KEY_RE.get_or_init(|| regex::Regex::new(r"user_[A-Za-z0-9_-]+").unwrap())
}

/// 请求事件转发器（Tauri 注入，供前端中继轨道展示）。
static REQUEST_SINK: Mutex<Option<Box<dyn Fn(&RequestInfo) + Send + Sync>>> = Mutex::new(None);

pub fn set_request_sink<F>(f: F)
where
    F: Fn(&RequestInfo) + Send + Sync + 'static,
{
    *REQUEST_SINK.lock().unwrap() = Some(Box::new(f));
}

fn emit_request(info: &RequestInfo) {
    if let Some(sink) = REQUEST_SINK.lock().unwrap().as_ref() {
        sink(info);
    }
}

pub(crate) fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let auth = auth.strip_prefix("Bearer ")?;
    key_re().find(auth).map(|m| m.as_str().to_string())
}

fn config_port(cfg: &Config) -> u16 {
    cfg.port
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/v1/models", get(models))
        .route("/health", get(health))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state)
}

pub async fn serve(
    listener: TcpListener,
    state: Arc<AppState>,
    shutdown: oneshot::Receiver<()>,
    with_background: bool,
) -> io::Result<()> {
    let addr = listener.local_addr().unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap());
    state.mark_started();
    log::info(&format!("68proxy listening on http://{}", addr));

    if with_background {
        // 后台任务：CC 版本刷新（启动 + 每 24h）、会话清理（每小时）
        {
            let st = state.clone();
            tokio::spawn(async move {
                cc_client::refresh_cc_version(&st).await;
                loop {
                    tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
                    cc_client::refresh_cc_version(&st).await;
                }
            });
        }
        {
            let st = state.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60 * 60)).await;
                    let now = now_millis();
                    let mut sessions = st.sessions.lock().unwrap();
                    let before = sessions.len();
                    sessions.retain(|_, e| now < e.expires_at);
                    let cleaned = sessions.len() != before;
                    drop(sessions);
                    if cleaned {
                        log::info("Session cleanup done");
                    }
                }
            });
        }
    }

    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            let _ = shutdown.await;
        })
        .await
}

fn json_response(status: u16, body: Value, retry_after: Option<u64>) -> axum::response::Response {
    let mut builder = axum::response::Response::builder()
        .status(status)
        .header("Content-Type", "application/json");
    if let Some(ra) = retry_after {
        builder = builder.header("Retry-After", ra.to_string());
    }
    builder
        .body(Body::from(body.to_string()))
        .expect("valid response")
}

fn sse_response(
    prefix: Vec<String>,
    rx: mpsc::Receiver<Frame>,
    protocol: Protocol,
    st: Arc<AppState>,
    ctx: ReqCtx,
) -> axum::response::Response {
    let prefix_stream = stream::iter(prefix.into_iter().map(|s| Ok::<_, Infallible>(s)));
    let tail = stream::unfold(
        (rx, protocol, st, ctx, false, None::<String>),
        |(mut rx, protocol, st, ctx, mut ended, mut pending)| async move {
            loop {
                if let Some(f) = pending.take() {
                    return Some((Ok::<_, Infallible>(f), (rx, protocol, st, ctx, ended, None)));
                }
                if ended {
                    return None;
                }
                match rx.recv().await {
                    Some(Frame::Sse(s)) => {
                        return Some((Ok(s), (rx, protocol, st, ctx, ended, None)));
                    }
                    Some(Frame::Done { .. }) => {
                        finish_request(&st, &ctx, "ok");
                        return None;
                    }
                    Some(Frame::Timeout) => {
                        finish_request(&st, &ctx, "timeout");
                        let f = protocol_error_frame(protocol, &timeout_message(&st), None);
                        pending = Some(f);
                        ended = true;
                    }
                    Some(Frame::Error(msg)) => {
                        finish_request(&st, &ctx, "error");
                        pending = Some(protocol_error_frame(protocol, &msg, None));
                        ended = true;
                    }
                    None => return None,
                }
            }
        },
    );
    let stream = prefix_stream.chain(tail);
    axum::response::Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(stream))
        .expect("valid sse response")
}

#[derive(Clone, Copy)]
enum Protocol {
    OpenAi,
    Anthropic,
}

fn protocol_error_frame(protocol: Protocol, msg: &str, retry_after: Option<u64>) -> String {
    match protocol {
        Protocol::OpenAi => {
            let mut body = json!({ "error": { "message": msg, "type": "proxy_error" } });
            if let Some(ra) = retry_after {
                body["retry_after"] = json!(ra);
            }
            format!("data: {body}\n\n")
        }
        Protocol::Anthropic => format!(
            "event: error\ndata: {}\n\n",
            json!({ "type": "error", "error": { "type": "internal_error", "message": msg } })
        ),
    }
}

fn timeout_message(st: &AppState) -> String {
    let n = st.consecutive_timeouts.fetch_add(1, Ordering::SeqCst) + 1;
    if n >= TIMEOUT_REDUCE_CONTEXT_THRESHOLD {
        "Response timeout - try reducing context length (summarize earlier messages)".into()
    } else {
        "Response timeout - request timed out".into()
    }
}

fn reset_timeouts(st: &AppState) {
    st.consecutive_timeouts.store(0, Ordering::SeqCst);
}

fn record_start(st: &AppState, ctx: &ReqCtx) {
    let info = RequestInfo {
        id: ctx.id.clone(),
        path: ctx.path.to_string(),
        model: ctx.model.clone(),
        stream: ctx.stream,
        status: "streaming".into(),
        started_at: ctx.started_at,
        elapsed_ms: 0,
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        last_event: String::new(),
    };
    st.record_request(info.clone());
    emit_request(&info);
}

fn finish_request(st: &AppState, ctx: &ReqCtx, status: &str) {
    let elapsed = now_millis().saturating_sub(ctx.started_at);
    let mut q = st.requests.lock().unwrap();
    if let Some(entry) = q.iter_mut().find(|r| r.id == ctx.id) {
        entry.status = status.to_string();
        entry.elapsed_ms = elapsed;
        entry.input_tokens = ctx.input_tokens;
        entry.output_tokens = ctx.output_tokens;
        entry.cached_tokens = ctx.cached_tokens;
        entry.last_event = ctx.last_event.clone();
    }
    drop(q);
    let info = RequestInfo {
        id: ctx.id.clone(),
        path: ctx.path.to_string(),
        model: ctx.model.clone(),
        stream: ctx.stream,
        status: status.to_string(),
        started_at: ctx.started_at,
        elapsed_ms: elapsed,
        input_tokens: ctx.input_tokens,
        output_tokens: ctx.output_tokens,
        cached_tokens: ctx.cached_tokens,
        last_event: ctx.last_event.clone(),
    };
    emit_request(&info);
}

#[derive(Clone)]
struct ReqCtx {
    id: String,
    path: &'static str,
    model: String,
    stream: bool,
    started_at: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    last_event: String,
}

enum Frame {
    Sse(String),
    Done { zero_output: bool },
    Timeout,
    Error(String),
}

fn parse_json_body(body: Bytes) -> Result<Value, ()> {
    serde_json::from_slice(&body).map_err(|_| ())
}

async fn api_key_or_401(headers: &HeaderMap) -> Result<String, axum::response::Response> {
    if let Some(k) = extract_api_key(headers) {
        return Ok(k);
    }
    if let Some(k) = crate::credentials::cached_key() {
        return Ok(k);
    }
    Err(json_response(
        401,
        json!({
            "error": {
                "message": "Missing API key. Send in Authorization: Bearer <key> header",
                "type": "auth_error",
            }
        }),
        None,
    ))
}

// ── OpenAI /v1/chat/completions ─────────────────────────

async fn chat_completions(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let req = match parse_json_body(body) {
        Ok(v) => v,
        Err(_) => {
            return json_response(
                400,
                json!({ "error": { "message": "Invalid JSON body", "type": "invalid_request_error" } }),
                None,
            )
        }
    };
    let api_key = match api_key_or_401(&headers).await {
        Ok(k) => k,
        Err(r) => return r,
    };
    let stream = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    let model = req
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_MODEL)
        .to_string();
    let completion_id = format!("chatcmpl-{}", &uuid::Uuid::new_v4().to_string()[..12]);
    let ctx = ReqCtx {
        id: completion_id.clone(),
        path: "/v1/chat/completions",
        model: model.clone(),
        stream,
        started_at: now_millis(),
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        last_event: String::new(),
    };
    record_start(&st, &ctx);

    let cc_body = convert::build_cc_request(&req);
    cc_client::ensure_initialized(&st, &api_key).await;
    let upstream = match cc_client::forward_to_cc(&st, &cc_body, &api_key, &headers).await {
        Ok(r) => r,
        Err(e) => {
            log::error(&format!("Upstream error: {e}"));
            finish_request(&st, &ctx, "error");
            return json_response(
                502,
                json!({ "error": { "message": format!("Upstream error: {e}"), "type": "proxy_error" } }),
                Some(10),
            );
        }
    };
    if !upstream.status().is_success() {
        let status = upstream.status().as_u16();
        let text = upstream
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(500)
            .collect::<String>();
        log::error(&format!("CC API error: {status}"));
        let (mapped_status, mapped_body) = errors::map_cc_error(status, &text);
        let retry_after = mapped_body.get("retry_after").and_then(|v| v.as_u64());
        finish_request(&st, &ctx, "error");
        return json_response(mapped_status, mapped_body, retry_after);
    }

    if stream {
        handle_stream(st, upstream, ctx, Protocol::OpenAi).await
    } else {
        handle_nonstream(st, upstream, ctx, Protocol::OpenAi).await
    }
}

// ── Anthropic /v1/messages ─────────────────────────────

async fn messages(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let req = match parse_json_body(body) {
        Ok(v) => v,
        Err(_) => {
            return json_response(
                400,
                errors::anthropic_error(400, "invalid_request_error", "Invalid JSON body", None).1,
                None,
            )
        }
    };
    let api_key = match api_key_or_401(&headers).await {
        Ok(k) => k,
        Err(_) => {
            return json_response(
                401,
                errors::anthropic_error(
                    401,
                    "authentication_error",
                    "Missing API key. Send in Authorization: Bearer <key> header",
                    None,
                )
                .1,
                None,
            )
        }
    };
    let stream = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    let model = req
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("claude-sonnet-4-6")
        .to_string();
    let message_id = format!("msg_{}", &uuid::Uuid::new_v4().to_string()[..12]);
    let ctx = ReqCtx {
        id: message_id.clone(),
        path: "/v1/messages",
        model: model.clone(),
        stream,
        started_at: now_millis(),
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        last_event: String::new(),
    };
    record_start(&st, &ctx);

    let openai_req = convert::convert_anthropic_to_openai(&req);
    let cc_body = convert::build_cc_request(&openai_req);
    cc_client::ensure_initialized(&st, &api_key).await;
    let upstream = match cc_client::forward_to_cc(&st, &cc_body, &api_key, &headers).await {
        Ok(r) => r,
        Err(e) => {
            log::error(&format!("Upstream error: {e}"));
            finish_request(&st, &ctx, "error");
            return json_response(
                502,
                errors::anthropic_error(502, "proxy_error", &format!("Upstream error: {e}"), Some(10)).1,
                Some(10),
            );
        }
    };
    if !upstream.status().is_success() {
        let status = upstream.status().as_u16();
        let text = upstream
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(500)
            .collect::<String>();
        log::error(&format!("CC API error (Anthropic): {status}"));
        let (mapped_status, mapped_body) = errors::map_cc_error(status, &text);
        let err_type = mapped_body
            .pointer("/error/type")
            .and_then(|v| v.as_str())
            .unwrap_or("proxy_error");
        let msg = mapped_body
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("CC API error");
        let retry_after = mapped_body.get("retry_after").and_then(|v| v.as_u64());
        let (status2, body2) = errors::anthropic_error(mapped_status, err_type, msg, retry_after);
        finish_request(&st, &ctx, "error");
        return json_response(status2, body2, retry_after);
    }

    if stream {
        handle_stream(st, upstream, ctx, Protocol::Anthropic).await
    } else {
        handle_nonstream(st, upstream, ctx, Protocol::Anthropic).await
    }
}

// ── 流式处理 ──────────────────────────────────────────

async fn handle_stream(
    st: Arc<AppState>,
    upstream: reqwest::Response,
    ctx: ReqCtx,
    protocol: Protocol,
) -> axum::response::Response {
    let (tx, rx) = mpsc::channel::<Frame>(256);
    let st2 = st.clone();
    let model = ctx.model.clone();
    let id = ctx.id.clone();

    tokio::spawn(async move {
        match protocol {
            Protocol::OpenAi => {
                stream_openai(st2, upstream, tx, model, id).await;
            }
            Protocol::Anthropic => {
                stream_anthropic(st2, upstream, tx, model, id).await;
            }
        }
    });

    let mut prefix: Vec<String> = Vec::new();
    let mut rx = rx;
    loop {
        match rx.recv().await {
            Some(Frame::Sse(s)) => {
                if s.starts_with(": ") {
                    continue;
                }
                let is_content = match protocol {
                    Protocol::OpenAi => true,
                    Protocol::Anthropic => s.contains("\"text_delta\"") || s.contains("\"tool_use\""),
                };
                if is_content {
                    prefix.push(s);
                    return sse_response(prefix, rx, protocol, st, ctx);
                }
                prefix.push(s);
            }
            Some(Frame::Done { zero_output }) => {
                if zero_output {
                    reset_timeouts(&st);
                    finish_request(&st, &ctx, "error");
                    let (status, body, ra) = match protocol {
                        Protocol::OpenAi => {
                            let (s, b) = errors::openai_error(
                                429,
                                "rate_limit_error",
                                "Empty response from upstream (zero output tokens)",
                                Some(10),
                            );
                            (s, b, Some(10))
                        }
                        Protocol::Anthropic => {
                            let (s, b) = errors::anthropic_error(
                                429,
                                "rate_limit_error",
                                "Empty response from upstream (zero output tokens)",
                                Some(10),
                            );
                            (s, b, Some(10))
                        }
                    };
                    return json_response(status, body, ra);
                }
                reset_timeouts(&st);
                return sse_response(prefix, rx, protocol, st, ctx);
            }
            Some(Frame::Timeout) => {
                let msg = timeout_message(&st);
                finish_request(&st, &ctx, "timeout");
                return json_response(
                    429,
                    match protocol {
                        Protocol::OpenAi => {
                            errors::openai_error(429, "rate_limit_error", &msg, Some(5)).1
                        }
                        Protocol::Anthropic => {
                            errors::anthropic_error(429, "rate_limit_error", &msg, Some(5)).1
                        }
                    },
                    Some(5),
                );
            }
            Some(Frame::Error(msg)) => {
                finish_request(&st, &ctx, "error");
                return json_response(
                    502,
                    match protocol {
                        Protocol::OpenAi => {
                            errors::openai_error(502, "proxy_error", &msg, Some(10)).1
                        }
                        Protocol::Anthropic => {
                            errors::anthropic_error(502, "proxy_error", &msg, Some(10)).1
                        }
                    },
                    Some(10),
                );
            }
            None => {
                finish_request(&st, &ctx, "disconnect");
                return json_response(
                    502,
                    json!({ "error": { "message": "Upstream closed unexpectedly", "type": "proxy_error" } }),
                    Some(10),
                );
            }
        }
    }
}

async fn stream_openai(
    st: Arc<AppState>,
    upstream: reqwest::Response,
    tx: mpsc::Sender<Frame>,
    model: String,
    completion_id: String,
) {
    let mut translator = OpenAiTranslator::new(&model, &completion_id);
    let mut stream = upstream.bytes_stream();
    let mut buffer = String::new();
    let mut last_event = String::new();

    loop {
        let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => {
                log::error(&format!("Stream read error: {e}"));
                let _ = tx.send(Frame::Error(e.to_string())).await;
                return;
            }
            Ok(None) => break,
            Err(_) => {
                log::warn("Stream idle timeout");
                let _ = tx.send(Frame::Timeout).await;
                return;
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        let (complete, last) = split_lines(&buffer);
        buffer = last;
        let mut had_output = false;
        for line in &complete {
            let frames = translator.parse_line(line);
            if !frames.is_empty() {
                had_output = true;
            }
            if !translator.last_cc_event.is_empty() {
                last_event = translator.last_cc_event.clone();
            }
            for f in frames {
                if tx.send(Frame::Sse(f)).await.is_err() {
                    return;
                }
            }
        }
        if !had_output && tx.send(Frame::Sse(": keepalive\n\n".into())).await.is_err() {
            return;
        }
    }
    if !buffer.trim().is_empty() {
        for f in translator.parse_line(&buffer) {
            if tx.send(Frame::Sse(f)).await.is_err() {
                return;
            }
        }
    }
    update_ctx_tokens(&st, &completion_id, &translator.input_tokens, &translator.output_tokens, &translator.cached_tokens, &last_event);
    if translator.output_tokens == 0 {
        let _ = tx.send(Frame::Sse(translator.zero_output_error_frame())).await;
        let _ = tx.send(Frame::Done { zero_output: true }).await;
    } else {
        let _ = tx.send(Frame::Sse(translator.done_event())).await;
        let _ = tx.send(Frame::Done { zero_output: false }).await;
    }
}

async fn stream_anthropic(
    st: Arc<AppState>,
    upstream: reqwest::Response,
    tx: mpsc::Sender<Frame>,
    model: String,
    message_id: String,
) {
    let mut translator = AnthropicTranslator::new(&model, &message_id);
    if tx.send(Frame::Sse(translator.message_start())).await.is_err() {
        return;
    }
    let mut stream = upstream.bytes_stream();
    let mut buffer = String::new();
    let mut last_event = String::new();

    loop {
        let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => {
                log::error(&format!("Stream read error: {e}"));
                let _ = tx.send(Frame::Error(e.to_string())).await;
                return;
            }
            Ok(None) => break,
            Err(_) => {
                log::warn("Stream idle timeout");
                let _ = tx.send(Frame::Timeout).await;
                return;
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        let (complete, last) = split_lines(&buffer);
        buffer = last;
        let mut had_output = false;
        for line in &complete {
            let frames = translator.process_line(line);
            if !frames.is_empty() {
                had_output = true;
            }
            if !translator.last_cc_event.is_empty() {
                last_event = translator.last_cc_event.clone();
            }
            for f in frames {
                if tx.send(Frame::Sse(f)).await.is_err() {
                    return;
                }
            }
        }
        if !had_output && tx.send(Frame::Sse(": keepalive\n\n".into())).await.is_err() {
            return;
        }
    }
    if !buffer.trim().is_empty() {
        for f in translator.process_line(&buffer) {
            if tx.send(Frame::Sse(f)).await.is_err() {
                return;
            }
        }
    }
    let zero = translator.output_tokens == 0;
    for f in translator.finalize() {
        if tx.send(Frame::Sse(f)).await.is_err() {
            return;
        }
    }
    update_ctx_tokens(&st, &message_id, &translator.input_tokens, &translator.output_tokens, &translator.cached_tokens, &last_event);
    let _ = tx.send(Frame::Done { zero_output: zero }).await;
}

fn update_ctx_tokens(
    st: &AppState,
    id: &str,
    input: &u64,
    output: &u64,
    cached: &u64,
    last_event: &str,
) {
    let mut q = st.requests.lock().unwrap();
    if let Some(entry) = q.iter_mut().find(|r| r.id == id) {
        entry.input_tokens = *input;
        entry.output_tokens = *output;
        entry.cached_tokens = *cached;
        entry.last_event = last_event.to_string();
    }
}

// ── 非流式处理 ────────────────────────────────────────

async fn handle_nonstream(
    st: Arc<AppState>,
    upstream: reqwest::Response,
    ctx: ReqCtx,
    protocol: Protocol,
) -> axum::response::Response {
    let mut stream = upstream.bytes_stream();
    let mut buffer = String::new();
    let mut full_text = String::new();
    let mut reasoning = String::new();
    let mut finish_reason = "stop".to_string();
    let mut usage: Option<Value> = None;
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut last_event = String::new();

    loop {
        let chunk = match tokio::time::timeout(NONSTREAM_IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => {
                log::error(&format!("Non-stream read error: {e}"));
                finish_request(&st, &ctx, "error");
                return nonstream_error(protocol, 502, &format!("Upstream error: {e}"), Some(10));
            }
            Ok(None) => break,
            Err(_) => {
                log::warn("Non-stream idle timeout");
                finish_request(&st, &ctx, "timeout");
                let msg = timeout_message(&st);
                return nonstream_error(protocol, 429, &msg, Some(5));
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        let (complete, last) = split_lines(&buffer);
        buffer = last;
        for line in &complete {
            parse_ndjson_line(
                line,
                protocol,
                &mut full_text,
                &mut reasoning,
                &mut finish_reason,
                &mut usage,
                &mut tool_calls,
                &mut last_event,
            );
        }
    }
    if !buffer.trim().is_empty() {
        parse_ndjson_line(
            &buffer,
            protocol,
            &mut full_text,
            &mut reasoning,
            &mut finish_reason,
            &mut usage,
            &mut tool_calls,
            &mut last_event,
        );
    }

    let (mut input, output, mut cached) = usage_tokens(&usage);
    let cache_write = usage
        .as_ref()
        .and_then(|u| u.pointer("/inputTokenDetails/cacheWriteTokens"))
        .and_then(|v| v.as_u64());
    if output == 0 {
        input = 0;
        cached = 0;
    }
    if output == 0 {
        finish_request(&st, &ctx, "error");
        return nonstream_error(
            protocol,
            429,
            "Empty response from upstream (zero output tokens)",
            Some(10),
        );
    }
    reset_timeouts(&st);
    finish_request(&st, &ctx, "ok");

    match protocol {
        Protocol::OpenAi => {
            let body = convert::build_openai_response(
                &ctx.id,
                &ctx.model,
                &full_text,
                &reasoning,
                if tool_calls.is_empty() { None } else { Some(&tool_calls) },
                &finish_reason,
                input,
                output,
                cached,
            );
            json_response(200, body, None)
        }
        Protocol::Anthropic => {
            let body = convert::build_anthropic_response(
                &ctx.id,
                &ctx.model,
                &full_text,
                if tool_calls.is_empty() { None } else { Some(&tool_calls) },
                &finish_reason,
                input,
                output,
                cached,
                cache_write,
            );
            json_response(200, body, None)
        }
    }
}

/// 将缓冲按行拆分：返回完整行 + 最后未完结片段，避免借用冲突。
fn split_lines(buffer: &str) -> (Vec<String>, String) {
    let split: Vec<&str> = buffer.split('\n').collect();
    if split.is_empty() {
        return (Vec::new(), String::new());
    }
    let (complete, last) = split.split_at(split.len() - 1);
    (
        complete.iter().map(|s| s.to_string()).collect(),
        last[0].to_string(),
    )
}

fn nonstream_error(protocol: Protocol, status: u16, msg: &str, retry_after: Option<u64>) -> axum::response::Response {
    let (status, body) = match protocol {
        Protocol::OpenAi => errors::openai_error(status, if status == 429 { "rate_limit_error" } else { "proxy_error" }, msg, retry_after),
        Protocol::Anthropic => errors::anthropic_error(status, if status == 429 { "rate_limit_error" } else { "proxy_error" }, msg, retry_after),
    };
    json_response(status, body, retry_after)
}

fn usage_tokens(usage: &Option<Value>) -> (u64, u64, u64) {
    let u = match usage {
        Some(u) => u,
        None => return (0, 0, 0),
    };
    (
        u.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
        u.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
        u.get("cachedInputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
    )
}

#[allow(clippy::too_many_arguments)]
fn parse_ndjson_line(
    line: &str,
    protocol: Protocol,
    full_text: &mut String,
    reasoning: &mut String,
    finish_reason: &mut String,
    usage: &mut Option<Value>,
    tool_calls: &mut Vec<Value>,
    last_event: &mut String,
) {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" || trimmed.starts_with(':') {
        return;
    }
    let event: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return,
    };
    let event_type = match event.get("type").and_then(|t| t.as_str()) {
        Some(t) => t.to_string(),
        None => return,
    };
    *last_event = event_type.clone();
    match event_type.as_str() {
        "text-start" | "reasoning-start" | "start" | "start-step" | "finish-step" => {}
        "text-delta" => {
            full_text.push_str(event.get("text").and_then(|t| t.as_str()).unwrap_or(""));
        }
        "reasoning-delta" => {
            if matches!(protocol, Protocol::OpenAi) {
                reasoning.push_str(event.get("text").and_then(|t| t.as_str()).unwrap_or(""));
            }
        }
        "tool-call" => {
            let id = event
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("call_{}", &uuid::Uuid::new_v4().to_string()[..8]));
            let name = event.get("toolName").and_then(|v| v.as_str()).unwrap_or("");
            let args = match event.get("input") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => "{}".to_string(),
            };
            tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": args },
            }));
        }
        "finish" => {
            *finish_reason = convert::map_finish_reason(event.get("finishReason").and_then(|v| v.as_str()).unwrap_or("stop"));
            if let Some(u) = event.get("totalUsage").cloned() {
                *usage = Some(u);
            }
        }
        "error" => {
            let msg = event
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .or_else(|| event.get("message").and_then(|v| v.as_str()))
                .unwrap_or("Unknown error");
            log::warn(&format!("CC error (non-stream): {msg}"));
        }
        "reasoning-end" | "provider-metadata" | "tool-input-start" | "tool-input-delta" | "tool-input-end" | "tool-error" | "text-end" => {}
        other => {
            log::warn(&format!("Unknown CC event type: {other}"));
        }
    }
}

// ── 其他路由 ──────────────────────────────────────────

async fn models(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let api_key = extract_api_key(&headers)
        .or_else(crate::credentials::cached_key);
    let (list, _) = cc_client::fetch_models(&st, api_key.as_deref()).await;
    Json(json!({
        "object": "list",
        "data": list
            .iter()
            .map(|m| json!({
                "id": m.id,
                "object": "model",
                "created": now_secs(),
                "owned_by": "command-code",
            }))
            .collect::<Vec<_>>(),
    }))
    .into_response()
}

async fn health() -> &'static str {
    "OK"
}

async fn not_found() -> axum::response::Response {
    json_response(
        404,
        json!({ "error": { "message": "Not found", "type": "not_found" } }),
        None,
    )
}

pub fn listen_addr(cfg: &Config) -> SocketAddr {
    let host: std::net::IpAddr = cfg
        .host
        .parse()
        .unwrap_or_else(|_| "0.0.0.0".parse().unwrap());
    SocketAddr::new(host, config_port(cfg))
}
