//! Command Code 浏览器授权登录：复刻原始 CLI 的 loopback OAuth 流程。
//!
//! 启动一个绑定 `127.0.0.1:0`（随机端口）的本地 HTTP 服务器，前端用系统浏览器打开
//! 授权 URL；用户在 commandcode.ai 完成授权后，服务端把已签发的 `apiKey/userId/userName`
//! 通过 303 重定向回本地回调地址，本模块校验 `state` 后捕获凭据，浏览器页自动关闭。
//! 捕获结果存于 AppState（`auth_login`），前端经 `auth_login_poll` 查询。

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use axum::Router;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;

use super::state::AppState;

/// 一次浏览器登录的会话状态（挂 AppState.auth_login）。
#[derive(Debug, Clone)]
pub struct AuthLoginSession {
    /// 防 CSRF 的随机 state（回调用 `CallbackCtx.expected_state` 校验，此处仅作记录）。
    #[allow(dead_code)]
    pub state: String,
    /// loopback 服务器实际监听端口。
    pub port: u16,
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
<body><div class="card"><div class="tick">✅</div><h1>授权成功</h1><p>你可以关闭此窗口并返回 68proxy。</p></div>
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
<body><div class="card"><div class="x">⚠️</div><h1>授权被拒绝</h1><p>你可以在 68proxy 中重试登录。</p></div>
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
/// 返回授权 URL（前端用于打开浏览器）。服务器随进程常驻，直到收到回调或
/// `cancel_auth_login` 主动取消。
pub async fn start_auth_login(state: &Arc<AppState>) -> Result<String, String> {
    // 已有进行中的登录则先清理
    *state.auth_login.lock().unwrap() = None;

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("启动授权回调服务器失败: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("读取授权回调端口失败: {e}"))?
        .port();
    let state_token = generate_state();
    let api_base = state.config.read().unwrap().api_base.clone();
    let studio_base = studio_base_from_api(&api_base);
    let url = build_auth_url(&studio_base, port, &state_token);

    // 记录会话到 AppState
    *state.auth_login.lock().unwrap() = Some(AuthLoginSession {
        state: state_token.clone(),
        port,
        result: None,
    });

    let ctx = Arc::new(CallbackCtx {
        state: state.clone(),
        expected_state: state_token.clone(),
    });
    let router = Router::new()
        .route("/callback", get(callback_handler))
        .route("/callback/complete", get(complete_handler))
        .with_state(ctx);
    let server = axum::serve(listener, router);

    // 后台运行，不阻塞调用方
    tokio::spawn(async move {
        let _ = server.await;
    });

    Ok(url)
}

/// 处理回调：校验 state，捕获 apiKey 或 error，写入 AppState 并 303 跳转收尾页。
async fn callback_handler(
    State(ctx): State<Arc<CallbackCtx>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let st = ctx.state.clone();
    let expected_state = ctx.expected_state.clone();
    // 错误分支（用户拒绝等）
    if let Some(err) = params.get("error") {
        let denied = err == "access_denied";
        let desc = params
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| err.clone());
        let mut session = st.auth_login.lock().unwrap();
        if let Some(s) = session.as_mut() {
            s.result = Some(if denied {
                LoginResult::Denied
            } else {
                LoginResult::Failed(desc)
            });
        }
        drop(session);
        return Redirect::to("/callback/complete").into_response();
    }

    // state 校验（防 CSRF）
    let state_ok = params
        .get("state")
        .map(|s| s == &expected_state)
        .unwrap_or(false);
    let api_key = params.get("api_key").cloned().unwrap_or_default();
    let user_id = params.get("user_id").cloned().unwrap_or_default();
    let user_name = params.get("user_name").cloned().unwrap_or_default();

    if !state_ok {
        let mut session = st.auth_login.lock().unwrap();
        if let Some(s) = session.as_mut() {
            s.result = Some(LoginResult::Failed("state 校验失败，请重试登录".into()));
        }
        drop(session);
        return Redirect::to("/callback/complete").into_response();
    }
    if api_key.is_empty() || user_id.is_empty() {
        let mut session = st.auth_login.lock().unwrap();
        if let Some(s) = session.as_mut() {
            s.result = Some(LoginResult::Failed(
                "回调缺少必要参数（apiKey/userId），请重试登录".into(),
            ));
        }
        drop(session);
        return Redirect::to("/callback/complete").into_response();
    }

    // 成功：写入结果
    {
        let mut session = st.auth_login.lock().unwrap();
        if let Some(s) = session.as_mut() {
            s.result = Some(LoginResult::Success {
                api_key,
                user_id,
                user_name,
            });
        }
    }
    Redirect::to("/callback/complete").into_response()
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
            None => success_page_html().to_string(),
        }
    };
    axum::response::Html(page)
}

/// 主动取消进行中的登录（关闭服务器由进程生命周期管理）。
pub fn cancel_auth_login(state: &AppState) {
    *state.auth_login.lock().unwrap() = None;
}

/// 查询当前登录结果（供前端轮询）。返回 JSON：`{status, account?}`。
pub fn poll_auth_login(state: &AppState) -> serde_json::Value {
    let session = state.auth_login.lock().unwrap();
    let Some(s) = session.as_ref() else {
        return json!({ "status": "idle" });
    };
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn auth_url_shape() {
        let url = build_auth_url("https://commandcode.ai", 43210, "teststate123");
        assert!(url.starts_with("https://commandcode.ai/studio/auth/cli?callback="));
        assert!(url.contains("callback=http%3A%2F%2F127.0.0.1%3A43210%2Fcallback"));
        assert!(url.contains("state=teststate123"));
        assert!(url.contains("mode=redirect"));
    }

    #[test]
    fn url_encoding_escapes() {
        let s = urlencoding("http://127.0.0.1:8080/callback?x=1&y=2");
        assert_eq!(s, "http%3A%2F%2F127.0.0.1%3A8080%2Fcallback%3Fx%3D1%26y%3D2");
    }
}
