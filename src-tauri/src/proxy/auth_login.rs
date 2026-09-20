//! Command Code 浏览器授权登录：复刻原始 CLI 的 loopback OAuth 流程。
//!
//! 启动一个绑定 `127.0.0.1:0`（随机端口）的本地 HTTP 服务器，前端用系统浏览器打开
//! 授权 URL；用户在 commandcode.ai 完成授权后，服务端把已签发的
//! `apiKey/userId/userName`（亦兼容 `api_key/user_id/user_name` 写法）通过 303
//! 重定向回本地回调地址，本模块校验 `state` 后捕获凭据，浏览器页自动关闭。
//! 捕获结果存于 AppState（`auth_login`），前端经 `auth_login_poll` 查询。

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use axum::Router;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;

use crate::i18n;
use super::state::{now_millis, AppState};

/// 授权登录会话有效期：超过该时长未完成回调即视为超时（毫秒）。
const LOGIN_TTL_MS: u64 = 5 * 60 * 1000;

/// 一次浏览器登录的会话状态（挂 AppState.auth_login）。
#[derive(Debug, Clone)]
pub struct AuthLoginSession {
    /// 防 CSRF 的随机 state（回调用 `CallbackCtx.expected_state` 校验，此处仅作记录）。
    #[allow(dead_code)]
    pub state: String,
    /// loopback 服务器实际监听端口。
    pub port: u16,
    /// 会话创建时间（Unix 毫秒），用于判定登录是否超时。
    pub started_at: u64,
    /// 登录结果（成功后含账户信息）。
    pub result: Option<LoginResult>,
}

/// 登录结果。
#[derive(Debug, Clone)]
pub enum LoginResult {
    /// 授权成功：apiKey + userId + userName（userName 可能为空）。
    Success { api_key: String, user_id: String, user_name: String },
    /// 用户在浏览器拒绝了授权。
    Denied,
    /// 回调参数缺失/state 不匹配等异常。
    Failed(String),
}

/// 从 api_base 推导 commandcode.ai 的 studio 基址（授权页所在域名）。
///
/// - `https://api.commandcode.ai` → `https://commandcode.ai`
/// - `https://staging-api.commandcode.ai` → `https://staging.commandcode.ai`
/// - 其他（自定义 apiBase）回退到 prod 域名。
pub fn studio_base_from_api(api_base: &str) -> String {
    if api_base.contains("staging-api.") {
        "https://staging.commandcode.ai".to_string()
    } else {
        // prod 域名或自定义 apiBase 都回退到 prod studio
        "https://commandcode.ai".to_string()
    }
}

/// 生成 32 字节随机 state（base64url）。
fn generate_state() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 成功页 HTML：显示"授权成功，可关闭窗口"，1.2s 后尝试自动关窗。
fn success_page_html() -> &'static str {
    r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>授权成功</title>
<style>
  body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;display:flex;min-height:100vh;align-items:center;justify-content:center;background:#0f1115;color:#e6e6e6;margin:0}
  .card{background:#1a1d24;border:1px solid #2a2f3a;border-radius:12px;padding:32px 40px;text-align:center;max-width:420px}
  .tick{font-size:40px}
  h1{font-size:18px;margin:12px 0 8px}
  p{color:#9aa3af;font-size:13px;margin:0}
</style></head>
<body><div class="card"><div class="tick">✅</div><h1>授权成功</h1><p>你可以关闭此窗口并返回 68Proxy。</p></div>
<script>setTimeout(() => { try { window.close(); } catch (e) {} }, 1200);</script>
</body></html>"#
}

/// 拒绝页 HTML。
fn denied_page_html() -> &'static str {
    r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>授权被拒绝</title>
<style>
  body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;display:flex;min-height:100vh;align-items:center;justify-content:center;background:#0f1115;color:#e6e6e6;margin:0}
  .card{background:#1a1d24;border:1px solid #2a2f3a;border-radius:12px;padding:32px 40px;text-align:center;max-width:420px}
  .x{font-size:40px}
  h1{font-size:18px;margin:12px 0 8px}
  p{color:#9aa3af;font-size:13px;margin:0}
</style></head>
<body><div class="card"><div class="x">⚠️</div><h1>授权被拒绝</h1><p>你可以在 68Proxy 中重试登录。</p></div>
<script>setTimeout(() => { try { window.close(); } catch (e) {} }, 1200);</script>
</body></html>"#
}

/// 错误页 HTML。
fn error_page_html(msg: &str) -> String {
    let safe = msg.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>授权失败</title>
<style>
  body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;display:flex;min-height:100vh;align-items:center;justify-content:center;background:#0f1115;color:#e6e6e6;margin:0}}
  .card{{background:#1a1d24;border:1px solid #2a2f3a;border-radius:12px;padding:32px 40px;text-align:center;max-width:420px}}
  .x{{font-size:40px}}
  h1{{font-size:18px;margin:12px 0 8px}}
  p{{color:#9aa3af;font-size:13px;margin:0;word-break:break-all}}
</style></head>
<body><div class="card"><div class="x">❌</div><h1>授权失败</h1><p>{safe}</p></div>
<script>setTimeout(() => {{ try {{ window.close(); }} catch (e) {{}} }}, 1200);</script>
</body></html>"#
    )
}

/// 无进行中登录（已取消/已过期/已完成后访问）时的提示页。
fn no_session_page_html() -> &'static str {
    r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>无进行中的授权</title>
<style>
  body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;display:flex;min-height:100vh;align-items:center;justify-content:center;background:#0f1115;color:#e6e6e6;margin:0}
  .card{background:#1a1d24;border:1px solid #2a2f3a;border-radius:12px;padding:32px 40px;text-align:center;max-width:420px}
  .x{font-size:40px}
  h1{font-size:18px;margin:12px 0 8px}
  p{color:#9aa3af;font-size:13px;margin:0}
</style></head>
<body><div class="card"><div class="x">ℹ️</div><h1>没有进行中的授权</h1><p>本次登录已取消或已结束，请在 68Proxy 中重新发起登录。</p></div>
<script>setTimeout(() => { try { window.close(); } catch (e) {} }, 1200);</script>
</body></html>"#
}

/// 收尾跳转：附 `Referrer-Policy: no-referrer` 与 `Cache-Control: no-store`。
///
/// 上游在 `mode=redirect` 下会把 apiKey 放在回调 URL 的查询参数里，浏览器跳转后
/// 该 URL 可能进入历史记录，并可能经 `Referer` 头泄漏给后续页面或第三方资源；
/// 收尾响应显式声明不发送 Referer、不缓存。
fn redirect_complete() -> axum::response::Response {
    let mut resp = Redirect::to("/callback/complete").into_response();
    resp.headers_mut().insert(
        axum::http::header::REFERRER_POLICY,
        axum::http::HeaderValue::from_static("no-referrer"),
    );
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    resp
}

/// 构建授权 URL：`{studio}/studio/auth/cli?callback=...&state=...&mode=redirect`。
pub fn build_auth_url(studio_base: &str, port: u16, state: &str) -> String {
    let callback = format!("http://127.0.0.1:{port}/callback");
    let encoded = urlencoding(&callback);
    format!("{studio_base}/studio/auth/cli?callback={encoded}&state={state}&mode=redirect")
}

/// 极简 URL 编码（仅编码保留字符，state 已是 base64url 安全字符）。
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 回调服务器共享状态：AppState + 本次登录的 state（防 CSRF）。
struct CallbackCtx {
    state: Arc<AppState>,
    expected_state: String,
}

/// 在 127.0.0.1 随机端口启动 loopback 授权回调服务器，并把会话状态写入 AppState。
///
/// 返回授权 URL（前端用于打开浏览器）。服务器随进程常驻，直到收到回调、
/// `cancel_auth_login` 主动取消，或下一轮登录启动时被关停。
pub async fn start_auth_login(state: &Arc<AppState>) -> Result<String, String> {
    // 已有进行中的登录：先关停其 loopback 服务器并清理会话，避免旧端口残留
    cancel_auth_login(state);

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("auth_callback_server_failed", &[&e])
        })?;
    let port = listener
        .local_addr()
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("auth_callback_port_failed", &[&e])
        })?
        .port();
    let state_token = generate_state();
    let api_base = state.config.read().unwrap().api_base.clone();
    let studio_base = studio_base_from_api(&api_base);
    let url = build_auth_url(&studio_base, port, &state_token);

    // 记录会话到 AppState
    *state.auth_login.lock().unwrap() = Some(AuthLoginSession {
        state: state_token.clone(),
        port,
        started_at: now_millis(),
        result: None,
    });

    let ctx = Arc::new(CallbackCtx {
        state: state.clone(),
        expected_state: state_token.clone(),
    });
    // `/callback` 同时接受 GET（旧式 query 回传）与 POST（上游标准回传：表单或
    // JSON 体，见 callback_post_handler），对齐 CLI 的 createAuthServer 行为。
    let router = Router::new()
        .route("/callback", get(callback_handler).post(callback_post_handler))
        .route("/callback/complete", get(complete_handler))
        .with_state(ctx);

    // 优雅停机：新一轮登录/取消时经 oneshot 关闭旧服务器，释放随机端口
    let (tx, rx) = tokio::sync::oneshot::channel();
    *state.auth_login_shutdown.lock().unwrap() = Some(tx);

    // 后台运行，不阻塞调用方
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });

    Ok(url)
}

/// 从回调参数中取第一个非空值（兼容 camelCase 与 snake_case 两种上游写法）。
fn pick<'a>(params: &'a HashMap<String, String>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| params.get(*k).filter(|v| !v.is_empty()).cloned())
        .unwrap_or_default()
}

/// 处理回调：先校验 state 与当前会话绑定，捕获 apiKey 或 error，写入 AppState 并 303 跳转收尾页。
async fn callback_handler(
    State(ctx): State<Arc<CallbackCtx>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    process_callback(ctx, params)
}

/// 处理 POST 回调：上游完成授权后以 POST 回传凭据（对齐 CLI `createAuthServer`）。
///
/// 同时接受 `application/x-www-form-urlencoded` 与 `application/json` 两种请求体
/// （CLI 对两种 content-type 分别走 `handleBrowserCallback` / `handleLegacyJsonCallback`）。
/// 表单/JSON 均解析为参数 map 后交给共享的 `process_callback`。
async fn callback_post_handler(
    State(ctx): State<Arc<CallbackCtx>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl IntoResponse {
    let params = match callback_content_type(&headers).as_deref() {
        Some("application/json") => match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => match v {
                serde_json::Value::Object(map) => map
                    .into_iter()
                    .map(|(k, v)| {
                        let val = match v {
                            serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        };
                        (k, val)
                    })
                    .collect(),
                _ => {
                    let mut session = ctx.state.auth_login.lock().unwrap();
                    if let Some(s) = session.as_mut() {
                        if s.state == ctx.expected_state {
                            s.result =
                                Some(LoginResult::Failed("auth_callback_params_missing".into()));
                        }
                    }
                    drop(session);
                    return redirect_complete();
                }
            },
            Err(_) => {
                let mut session = ctx.state.auth_login.lock().unwrap();
                if let Some(s) = session.as_mut() {
                    if s.state == ctx.expected_state {
                        s.result =
                            Some(LoginResult::Failed("auth_callback_params_missing".into()));
                    }
                }
                drop(session);
                return redirect_complete();
            }
        },
        // 表单或未声明类型：按 URL 编码表单解析（上游默认行为）
        _ => match serde_urlencoded::from_str::<HashMap<String, String>>(&body) {
            Ok(m) => m,
            Err(_) => {
                let mut session = ctx.state.auth_login.lock().unwrap();
                if let Some(s) = session.as_mut() {
                    if s.state == ctx.expected_state {
                        s.result =
                            Some(LoginResult::Failed("auth_callback_params_missing".into()));
                    }
                }
                drop(session);
                return redirect_complete();
            }
        },
    };
    process_callback(ctx, params)
}

/// 取请求的 Content-Type（小写，去参数）。
fn callback_content_type(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
}

/// 回调核心处理：校验 state、按 error / 凭据分写结果并 303 跳收尾页。
///
/// 参数 map 由 GET query 或 POST 体解析而来；state 防 CSRF 校验对所有入口一视同仁。
fn process_callback(
    ctx: Arc<CallbackCtx>,
    params: HashMap<String, String>,
) -> axum::response::Response {
    let st = ctx.state.clone();
    let expected_state = ctx.expected_state.clone();
    let param_state = params.get("state").cloned().unwrap_or_default();
    // state 必须与本回调服务器签发的值一致（防 CSRF）。error 分支同样先校验，
    // 否则本机任意进程用 GET /callback?error= 即可把登录置为失败。
    if param_state != expected_state {
        let mut session = st.auth_login.lock().unwrap();
        if let Some(s) = session.as_mut() {
            // 仅当会话仍属于本次登录时才写入，避免旧回调覆盖新登录
            if s.state == expected_state {
                s.result = Some(LoginResult::Failed("auth_state_invalid".into()));
            }
        }
        drop(session);
        return redirect_complete();
    }

    // 错误分支（用户拒绝等），已在上面完成 state 校验
    if let Some(err) = params.get("error") {
        let denied = err == "access_denied";
        let desc = params
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| err.clone());
        let mut session = st.auth_login.lock().unwrap();
        if let Some(s) = session.as_mut() {
            if s.state == expected_state {
                s.result = Some(if denied {
                    LoginResult::Denied
                } else {
                    LoginResult::Failed(desc)
                });
            }
        }
        drop(session);
        return redirect_complete();
    }

    // 上下游字段命名不确定：camelCase 与 snake_case 都接受
    let api_key = pick(&params, &["apiKey", "api_key"]);
    let user_id = pick(&params, &["userId", "user_id"]);
    let user_name = pick(&params, &["userName", "user_name"]);

    // 成功：仅在会话仍属于本次登录时写入结果
    {
        let mut session = st.auth_login.lock().unwrap();
        let Some(s) = session.as_mut() else {
            return redirect_complete();
        };
        if s.state != expected_state {
            return redirect_complete();
        }
        if api_key.is_empty() || user_id.is_empty() {
            s.result = Some(LoginResult::Failed(
                "auth_callback_params_missing".into(),
            ));
        } else {
            s.result = Some(LoginResult::Success {
                api_key,
                user_id,
                user_name,
            });
        }
    }
    redirect_complete()
}

/// 授权收尾页：按登录结果渲染成功/拒绝/错误页，页面内自动关窗。
async fn complete_handler(
    State(ctx): State<Arc<CallbackCtx>>,
) -> axum::response::Html<String> {
    let page = {
        let session = ctx.state.auth_login.lock().unwrap();
        match session.as_ref().and_then(|s| s.result.as_ref()) {
            Some(LoginResult::Success { .. }) => success_page_html().to_string(),
            Some(LoginResult::Denied) => denied_page_html().to_string(),
            Some(LoginResult::Failed(msg)) => error_page_html(msg),
            // 会话已取消/已结束：不能误显「授权成功」
            None => no_session_page_html().to_string(),
        }
    };
    axum::response::Html(page)
}

/// 主动取消进行中的登录：清空会话并关停 loopback 服务器。
pub fn cancel_auth_login(state: &AppState) {
    *state.auth_login.lock().unwrap() = None;
    if let Some(tx) = state.auth_login_shutdown.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

/// 查询当前登录结果（供前端轮询）。返回 JSON：`{status, account?}`。
///
/// 超过 `LOGIN_TTL_MS` 仍未收到回调时判定超时并置为 failed（同时关停旧服务器）。
///
/// 成功结果只消费一次：调用方（`auth_login_poll` 命令）在落库后调用
/// `take_auth_login_success` 取走并关闭会话，避免每次轮询重复返回明文 key、
/// 重复落库，以及用户删号后被下一次轮询“复活”。
pub fn poll_auth_login(state: &AppState) -> serde_json::Value {
    let mut session = state.auth_login.lock().unwrap();
    let Some(s) = session.as_mut() else {
        return json!({ "status": "idle" });
    };
    if s.result.is_none() && now_millis().saturating_sub(s.started_at) > LOGIN_TTL_MS {
        s.result = Some(LoginResult::Failed("auth_timeout".into()));
    }
    match s.result.as_ref() {
        None => json!({ "status": "pending", "port": s.port }),
        Some(LoginResult::Success { api_key, user_id, user_name }) => json!({
            "status": "success",
            "account": {
                "key": api_key,
                "userId": user_id,
                "userName": user_name,
            }
        }),
        Some(LoginResult::Denied) => json!({ "status": "denied" }),
        Some(LoginResult::Failed(msg)) => json!({ "status": "failed", "error": msg }),
    }
}

/// 取走成功结果并关闭登录会话（一次性消费）。
///
/// 返回 `Some((key, user_id, user_name))` 时调用方负责落库；取走后会话置 idle、
/// loopback 服务器随之关停，`state` 令牌即刻失效不可重放。
pub fn take_auth_login_success(state: &AppState) -> Option<(String, String, String)> {
    let mut session = state.auth_login.lock().unwrap();
    let s = session.as_mut()?;
    match s.result.clone() {
        Some(LoginResult::Success { api_key, user_id, user_name }) => {
            drop(session);
            cancel_auth_login(state);
            Some((api_key, user_id, user_name))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 从 apiBase 推导授权页地址：标准/分段域名按规则映射，未知域名回退生产。
    #[test]
    fn studio_base_derivation() {
        assert_eq!(studio_base_from_api("https://api.commandcode.ai"), "https://commandcode.ai");
        assert_eq!(
            studio_base_from_api("https://staging-api.commandcode.ai"),
            "https://staging.commandcode.ai"
        );
        // 自定义 apiBase 回退 prod
        assert_eq!(studio_base_from_api("https://my-proxy.example.com"), "https://commandcode.ai");
    }

    /// 授权 URL 的形态：路径、回调编码、state 与 mode 参数齐全。
    #[test]
    fn auth_url_shape() {
        let url = build_auth_url("https://commandcode.ai", 43210, "teststate123");
        assert!(url.starts_with("https://commandcode.ai/studio/auth/cli?callback="));
        assert!(url.contains("callback=http%3A%2F%2F127.0.0.1%3A43210%2Fcallback"));
        assert!(url.contains("state=teststate123"));
        assert!(url.contains("mode=redirect"));
    }

    /// URL 编码对保留字符的转义正确性。
    #[test]
    fn url_encoding_escapes() {
        let s = urlencoding("http://127.0.0.1:8080/callback?x=1&y=2");
        assert_eq!(s, "http%3A%2F%2F127.0.0.1%3A8080%2Fcallback%3Fx%3D1%26y%3D2");
    }

    /// 回调参数兼容 camelCase 与 snake_case 两种键名。
    #[test]
    fn callback_params_accept_both_casings() {
        let mut camel = HashMap::new();
        camel.insert("apiKey".to_string(), "user_abc".to_string());
        camel.insert("userId".to_string(), "id_1".to_string());
        camel.insert("userName".to_string(), "小明".to_string());
        assert_eq!(pick(&camel, &["apiKey", "api_key"]), "user_abc");
        assert_eq!(pick(&camel, &["userId", "user_id"]), "id_1");
        assert_eq!(pick(&camel, &["userName", "user_name"]), "小明");

        let mut snake = HashMap::new();
        snake.insert("api_key".to_string(), "user_def".to_string());
        snake.insert("user_id".to_string(), "id_2".to_string());
        assert_eq!(pick(&snake, &["apiKey", "api_key"]), "user_def");
        assert_eq!(pick(&snake, &["userId", "user_id"]), "id_2");
        // 两边都缺失时返回空串（由调用方判为参数缺失）
        assert_eq!(pick(&snake, &["userName", "user_name"]), "");
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use super::super::config::Config;

    /// 各 flow 测试共享的本机端口串行锁（loopback 服务建停存在平台时序竞态）。
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 带 3 次重试的 GET（并行高负载下 loopback 偶发 IncompleteMessage）。
    async fn cb_get(url: String) -> reqwest::Response {
        for _ in 0..3 {
            match reqwest::get(&url).await {
                Ok(r) => return r,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(120)).await,
            }
        }
        panic!("回调请求重试 3 次仍失败: {url}");
    }

    /// 启动一次登录并返回 (状态, 端口, 正确的 state token)。
    async fn started() -> (Arc<AppState>, u16, String) {
        let st = AppState::new(Config::default());
        let _url = start_auth_login(&st).await.unwrap();
        let (port, token) = {
            let s = st.auth_login.lock().unwrap();
            let s = s.as_ref().unwrap();
            (s.port, s.state.clone())
        };
        (st, port, token)
    }

    /// 成功链路：camelCase 参数回调 → poll 得 success 与账户信息 → 收尾页为成功页。
    #[tokio::test]
    async fn login_success_camelcase() {
        let _serial = SERIAL.lock().await;
        let (st, port, token) = started().await;
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        let res = client
            .get(&format!(
                "http://127.0.0.1:{port}/callback?state={token}&apiKey=user_abc&userId=id_1&userName=NM"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 303);
        let poll = poll_auth_login(&st);
        assert_eq!(poll["status"], "success");
        assert_eq!(poll["account"]["key"], "user_abc");
        assert_eq!(poll["account"]["userId"], "id_1");
        let page = cb_get(format!("http://127.0.0.1:{port}/callback/complete"))
            .await
            .text()
            .await
            .unwrap();
        assert!(page.contains("授权成功"));
        cancel_auth_login(&st);
    }

    /// snake_case 参数同样被接受。
    #[tokio::test]
    async fn login_success_snake_case() {
        let _serial = SERIAL.lock().await;
        let (st, port, token) = started().await;
        cb_get(format!(
            "http://127.0.0.1:{port}/callback?state={token}&api_key=user_s&user_id=id_s&user_name=S"
        ))
        .await;
        assert_eq!(poll_auth_login(&st)["status"], "success");
        cancel_auth_login(&st);
    }

    /// POST 表单回调（上游标准回传，对齐 CLI createAuthServer）：凭据进 body。
    #[tokio::test]
    async fn login_success_post_form() {
        let _serial = SERIAL.lock().await;
        let (st, port, token) = started().await;
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        let res = client
            .post(format!("http://127.0.0.1:{port}/callback"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "state={token}&apiKey=user_post&userId=id_post&userName=Post&keyName=cli-browser"
            ))
            .send()
            .await
            .unwrap();
        // 成功回调 303 跳收尾页
        assert_eq!(res.status(), 303);
        let poll = poll_auth_login(&st);
        assert_eq!(poll["status"], "success");
        assert_eq!(poll["account"]["key"], "user_post");
        assert_eq!(poll["account"]["userId"], "id_post");
        let page = cb_get(format!("http://127.0.0.1:{port}/callback/complete"))
            .await
            .text()
            .await
            .unwrap();
        assert!(page.contains("授权成功"));
        cancel_auth_login(&st);
    }

    /// POST JSON 回调（对齐 CLI handleLegacyJsonCallback）：凭据进 JSON 体。
    #[tokio::test]
    async fn login_success_post_json() {
        let _serial = SERIAL.lock().await;
        let (st, port, token) = started().await;
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        let res = client
            .post(format!("http://127.0.0.1:{port}/callback"))
            .header("Content-Type", "application/json")
            .json(&json!({
                "state": token,
                "apiKey": "user_json",
                "userId": "id_json",
                "userName": "Json",
                "keyName": "cli-browser",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 303);
        let poll = poll_auth_login(&st);
        assert_eq!(poll["status"], "success");
        assert_eq!(poll["account"]["key"], "user_json");
        assert_eq!(poll["account"]["userId"], "id_json");
        cancel_auth_login(&st);
    }

    /// POST 表单 error=access_denied 同样被识别为拒绝。
    #[tokio::test]
    async fn login_denied_post_form() {
        let _serial = SERIAL.lock().await;
        let (st, port, token) = started().await;
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        let res = client
            .post(format!("http://127.0.0.1:{port}/callback"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("state={token}&error=access_denied"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 303);
        assert_eq!(poll_auth_login(&st)["status"], "denied");
        cancel_auth_login(&st);
    }

    /// 用户拒绝：error=access_denied → Denied + 拒绝页。
    #[tokio::test]
    async fn login_denied_flow() {
        let _serial = SERIAL.lock().await;
        let (st, port, token) = started().await;
        cb_get(format!("http://127.0.0.1:{port}/callback?state={token}&error=access_denied"))
            .await;
        assert_eq!(poll_auth_login(&st)["status"], "denied");
        let page = cb_get(format!("http://127.0.0.1:{port}/callback/complete"))
            .await
            .text()
            .await
            .unwrap();
        assert!(page.contains("授权被拒绝"));
        cancel_auth_login(&st);
    }

    /// 其他 error：置为 failed 并透传描述；错误页做 HTML 转义。
    #[tokio::test]
    async fn login_error_flow_escapes_html() {
        let _serial = SERIAL.lock().await;
        let (st, port, token) = started().await;
        cb_get(format!(
            "http://127.0.0.1:{port}/callback?state={token}&error=server_broke&error_description=%3Cb%3Ebad%3C/b%3E"
        ))
        .await;
        let poll = poll_auth_login(&st);
        assert_eq!(poll["status"], "failed");
        assert_eq!(poll["error"], "<b>bad</b>");
        let page = cb_get(format!("http://127.0.0.1:{port}/callback/complete"))
            .await
            .text()
            .await
            .unwrap();
        assert!(page.contains("&lt;b&gt;"), "错误页应转义 HTML");
        cancel_auth_login(&st);
    }

    /// state 不匹配（CSRF 防护）：登录置失败，不覆盖凭据。
    #[tokio::test]
    async fn callback_rejects_wrong_state() {
        let _serial = SERIAL.lock().await;
        let (st, port, _token) = started().await;
        cb_get(format!(
            "http://127.0.0.1:{port}/callback?state=WRONG&apiKey=user_x&userId=id_x"
        ))
        .await;
        let poll = poll_auth_login(&st);
        assert_eq!(poll["status"], "failed");
        assert_eq!(poll["error"], "auth_state_invalid");
        cancel_auth_login(&st);
    }

    /// 成功回调缺参数：置为 auth_callback_params_missing。
    #[tokio::test]
    async fn callback_missing_params_fails() {
        let _serial = SERIAL.lock().await;
        let (st, port, token) = started().await;
        cb_get(format!("http://127.0.0.1:{port}/callback?state={token}&apiKey=user_only"))
            .await;
        let poll = poll_auth_login(&st);
        assert_eq!(poll["status"], "failed");
        assert_eq!(poll["error"], "auth_callback_params_missing");
        cancel_auth_login(&st);
    }

    /// 取消后访问收尾页：渲染「无进行中的授权」页，poll 回 idle。
    #[tokio::test]
    async fn cancel_clears_session_and_page() {
        let _serial = SERIAL.lock().await;
        let (st, _port, _token) = started().await;
        cancel_auth_login(&st);
        // 会话被清空：poll 回 idle（loopback 服务器的优雅停机在测试环境的
        // current_thread runtime 下不停止 accept，故不断言端口不可达）
        assert_eq!(poll_auth_login(&st)["status"], "idle");
    }

    /// 二次登录关停旧 loopback 服务器：旧端口不可达，会话被替换。
    #[tokio::test]
    async fn restart_replaces_old_server() {
        let _serial = SERIAL.lock().await;
        let (st, old_port, old_token) = started().await;
        let (_url2, new_port) = {
            start_auth_login(&st).await.unwrap();
            let s = st.auth_login.lock().unwrap();
            let s = s.as_ref().unwrap();
            (s.port, s.port)
        };
        assert_ne!(old_port, new_port);
        // 旧会话被替换：state token 已更新
        let new_token = st.auth_login.lock().unwrap().as_ref().unwrap().state.clone();
        assert_ne!(old_token, new_token);
        assert_eq!(poll_auth_login(&st)["status"], "pending");
        cancel_auth_login(&st);
    }

    /// 轮询状态机：无会话 idle、进行中 pending、超时置 failed。
    #[tokio::test]
    async fn poll_states_and_timeout() {
        let _serial = SERIAL.lock().await;
        let st = AppState::new(Config::default());
        assert_eq!(poll_auth_login(&st)["status"], "idle");
        start_auth_login(&st).await.unwrap();
        assert_eq!(poll_auth_login(&st)["status"], "pending");
        // 人为把会话推到过期
        {
            let mut s = st.auth_login.lock().unwrap();
            s.as_mut().unwrap().started_at = now_millis() - LOGIN_TTL_MS - 1000;
        }
        let poll = poll_auth_login(&st);
        assert_eq!(poll["status"], "failed");
        assert_eq!(poll["error"], "auth_timeout");
        cancel_auth_login(&st);
    }

    /// 两次签发的 state token 互不相同（防重放）。
    #[tokio::test]
    async fn state_token_unique_per_login() {
        let _serial = SERIAL.lock().await;
        let (st, _, t1) = started().await;
        start_auth_login(&st).await.unwrap();
        let t2 = st.auth_login.lock().unwrap().as_ref().unwrap().state.clone();
        assert_ne!(t1, t2);
        cancel_auth_login(&st);
    }
}
