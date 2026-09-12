//! 本地 HTTP 服务：路由分发、鉴权、流式/非流式转发与协议错误处理。
//!
//! 三种下游协议入口（/v1/chat/completions、/v1/messages、/v1/responses）统一走
//! “转换为 CC 信封 → 转发上游 → 按协议翻译回 SSE/JSON”的流程；流式路径通过
//! mpsc 通道把后台读流任务产出的帧交给响应流，实现首帧前可回退为普通 JSON 错误。

use axum::body::Body;
use axum::extract::State;
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
use super::sse::{AnthropicTranslator, OpenAiTranslator, ResponsesTranslator};
use super::state::{now_millis, now_secs, AppState, RequestInfo};
use super::usage::UsageEntry;

/// 流式响应两次上游数据之间的最大空闲时间，超时判定为 Timeout。
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// 非流式响应等待上游完整结果的最大空闲时间。
const NONSTREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// 连续超时达到该阈值后，超时错误消息提示客户端缩减上下文长度。
const TIMEOUT_REDUCE_CONTEXT_THRESHOLD: u32 = 3;
/// Anthropic 流式静默超过该时长时主动发 ping 事件保活（覆盖上游排队/长思考窗口）。
const ANTHROPIC_PING_IDLE: Duration = Duration::from_secs(15);

/// 合法 API Key 的正则（OnceLock 惰性编译一次，避免每次请求重复构建）。
static KEY_RE: OnceLock<regex::Regex> = OnceLock::new();

/// 获取已编译的 API Key 匹配正则 `user_[A-Za-z0-9_-]+`。
fn key_re() -> &'static regex::Regex {
    KEY_RE.get_or_init(|| regex::Regex::new(r"user_[A-Za-z0-9_-]+").unwrap())
}

/// 请求事件转发器（Tauri 注入，供前端中继轨道展示）。
static REQUEST_SINK: Mutex<Option<Box<dyn Fn(&RequestInfo) + Send + Sync>>> = Mutex::new(None);

/// 注册请求事件转发回调（Tauri setup 阶段调用，用于向前端实时推送请求状态）。
pub fn set_request_sink<F>(f: F)
where
    F: Fn(&RequestInfo) + Send + Sync + 'static,
{
    *REQUEST_SINK.lock().unwrap() = Some(Box::new(f));
}

/// 把一条请求摘要推送给已注册的 sink（未注册则忽略）。
fn emit_request(info: &RequestInfo) {
    if let Some(sink) = REQUEST_SINK.lock().unwrap().as_ref() {
        sink(info);
    }
}

/// 从请求头提取合法 API Key（user_ 前缀片段）。
///
/// 优先 `Authorization: Bearer <key>`（OpenAI SDK 风格），回退 `x-api-key` 头
/// （Anthropic SDK 风格）；无效返回 None。
pub(crate) fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    let from_auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|a| a.strip_prefix("Bearer "))
        .and_then(|k| key_re().find(k).map(|m| m.as_str().to_string()));
    if from_auth.is_some() {
        return from_auth;
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .and_then(|k| key_re().find(k).map(|m| m.as_str().to_string()))
}

/// 读取配置中的代理监听端口。
fn config_port(cfg: &Config) -> u16 {
    cfg.port
}

/// 组装全部路由：三个协议入口 + /v1/models + /health，附请求体大小限制与宽松 CORS。
///
/// 请求体大小由各 handler 的 read_json_body 按 config.max_body_mb 限制（超限 413 并排空），
/// 不再依赖 DefaultBodyLimit；并挂载在途请求计数中间件（config.max_inflight > 0 时超限 503）。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/v1/responses", post(responses))
        .route("/v1/models", get(models))
        .route("/health", get(health))
        .route("/", get(health))
        .fallback(not_found)
        .layer(axum::middleware::from_fn_with_state(state.clone(), inflight_guard))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state)
}

/// 在途请求计数中间件：`max_inflight > 0` 时对业务路径（/health、/ 除外）计数，
/// 超限直接返回 503 server_busy + Retry-After: 5（OpenAI/Anthropic SDK 认得并自动退避）。
async fn inflight_guard(
    State(st): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // 探活路径不计数、不受限
    let path = req.uri().path().to_string();
    if path == "/health" || path == "/" {
        return next.run(req).await;
    }
    let max = st.config.read().unwrap().max_inflight;
    if max == 0 {
        return next.run(req).await;
    }
    let cur = st.inflight.fetch_add(1, Ordering::SeqCst) + 1;
    if cur > max {
        st.inflight.fetch_sub(1, Ordering::SeqCst);
        return json_response(
            503,
            json!({
                "error": { "message": "Server busy, too many in-flight requests", "type": "server_busy" },
                "retry_after": 5,
            }),
            Some(5),
        );
    }
    let resp = next.run(req).await;
    st.inflight.fetch_sub(1, Ordering::SeqCst);
    resp
}

/// 在给定 listener 上启动服务，收到 `shutdown` 信号后优雅停机。
///
/// - `with_background`：是否启动后台任务（24h 刷新 CLI 版本、每小时清理过期会话）。
///   集成测试传 false 以避免后台网络请求干扰。
pub async fn serve(
    listener: TcpListener,
    state: Arc<AppState>,
    shutdown: oneshot::Receiver<()>,
    with_background: bool,
) -> io::Result<()> {
    let addr = listener.local_addr().unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap());
    state.mark_started();
    log::info(&format!("68proxy listening on http://{}", addr));
    // 启动状态打印：会话策略、配置开关、空闲超时、在途上限与请求体内存告警。
    {
        let cfg = state.config.read().unwrap().clone();
        log::info(&format!(
            "session: 12h + 1h jitter per API key | zdr: {} | emptySystemPlaceholder: {} | model refresh: {}s | stream idle: {}s | nonstream idle: {}s",
            cfg.zdr,
            cfg.empty_system_placeholder,
            cfg.model_refresh_interval_ms / 1000,
            STREAM_IDLE_TIMEOUT.as_secs(),
            NONSTREAM_IDLE_TIMEOUT.as_secs(),
        ));
        if cfg.client_drain_timeout_ms > 0 {
            log::info(&format!(
                "client drain watchdog enabled ({}ms): stalled clients will be disconnected",
                cfg.client_drain_timeout_ms
            ));
        }
        if cfg.max_inflight > 0 {
            log::info(&format!(
                "in-flight request cap enabled (max {}): overflow returns 503 + Retry-After",
                cfg.max_inflight
            ));
        }
        // 请求体内存告警：转发前存在多份副本，峰值 ≈ body × 5.1~7.4（取 5.5 估算），
        // 隐含最坏单请求峰值 ≥ 500MB 时提示。
        let worst_mb = (cfg.max_body_mb as f64) * 5.5;
        if worst_mb >= 500.0 {
            log::warn(&format!(
                "memory warning: max_body_mb={} implies worst-case ~{}MB per request; consider lowering CC_MAX_BODY_MB or adding an in-flight cap",
                cfg.max_body_mb,
                worst_mb.round() as u64
            ));
        }
    }

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

/// 构造 JSON 响应，`retry_after` 存在时附带 `Retry-After` 头。
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

/// 把已缓存的首帧 `prefix` 与后台读流任务经 `rx` 送来的后续帧拼接为 SSE 响应。
///
/// 收到 Done 帧时结束流并记 ok；Timeout/Error 帧则先下发按协议构造的错误帧再终止，
/// 并借 `pending` 状态保证错误帧发出后才结束迭代。
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
                // Anthropic 客户端会吞掉 signal 事件、正文前可能长时间静默（上游排队/
                // 长思考）：静默超过阈值时主动发标准 ping 事件（官方 SDK 会忽略）保活。
                let next = if matches!(protocol, Protocol::Anthropic) {
                    match tokio::time::timeout(ANTHROPIC_PING_IDLE, rx.recv()).await {
                        Ok(v) => Some(v),
                        Err(_) => {
                            return Some((
                                Ok::<_, Infallible>("event: ping\ndata: {\"type\":\"ping\"}\n\n".to_string()),
                                (rx, protocol, st, ctx, ended, None),
                            ));
                        }
                    }
                } else {
                    Some(rx.recv().await)
                };
                match next {
                    Some(Some(Frame::Sse(s))) => {
                        return Some((Ok(s), (rx, protocol, st, ctx, ended, None)));
                    }
                    Some(Some(Frame::Done { .. })) => {
                        finish_request(&st, &ctx, "ok");
                        return None;
                    }
                    Some(Some(Frame::Timeout)) => {
                        finish_request(&st, &ctx, "timeout");
                        pending = Some(protocol_error_frame(protocol, &timeout_message(&st), None));
                        ended = true;
                    }
                    Some(Some(Frame::Error(msg))) => {
                        finish_request(&st, &ctx, "error");
                        pending = Some(protocol_error_frame(protocol, &msg, None));
                        ended = true;
                    }
                    Some(None) | None => return None,
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

/// 下游协议类型，决定错误帧/错误响应的构造格式。
#[derive(Clone, Copy)]
enum Protocol {
    /// OpenAI Chat Completions。
    OpenAi,
    /// Anthropic Messages。
    Anthropic,
    /// OpenAI Responses。
    Responses,
}

/// 按协议构造流内错误帧（响应头已发出、无法改状态码时使用）。
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
        Protocol::Responses => format!(
            "event: response.failed\ndata: {}\n\n",
            json!({ "type": "response.failed", "response": {
                "object": "response", "status": "failed",
                "error": { "type": "server_error", "message": msg },
            } })
        ),
    }
}

/// 生成超时错误消息：连续超时达到阈值（上下文过长）时给出缩减上下文的建议。
/// 副作用：连续超时计数加 1。
fn timeout_message(st: &AppState) -> String {
    let n = st.consecutive_timeouts.fetch_add(1, Ordering::SeqCst) + 1;
    if n >= TIMEOUT_REDUCE_CONTEXT_THRESHOLD {
        "Response timeout - try reducing context length (summarize earlier messages)".into()
    } else {
        "Response timeout - request timed out".into()
    }
}

/// 请求成功后清零连续超时计数。
fn reset_timeouts(st: &AppState) {
    st.consecutive_timeouts.store(0, Ordering::SeqCst);
}

/// 请求开始时登记初始摘要（status=streaming）并推送给前端。
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

/// 请求结束时按 id 回填队列中对应条目的状态/耗时/token，并把最终摘要推送给前端。
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

/// 单次请求的上下文：贯穿 handler → 流式任务 → 结束回填的生命周期数据。
#[derive(Clone)]
struct ReqCtx {
    /// 请求 ID（与下游响应体中的 id 一致）。
    id: String,
    /// 入口路径。
    path: &'static str,
    /// 请求模型名。
    model: String,
    /// 是否流式。
    stream: bool,
    /// 开始时间（Unix 毫秒）。
    started_at: u64,
    /// 结束时回填：输入 token。
    input_tokens: u64,
    /// 结束时回填：输出 token。
    output_tokens: u64,
    /// 结束时回填：缓存命中 token。
    cached_tokens: u64,
    /// 结束时回填：最后的 CC 事件类型。
    last_event: String,
}

/// 后台读流任务经 mpsc 通道传给响应流的帧类型。
enum Frame {
    /// 一条可直接下发的 SSE 文本帧。
    Sse(String),
    /// 上游流正常结束；`zero_output` 标记是否零输出（需改判限流）。
    Done { zero_output: bool },
    /// 上游空闲超时。
    Timeout,
    /// 上游读流出错，携带错误消息。
    Error(String),
}

/// 发送帧到下游通道。返回 false 表示下游不可写（客户端断连，或僵死超过
/// `drain_timeout`），调用方应立即中止上游并退出。
///
/// `client_drain_timeout_ms` 为 0 时禁用僵死看门狗，仅依赖通道背压与断连检测。
async fn send_frame(tx: &mpsc::Sender<Frame>, frame: Frame, drain_timeout: Duration) -> bool {
    if drain_timeout.is_zero() {
        return tx.send(frame).await.is_ok();
    }
    match tokio::time::timeout(drain_timeout, tx.send(frame)).await {
        Ok(Ok(())) => true,
        _ => {
            log::warn("Client stalled on backpressure, dropping connection");
            false
        }
    }
}

/// 流式读取请求体为 JSON。超过 `max_size` 时返回 413——剩余请求体交由
/// hyper 自动排空（丢弃未读数据），使连接保持 keep-alive 可复用，
/// 客户端收到明确的 413 而非 Connection reset。
async fn read_json_body(
    body: axum::body::Body,
    max_size: usize,
) -> Result<Value, (u16, &'static str, &'static str)> {
    use futures_util::StreamExt as _;
    let mut stream = body.into_data_stream();
    let mut chunks: Vec<Bytes> = Vec::new();
    let mut total = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(_) => return Err((400, "invalid_request_error", "Invalid JSON body")),
        };
        total += chunk.len();
        if total > max_size {
            return Err((413, "invalid_request_error", "Request body exceeds limit"));
        }
        chunks.push(chunk);
    }
    let bytes: Vec<u8> = chunks.into_iter().flatten().collect();
    serde_json::from_slice(&bytes).map_err(|_| (400, "invalid_request_error", "Invalid JSON body"))
}

/// 鉴权：优先取请求头中的 API Key，其次用本地缓存的 Key；都没有则返回 401 响应。
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

/// OpenAI Chat Completions 入口：转换请求 → 转发 CC → 按 stream 走 SSE 或 JSON 路径。
/// 上游非 2xx 经 map_cc_error 映射为下游状态码。
async fn chat_completions(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> axum::response::Response {
    let max_body = (st.config.read().unwrap().max_body_mb as usize) * 1024 * 1024;
    let req = match read_json_body(body, max_body).await {
        Ok(v) => v,
        Err((status, err_type, msg)) => {
            return json_response(
                status,
                json!({ "error": { "message": msg, "type": err_type } }),
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
    let prompt_cache_key = req
        .get("prompt_cache_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
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

    let empty_placeholder = st.config.read().unwrap().empty_system_placeholder;
    let cc_body = convert::build_cc_request(&req, empty_placeholder);
    cc_client::ensure_initialized(&st, &api_key).await;
    let upstream = match cc_client::forward_to_cc(&st, &cc_body, &api_key, &headers, prompt_cache_key).await {
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
        log::error(&format!("CC API error: {status} — {}", summarize_upstream_error(&text)));
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

/// Anthropic Messages 入口：先转成 OpenAI Chat 格式再复用 CC 转换与转发，
/// 错误响应按 Anthropic 格式重新包装。
async fn messages(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> axum::response::Response {
    let max_body = (st.config.read().unwrap().max_body_mb as usize) * 1024 * 1024;
    let req = match read_json_body(body, max_body).await {
        Ok(v) => v,
        Err((status, err_type, msg)) => {
            return json_response(
                status,
                errors::anthropic_error(status, err_type, msg, None).1,
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
    let empty_placeholder = st.config.read().unwrap().empty_system_placeholder;
    let cc_body = convert::build_cc_request(&openai_req, empty_placeholder);
    cc_client::ensure_initialized(&st, &api_key).await;
    let upstream = match cc_client::forward_to_cc(&st, &cc_body, &api_key, &headers, None).await {
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
        log::error(&format!("CC API error (Anthropic): {status} — {}", summarize_upstream_error(&text)));
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

// ── OpenAI Responses /v1/responses ─────────────────────

/// OpenAI Responses 入口（Codex CLI 等）：先转成 OpenAI Chat 格式再复用 CC 转换与转发。
async fn responses(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> axum::response::Response {
    let max_body = (st.config.read().unwrap().max_body_mb as usize) * 1024 * 1024;
    let req = match read_json_body(body, max_body).await {
        Ok(v) => v,
        Err((status, err_type, msg)) => {
            return json_response(
                status,
                json!({ "error": { "message": msg, "type": err_type } }),
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
    // 本代理无状态、不保存会话：显式拒绝 previous_response_id，避免静默降级给出错误答案
    if req.get("previous_response_id").map(|v| !v.is_null()).unwrap_or(false) {
        return json_response(
            400,
            json!({ "error": {
                "message": "previous_response_id is not supported (this proxy is stateless); send the full input each turn",
                "type": "invalid_request_error",
            } }),
            None,
        );
    }
    let prompt_cache_key = req
        .get("prompt_cache_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let response_id = format!("resp_{}", &uuid::Uuid::new_v4().to_string()[..12]);
    let ctx = ReqCtx {
        id: response_id.clone(),
        path: "/v1/responses",
        model: model.clone(),
        stream,
        started_at: now_millis(),
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        last_event: String::new(),
    };
    record_start(&st, &ctx);

    let openai_req = convert::convert_responses_to_openai(&req);
    let empty_placeholder = st.config.read().unwrap().empty_system_placeholder;
    let cc_body = convert::build_cc_request(&openai_req, empty_placeholder);
    cc_client::ensure_initialized(&st, &api_key).await;
    let upstream = match cc_client::forward_to_cc(&st, &cc_body, &api_key, &headers, prompt_cache_key).await {
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
        log::error(&format!("CC API error (Responses): {status} — {}", summarize_upstream_error(&text)));
        let (mapped_status, mapped_body) = errors::map_cc_error(status, &text);
        let retry_after = mapped_body.get("retry_after").and_then(|v| v.as_u64());
        finish_request(&st, &ctx, "error");
        return json_response(mapped_status, mapped_body, retry_after);
    }

    if stream {
        handle_stream(st, upstream, ctx, Protocol::Responses).await
    } else {
        handle_nonstream(st, upstream, ctx, Protocol::Responses).await
    }
}

// ── 流式处理 ──────────────────────────────────────────

/// 流式响应总控：spawn 后台任务读上游 NDJSON 并按协议翻译，经 mpsc 转发帧。
///
/// 关键点：先消费通道直到出现首帧“内容帧”才确定升级为 SSE——在此之前若发生
/// 超时/错误/零输出，仍可回退为携带正确状态码的普通 JSON 错误响应。
/// keepalive 注释帧（": " 开头）不算内容帧，仅用于维持上游活跃。
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
                stream_openai(st2, upstream, tx, model, id, "/v1/chat/completions").await;
            }
            Protocol::Anthropic => {
                stream_anthropic(st2, upstream, tx, model, id, "/v1/messages").await;
            }
            Protocol::Responses => {
                stream_responses(st2, upstream, tx, model, id, "/v1/responses").await;
            }
        }
    });

    // 首帧确认循环：缓存前缀帧，直到出现首个“内容帧”才切到 SSE 输出
    let mut prefix: Vec<String> = Vec::new();
    let mut rx = rx;
    loop {
        match rx.recv().await {
            Some(Frame::Sse(s)) => {
                // SSE 注释帧（keepalive）不推进状态，直接丢弃
                if s.starts_with(": ") {
                    continue;
                }
                // 各协议“真正携带模型输出”的帧特征，决定从该帧起升级为 SSE
                let is_content = match protocol {
                    Protocol::OpenAi => true,
                    Protocol::Anthropic => s.contains("\"text_delta\"") || s.contains("\"tool_use\""),
                    Protocol::Responses => {
                        s.contains("response.output_item.added")
                            || s.contains("response.output_text.delta")
                            || s.contains("response.function_call_arguments.delta")
                    }
                };
                if is_content {
                    prefix.push(s);
                    return sse_response(prefix, rx, protocol, st, ctx);
                }
                prefix.push(s);
            }
            Some(Frame::Done { zero_output }) => {
                if zero_output {
                    // 上游未产出任何内容：尚未发 SSE 头，直接以 JSON 429 限流响应回退
                    reset_timeouts(&st);
                    finish_request(&st, &ctx, "error");
                    let (status, body, ra) = match protocol {
                        Protocol::OpenAi | Protocol::Responses => {
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
                        Protocol::OpenAi | Protocol::Responses => {
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
                        Protocol::OpenAi | Protocol::Responses => {
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

/// 后台任务：读上游字节流（按空闲超时），逐行经 OpenAiTranslator 翻译后发帧。
///
/// 本轮 chunk 未产出任何帧时补发 keepalive 注释帧维持下游连接；
/// 结束后按零输出与否发 Done，接收方若已断开（send 失败）则直接退出。
async fn stream_openai(
    st: Arc<AppState>,
    upstream: reqwest::Response,
    tx: mpsc::Sender<Frame>,
    model: String,
    completion_id: String,
    endpoint: &'static str,
) {
    let drain = Duration::from_millis(st.config.read().unwrap().client_drain_timeout_ms);
    let mut translator = OpenAiTranslator::new(&model, &completion_id);
    let mut stream = upstream.bytes_stream();
    let mut buffer = String::new();
    let mut last_event = String::new();

    loop {
        // 下游已断开（接收端 drop 通道）时立即停止读取并关闭上游连接
        if tx.is_closed() {
            log::info("Client disconnected, aborting upstream");
            return;
        }
        let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => {
                log::error(&format!("Stream read error: {e}"));
                let _ = send_frame(&tx, Frame::Error(e.to_string()), drain).await;
                return;
            }
            Ok(None) => break,
            Err(_) => {
                log::warn("Stream idle timeout");
                let _ = send_frame(&tx, Frame::Timeout, drain).await;
                return;
            }
        };
        let complete = push_and_split(&mut buffer, &String::from_utf8_lossy(&chunk));
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
                if !send_frame(&tx, Frame::Sse(f), drain).await {
                    return;
                }
            }
        }
        if !had_output && !send_frame(&tx, Frame::Sse(": keepalive\n\n".into()), drain).await {
            return;
        }
    }
    if !buffer.trim().is_empty() {
        for f in translator.parse_line(&buffer) {
            if !send_frame(&tx, Frame::Sse(f), drain).await {
                return;
            }
        }
    }
    update_ctx_tokens(&st, &completion_id, &translator.input_tokens, &translator.output_tokens, &translator.cached_tokens, &last_event);
    record_usage_entry(&st, &model, endpoint, translator.input_tokens, translator.output_tokens, translator.cached_tokens, 0, true);
    if translator.output_tokens == 0 {
        let _ = send_frame(&tx, Frame::Sse(translator.zero_output_error_frame()), drain).await;
        let _ = send_frame(&tx, Frame::Done { zero_output: true }, drain).await;
    } else {
        let _ = send_frame(&tx, Frame::Sse(translator.done_event()), drain).await;
        let _ = send_frame(&tx, Frame::Done { zero_output: false }, drain).await;
    }
}

/// 后台任务：读上游字节流并经 AnthropicTranslator 翻译（流程同 stream_openai，
/// 额外先发 message_start、结束时经 finalize 补发 message_delta/message_stop）。
async fn stream_anthropic(
    st: Arc<AppState>,
    upstream: reqwest::Response,
    tx: mpsc::Sender<Frame>,
    model: String,
    message_id: String,
    endpoint: &'static str,
) {
    let drain = Duration::from_millis(st.config.read().unwrap().client_drain_timeout_ms);
    let mut translator = AnthropicTranslator::new(&model, &message_id);
    if !send_frame(&tx, Frame::Sse(translator.message_start()), drain).await {
        return;
    }
    let mut stream = upstream.bytes_stream();
    let mut buffer = String::new();
    let mut last_event = String::new();

    loop {
        // 下游已断开（接收端 drop 通道）时立即停止读取并关闭上游连接
        if tx.is_closed() {
            log::info("Client disconnected, aborting upstream");
            return;
        }
        let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => {
                log::error(&format!("Stream read error: {e}"));
                let _ = send_frame(&tx, Frame::Error(e.to_string()), drain).await;
                return;
            }
            Ok(None) => break,
            Err(_) => {
                log::warn("Stream idle timeout");
                let _ = send_frame(&tx, Frame::Timeout, drain).await;
                return;
            }
        };
        let complete = push_and_split(&mut buffer, &String::from_utf8_lossy(&chunk));
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
                if !send_frame(&tx, Frame::Sse(f), drain).await {
                    return;
                }
            }
        }
        if !had_output && !send_frame(&tx, Frame::Sse(": keepalive\n\n".into()), drain).await {
            return;
        }
    }
    if !buffer.trim().is_empty() {
        for f in translator.process_line(&buffer) {
            if !send_frame(&tx, Frame::Sse(f), drain).await {
                return;
            }
        }
    }
    let zero = translator.output_tokens == 0;
    for f in translator.finalize() {
        if !send_frame(&tx, Frame::Sse(f), drain).await {
            return;
        }
    }
    update_ctx_tokens(&st, &message_id, &translator.input_tokens, &translator.output_tokens, &translator.cached_tokens, &last_event);
    record_usage_entry(
        &st,
        &model,
        endpoint,
        translator.input_tokens,
        translator.output_tokens,
        translator.cached_tokens,
        translator.cache_write_tokens.unwrap_or(0),
        true,
    );
    let _ = send_frame(&tx, Frame::Done { zero_output: zero }, drain).await;
}

/// 后台任务：读上游字节流并经 ResponsesTranslator 翻译（流程同 stream_openai，
/// 额外先发 response.created、结束时经 finalize 补发 response.completed/failed）。
async fn stream_responses(
    st: Arc<AppState>,
    upstream: reqwest::Response,
    tx: mpsc::Sender<Frame>,
    model: String,
    response_id: String,
    endpoint: &'static str,
) {
    let drain = Duration::from_millis(st.config.read().unwrap().client_drain_timeout_ms);
    let mut translator = ResponsesTranslator::new(&model, &response_id);
    if !send_frame(&tx, Frame::Sse(translator.response_start()), drain).await {
        return;
    }
    let mut stream = upstream.bytes_stream();
    let mut buffer = String::new();
    let mut last_event = String::new();

    loop {
        // 下游已断开（接收端 drop 通道）时立即停止读取并关闭上游连接
        if tx.is_closed() {
            log::info("Client disconnected, aborting upstream");
            return;
        }
        let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => {
                log::error(&format!("Stream read error: {e}"));
                let _ = send_frame(&tx, Frame::Error(e.to_string()), drain).await;
                return;
            }
            Ok(None) => break,
            Err(_) => {
                log::warn("Stream idle timeout");
                let _ = send_frame(&tx, Frame::Timeout, drain).await;
                return;
            }
        };
        let complete = push_and_split(&mut buffer, &String::from_utf8_lossy(&chunk));
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
                if !send_frame(&tx, Frame::Sse(f), drain).await {
                    return;
                }
            }
        }
        if !had_output && !send_frame(&tx, Frame::Sse(": keepalive\n\n".into()), drain).await {
            return;
        }
    }
    if !buffer.trim().is_empty() {
        for f in translator.process_line(&buffer) {
            if !send_frame(&tx, Frame::Sse(f), drain).await {
                return;
            }
        }
    }
    let zero = translator.output_tokens == 0;
    for f in translator.finalize() {
        if !send_frame(&tx, Frame::Sse(f), drain).await {
            return;
        }
    }
    update_ctx_tokens(&st, &response_id, &translator.input_tokens, &translator.output_tokens, &translator.cached_tokens, &last_event);
    record_usage_entry(&st, &model, endpoint, translator.input_tokens, translator.output_tokens, translator.cached_tokens, 0, true);
    let _ = send_frame(&tx, Frame::Done { zero_output: zero }, drain).await;
}

/// 流结束后把上游回报的 token 统计与最后事件类型回填到请求队列对应条目。
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

/// 记录一条成功的 token 用量到统计库（token 真实值出现点调用）。
///
/// 对齐 9router 的保存约定：输入/输出均为 0 的请求（如上游空响应）不记，
/// 失败/超时/断连请求也不计入用量统计；成本由单价表实时估算。
fn record_usage_entry(
    st: &AppState,
    model: &str,
    endpoint: &str,
    prompt: u64,
    completion: u64,
    cached: u64,
    cache_write: u64,
    stream: bool,
) {
    if prompt == 0 && completion == 0 {
        return;
    }
    // 配置关闭统计时不再记录
    let enabled = st.config.read().unwrap().usage_enabled;
    if !enabled {
        return;
    }
    st.record_usage(&UsageEntry {
        ts: now_millis(),
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        status: "ok".into(),
        prompt_tokens: prompt,
        completion_tokens: completion,
        cached_tokens: cached,
        cache_write_tokens: cache_write,
        stream,
    });
    // 保留策略：按天清理超期明细（0 表示永久保留，不执行）
    let retention = st.config.read().unwrap().usage_retention_days;
    if retention > 0 {
        if let Err(e) = st.prune_usage(retention) {
            log::warn(&format!("清理过期用量失败: {e}"));
        }
    }
}

// ── 非流式处理 ────────────────────────────────────────

/// 非流式响应：读完整上游 NDJSON，聚合文本/推理/工具调用/usage 后一次性构造响应。
///
/// 上游本身始终以 NDJSON 流式返回，非流式只是把整条流聚合后再回给下游；
/// 零输出视为上游空响应返回 429，成功时按协议调用对应的 build_*_response。
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
        let complete = push_and_split(&mut buffer, &String::from_utf8_lossy(&chunk));
        for line in &complete {
            parse_ndjson_line(
                line,
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
        // usage 未回报输出：输入/缓存计数一并清零，避免无效请求计入统计
        input = 0;
        cached = 0;
    }
    // 零输出判定：chat 端按 usage 回报值；Anthropic/Responses 按实际内容——
    // 上游偶发不回 totalUsage 时，按 usage 判定会把有完整文本的响应误杀成 429。
    let empty = match protocol {
        Protocol::OpenAi => output == 0,
        Protocol::Anthropic | Protocol::Responses => {
            full_text.is_empty() && reasoning.is_empty() && tool_calls.is_empty()
        }
    };
    if empty {
        finish_request(&st, &ctx, "error");
        return nonstream_error(
            protocol,
            429,
            "Empty response from upstream (zero output tokens)",
            Some(10),
        );
    }
    reset_timeouts(&st);
    record_usage_entry(
        &st,
        &ctx.model,
        ctx.path,
        input,
        output,
        cached,
        cache_write.unwrap_or(0),
        false,
    );
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
                &reasoning,
                if tool_calls.is_empty() { None } else { Some(&tool_calls) },
                &finish_reason,
                input,
                output,
                cached,
                cache_write,
            );
            json_response(200, body, None)
        }
        Protocol::Responses => {
            let body = convert::build_responses_response(
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

/// 追加新到的数据并按需切分：仅当新数据含换行时才做行切分。
///
/// `buffer` 中永不残留 `\n`，故无换行即无完整行；避免对增长中的超长单行
/// （大 tool-call / tool-result）反复做全量 split —— O(n²) → O(n)。
fn push_and_split(buffer: &mut String, chunk: &str) -> Vec<String> {
    buffer.push_str(chunk);
    if !chunk.contains('\n') {
        return Vec::new();
    }
    let (complete, last) = split_lines(buffer);
    *buffer = last;
    complete
}

/// 把上游错误体摘要成单行，便于日志排查：压掉换行、截断到 500 字符，
/// 避免异常大的 body 刷爆日志，同时保证一条日志一行。
fn summarize_upstream_error(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat.trim();
    const LIMIT: usize = 500;
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() > LIMIT {
        let head: String = chars[..LIMIT].iter().collect();
        format!("{head}…({} more)", chars.len() - LIMIT)
    } else {
        flat.to_string()
    }
}

/// 非流式路径的协议化错误响应：429 记限流类型，其余记代理错误类型。
fn nonstream_error(protocol: Protocol, status: u16, msg: &str, retry_after: Option<u64>) -> axum::response::Response {
    let (status, body) = match protocol {
        Protocol::OpenAi | Protocol::Responses => errors::openai_error(status, if status == 429 { "rate_limit_error" } else { "proxy_error" }, msg, retry_after),
        Protocol::Anthropic => errors::anthropic_error(status, if status == 429 { "rate_limit_error" } else { "proxy_error" }, msg, retry_after),
    };
    json_response(status, body, retry_after)
}

/// 从 CC usage 对象提取 (输入, 输出, 缓存命中) token 数，缺失一律按 0。
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

/// 非流式聚合：解析单行 CC NDJSON 事件并就地累积到各聚合器（参数多因此豁免 clippy）。
///
/// text-delta 拼文本；reasoning-delta 仅 OpenAI 协议保留；tool-call 归一为
/// OpenAI tool_call 结构；finish 记录 finish_reason 与 totalUsage。
#[allow(clippy::too_many_arguments)]
fn parse_ndjson_line(
    line: &str,
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
            // 三种协议都保留推理文本：OpenAI 走 reasoning_content，Responses 走
            // reasoning 条目，Anthropic 走 thinking 内容块。
            reasoning.push_str(event.get("text").and_then(|t| t.as_str()).unwrap_or(""));
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

/// GET /v1/models：返回模型列表（OpenAI list 格式），Key 缺失时仍可回退硬编码列表。
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

/// GET /health：存活探针，恒返回 "OK"。
async fn health() -> &'static str {
    "OK"
}

/// 未匹配路由的统一 404 JSON 响应。
async fn not_found() -> axum::response::Response {
    json_response(
        404,
        json!({ "error": { "message": "Not found", "type": "not_found" } }),
        None,
    )
}

/// 由配置计算监听地址，host 非法时回退 0.0.0.0。
pub fn listen_addr(cfg: &Config) -> SocketAddr {
    let host: std::net::IpAddr = cfg
        .host
        .parse()
        .unwrap_or_else(|_| "0.0.0.0".parse().unwrap());
    SocketAddr::new(host, config_port(cfg))
}
