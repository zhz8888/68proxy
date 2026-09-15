//! 开发调试桥接：让浏览器直接打开 Vite 开发页面时也能调用 Rust 后端。
//!
//! 仅在 debug 构建（`tauri dev` / `cargo run`）编译并启用——release 构建通过
//! `#[cfg(debug_assertions)]` 完全排除本模块，避免把配置、凭据、本地转发 Key
//! 等命令暴露到 HTTP 上。
//!
//! 提供两个端点（默认 127.0.0.1:1431，可用 `CC_DEV_BRIDGE_PORT` 覆写）：
//! - `POST /rpc`：`{"cmd":"...","args":{...}}` → `{"ok":true,"data":...}` 或
//!   `{"ok":false,"error":"..."}`，按名分发到与 Tauri IPC 完全相同的命令函数；
//! - `GET /events`：SSE 事件流，转发 `proxy://log` / `proxy://request` /
//!   `proxy://stats` / `proxy://status` 等后端推送。
//!
//! 前端侧的对应实现在 `src/lib/ipc.ts`：检测到不在 Tauri 运行时即改走此桥接。

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::OnceLock;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

use crate::i18n;

/// 事件广播缓冲：调试时日志可能突发；订阅端落后过多会收到 Lagged 并跳过。
const EVENT_BUFFER: usize = 1024;

/// 桥接默认监听端口。
const DEFAULT_PORT: u16 = 1431;

/// 进程级事件广播发送端（`start` 时初始化）。
static EVENTS: OnceLock<broadcast::Sender<(String, Value)>> = OnceLock::new();

/// 全局 AppHandle，供 HTTP 处理器访问 Tauri 托管状态。
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// 向所有调试客户端广播一条事件（未启动或非调试构建时静默忽略）。
///
/// 由 lib.rs 的事件 sink 调用，使浏览器端能收到与 Tauri 事件同名同结构的推送。
pub fn publish(name: &str, payload: Value) {
    if let Some(tx) = EVENTS.get() {
        let _ = tx.send((name.to_string(), payload));
    }
}

/// 启动桥接服务器（在后台任务中监听；失败仅记日志，不影响主程序）。
pub fn start(app: tauri::AppHandle) {
    let _ = APP.set(app);
    let _ = EVENTS.set(broadcast::channel(EVENT_BUFFER).0);

    let port = std::env::var("CC_DEV_BRIDGE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    tauri::async_runtime::spawn(async move {
        let router = Router::new()
            .route("/rpc", post(rpc))
            .route("/events", get(events))
            .route("/health", get(|| async { "ok" }))
            // 开发用：放行来自任意本地开发源（localhost:1420 等）的跨域请求
            .layer(CorsLayer::permissive());
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                crate::proxy::log::info(&format!(
                    "{}: http://127.0.0.1:{port}/rpc",
                    i18n::pick(
                        "调试桥接已启动，浏览器直连前端页面时将经此调用后端",
                        "Debug bridge started; browser pages call the backend through"
                    )
                ));
                let _ = axum::serve(listener, router).await;
            }
            Err(e) => crate::proxy::log::warn(&format!(
                "{}: {e}",
                i18n::pick(
                    "调试桥接启动失败（端口可能被占用，可用 CC_DEV_BRIDGE_PORT 指定其它端口）",
                    "Failed to start the debug bridge (port may be in use; set CC_DEV_BRIDGE_PORT to use another)"
                )
            )),
        }
    });
}

/// RPC 请求体。
#[derive(Deserialize)]
struct RpcRequest {
    cmd: String,
    #[serde(default)]
    args: Value,
}

/// `POST /rpc`：分发命令并把结果统一包成 `{ok, data|error}`。
async fn rpc(Json(req): Json<RpcRequest>) -> Json<Value> {
    match dispatch(&req.cmd, &req.args).await {
        Ok(data) => Json(json!({ "ok": true, "data": data })),
        Err(e) => Json(json!({ "ok": false, "error": e })),
    }
}

/// `GET /events`：把广播中的事件以 SSE 推给浏览器（事件名为后端事件名）。
async fn events() -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let rx = match EVENTS.get() {
        Some(tx) => tx.subscribe(),
        // 未初始化时建一个空通道，保持返回类型一致
        None => {
            let (tx, rx) = broadcast::channel(EVENT_BUFFER);
            std::mem::forget(tx);
            rx
        }
    };
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok((name, payload)) => {
                    let ev = Event::default().event(name).data(payload.to_string());
                    return Some((Ok::<_, Infallible>(ev), rx));
                }
                // 订阅端落后导致的丢弃：跳过继续等新事件
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// 把可序列化的命令返回值转成 JSON（失败给出中文描述）。
fn to_value<T: serde::Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v)
        .map_err(|e| i18n::err_args("serialize_result_failed", &[&e.to_string()]))
}

/// 按命令名分发到对应的 Tauri 命令函数。
///
/// 参数名与前端传给 Tauri 的一致（camelCase），复用同一批命令实现，
/// 因此桥接与真实 IPC 的行为完全一致。
async fn dispatch(cmd: &str, args: &Value) -> Result<Value, String> {
    // 参数取值助手：缺失时给出默认值，避免因前端少传字段而报错
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let s_or = |k: &str, d: &str| {
        let v = s(k);
        if v.is_empty() { d.to_string() } else { v }
    };
    let b = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let n = |k: &str| args.get(k).and_then(|v| v.as_u64());
    let app = || APP.get().cloned().ok_or_else(|| i18n::err("app_not_ready"));

    match cmd {
        // ── 代理生命周期 ──
        "proxy_start" => Ok(to_value(crate::proxy_start(app()?).await?)?),
        "proxy_stop" => Ok(to_value(crate::proxy_stop(app()?).await?)?),
        "proxy_restart" => Ok(to_value(crate::proxy_restart(app()?).await?)?),
        "proxy_status" => Ok(crate::proxy_status(app()?)),

        // ── 配置 ──
        "config_get" => Ok(to_value(crate::config_get(app()?))?),
        "config_save" => {
            let cfg: crate::proxy::config::Config = serde_json::from_value(
                args.get("config").cloned().unwrap_or(Value::Null),
            )
            .map_err(|e| i18n::err_args("config_parse_failed", &[&e.to_string()]))?;
            Ok(to_value(crate::config_save(app()?, cfg)?)?)
        }

        // ── 本地转发 Key ──
        "local_key_get" => Ok(to_value(crate::local_key_get(app()?)?)?),
        "local_key_set" => {
            crate::local_key_set(app()?, s("key"))?;
            Ok(Value::Null)
        }
        "local_key_generate" => Ok(to_value(crate::local_key_generate(app()?)?)?),
        "local_key_delete" => {
            crate::local_key_delete(app()?)?;
            Ok(Value::Null)
        }

        // ── Command Code 账户 ──
        "account_list" => Ok(to_value(crate::account_list(app()?)?)?),
        "account_add" => {
            let user_name = args.get("userName").and_then(|v| v.as_str()).map(str::to_string);
            crate::account_add(app()?, s("key"), user_name).await?;
            Ok(Value::Null)
        }
        "account_rename" => {
            crate::account_rename(app()?, s("userId"), s("userName"))?;
            Ok(Value::Null)
        }
        "account_remove" => {
            crate::account_remove(app()?, n("index").unwrap_or(0) as usize)?;
            Ok(Value::Null)
        }
        "account_quota" => Ok(to_value(crate::account_quota(app()?, s("userId")).await?)?),
        "accounts_quota" => Ok(to_value(crate::accounts_quota(app()?).await?)?),

        // ── 浏览器授权登录 ──
        "auth_login_start" => Ok(to_value(crate::auth_login_start(app()?).await?)?),
        "auth_login_poll" => Ok(to_value(crate::auth_login_poll(app()?).await?)?),
        "auth_login_cancel" => {
            let _ = crate::auth_login_cancel(app()?);
            Ok(Value::Null)
        }
        "auth_login_open_browser" => Ok(to_value(crate::auth_login_open_browser(
            s("url"),
            b("private"),
        ))?),

        // ── 账户使用规则 ──
        "account_routing_get" => Ok(crate::account_routing_get(app()?)),
        "account_routing_set" => {
            crate::account_routing_set(app()?, s("strategy"), s("preferredAccountId"))?;
            Ok(Value::Null)
        }
        "account_bindings_clear" => Ok(to_value(crate::account_bindings_clear(app()?)?)?),

        // ── 模型 ──
        "models_get" => Ok(to_value(crate::models_get(app()?, b("force")).await?)?),
        "models_catalog" => Ok(crate::models_catalog()),
        "models_catalog_update" => {
            let models: Vec<crate::proxy::pricing::ModelPricing> = serde_json::from_value(
                args.get("models").cloned().unwrap_or_else(|| json!([])),
            )
            .map_err(|e| i18n::err_args("models_parse_failed", &[&e.to_string()]))?;
            let source = args.get("source").and_then(|v| v.as_str()).map(str::to_string);
            Ok(to_value(crate::models_catalog_update(app()?, models, source)?)?)
        }
        "plan_status" => Ok(to_value(crate::plan_status(app()?, b("force")).await?)?),

        // ── 应用信息 ──
        "app_version" => Ok(to_value(crate::app_version(app()?))?),

        // ── 主题 ──
        "theme_get" => Ok(crate::theme_get(app()?)),
        "theme_set" => {
            crate::theme_set(app()?, s("theme"))?;
            Ok(Value::Null)
        }

        // ── 语言 ──
        "language_get" => Ok(crate::language_get(app()?)),
        "language_set" => {
            crate::language_set(app()?, s("language"))?;
            Ok(Value::Null)
        }

        // ── 日志 / 请求记录 ──
        "logs_get" => Ok(crate::logs_get(n("limit").map(|v| v as usize), n("afterSeq"))),
        "logs_clear" => {
            crate::logs_clear();
            Ok(Value::Null)
        }
        "logs_export" => Ok(to_value(crate::logs_export(s("path"))?)?),
        "requests_get" => Ok(crate::requests_get(app()?, n("limit").map(|v| v as usize))),

        // ── 用量统计 ──
        "stats_get" => Ok(to_value(crate::stats_get(app()?, s_or("period", "all"))?)?),
        "stats_chart" => Ok(to_value(crate::stats_chart(app()?, s_or("period", "all"))?)?),
        "stats_recent" => Ok(to_value(
            crate::stats_recent(app()?, n("limit").map(|v| v as usize))?,
        )?),
        "stats_clear_all" => Ok(to_value(crate::stats_clear_all(app()?)?)?),

        // ── 端口 / 开机自启 ──
        "port_check" => Ok(to_value(crate::port_check(n("port").unwrap_or(0) as u16).await?)?),
        "port_free" => Ok(to_value(crate::port_free(n("port").unwrap_or(0) as u16).await?)?),
        "autostart_get" => Ok(json!(crate::autostart_get(app()?))),
        "autostart_set" => {
            crate::autostart_set(app()?, b("enabled"))?;
            Ok(Value::Null)
        }

        other => Err(i18n::err_args("unknown_command", &[other])),
    }
}
