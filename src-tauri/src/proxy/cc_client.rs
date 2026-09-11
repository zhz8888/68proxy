use axum::http::HeaderMap;
use rand::Rng;
use reqwest::Response;
use serde_json::{json, Value};
use std::time::Duration;

use super::fingerprint;
use super::log;
use super::state::{now_millis, AppState, KeyState, ModelInfo, SessionEntry};

/// 硬编码模型回退列表（动态拉取失败时使用）。
pub const HARDCODED_MODELS: &[(&str, &str)] = &[
    ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
    ("claude-opus-4-8", "Claude Opus 4.8"),
    ("claude-opus-4-7", "Claude Opus 4.7"),
    ("claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
    ("gpt-5.5", "GPT-5.5"),
    ("gpt-5.4", "GPT-5.4"),
    ("gpt-5.4-mini", "GPT-5.4 Mini"),
    ("gpt-5.3-codex", "GPT-5.3 Codex"),
    ("deepseek/deepseek-v4-pro", "DeepSeek V4 Pro"),
    ("deepseek/deepseek-v4-flash", "DeepSeek V4 Flash"),
    ("moonshotai/Kimi-K2.6", "Kimi K2.6"),
    ("moonshotai/Kimi-K2.5", "Kimi K2.5"),
    ("zai-org/GLM-5.1", "GLM 5.1"),
    ("zai-org/GLM-5", "GLM 5"),
    ("MiniMaxAI/MiniMax-M3", "MiniMax M3"),
    ("MiniMaxAI/MiniMax-M2.7", "MiniMax M2.7"),
    ("MiniMaxAI/MiniMax-M2.5", "MiniMax M2.5"),
    ("Qwen/Qwen3.6-Max-Preview", "Qwen 3.6 Max Preview"),
    ("Qwen/Qwen3.6-Plus", "Qwen 3.6 Plus"),
    ("Qwen/Qwen3.7-Max", "Qwen 3.7 Max"),
    ("stepfun/Step-3.7-Flash", "Step 3.7 Flash"),
    ("stepfun/Step-3.5-Flash", "Step 3.5 Flash"),
    ("xiaomi/mimo-v2.5-pro", "MiMo V2.5 Pro"),
    ("xiaomi/mimo-v2.5", "MiMo V2.5"),
    ("google/gemini-3.5-flash", "Gemini 3.5 Flash"),
    ("google/gemini-3.1-flash-lite", "Gemini 3.1 Flash Lite"),
    ("tencent/hy3-paid", "Tencent HY3 Paid"),
    ("sakana/fugu-ultra", "Sakana Fugu Ultra"),
    ("thinkingmachines/inkling", "Thinking Machines Inkling"),
    ("poolside/laguna-s-2.1-free", "Poolside Laguna S 2.1 Free"),
];

/// 生成 W3C traceparent 头（`00-{32位trace}-{16位parent}-01`），模拟 CLI 的链路追踪。
pub fn generate_traceparent() -> String {
    let mut rng = rand::thread_rng();
    let trace: String = (0..16).map(|_| format!("{:02x}", rng.gen::<u8>())).collect();
    let parent: String = (0..8).map(|_| format!("{:02x}", rng.gen::<u8>())).collect();
    format!("00-{trace}-{parent}-01")
}

/// 从 sessionId 构造假工作目录 slug，与真实 CLI 规则一致。
///
/// 取 sessionId 前 4 位十六进制数从名称池选词，拼成伪装的 Windows 项目路径
/// `C:\Users\dev\projects\{name}-{hex4}`，再按 CLI 规则去除盘符、
/// 非字母数字转 `-` 并全部小写。
pub fn fake_project_slug(session_id: &str) -> String {
    const NAMES: &[&str] = &[
        "app", "api", "backend", "bot", "cli", "core", "data", "frontend", "lib", "plugin",
        "proxy", "server", "service", "tool", "web", "worker",
    ];
    let hex4 = session_id.get(..4).unwrap_or("0000");
    let idx = u16::from_str_radix(hex4, 16).unwrap_or(0) as usize % NAMES.len();
    let name = NAMES[idx];
    let path = format!(r"C:\Users\dev\projects\{name}-{hex4}").to_lowercase();
    let stripped = path.strip_prefix("c:").unwrap_or(&path);
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in stripped.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "cc-proxy".into()
    } else {
        slug
    }
}

/// 解析本次请求应使用的会话 ID。
///
/// 优先透传下游客户端头 `x-session-id` / `x-claude-code-session-id`（长度 ≥8 才采信），
/// 否则回退到本地为该 Key 维护的会话（见 ensure_session）。
fn get_session_id(state: &AppState, headers: &HeaderMap, api_key: &str) -> String {
    for name in ["x-session-id", "x-claude-code-session-id"] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
            if v.len() >= 8 {
                return v.to_string();
            }
        }
    }
    ensure_session(state, api_key)
}

/// 取该 API Key 当前有效会话；不存在或已过期时生成新 UUID 会话并按 12h+抖动 设置过期。
fn ensure_session(state: &AppState, api_key: &str) -> String {
    let now = now_millis();
    if let Some(entry) = state.sessions.lock().unwrap().get(api_key) {
        if now < entry.expires_at {
            return entry.session_id.clone();
        }
    }
    let mut rng = rand::thread_rng();
    let jitter = rng.gen_range(0..AppState::session_jitter_ms());
    let session_id = uuid::Uuid::new_v4().to_string();
    state.sessions.lock().unwrap().insert(
        api_key.to_string(),
        SessionEntry {
            session_id: session_id.clone(),
            expires_at: now + AppState::session_duration_ms() + jitter,
        },
    );
    log::info(&format!("Session created for key {}", &api_key[..api_key.len().min(8)]));
    session_id
}

/// 取该 API Key 的伪装状态；首次访问时生成随机设备指纹并缓存。
fn get_or_create_key_state(state: &AppState, api_key: &str) -> KeyState {
    let mut states = state.key_states.lock().unwrap();
    if let Some(s) = states.get(api_key) {
        return s.clone();
    }
    let ks = KeyState {
        fingerprint: fingerprint::generate(),
        next_init_at: 0,
    };
    log::info("Fingerprint generated for key");
    states.insert(api_key.to_string(), ks.clone());
    ks
}

/// 读取当前模拟的 command-code CLI 版本号。
pub fn cc_version(state: &AppState) -> String {
    state.cc_version.read().unwrap().clone()
}

/// 初始化预请求（fingerprint/record + lifecycle-events），8h + 2h 抖动刷新。
pub async fn ensure_initialized(state: &AppState, api_key: &str) {
    let now = now_millis();
    let next = get_or_create_key_state(state, api_key).next_init_at;
    if now < next {
        return;
    }

    let headers = base_headers(state, api_key);
    let fingerprint = get_or_create_key_state(state, api_key).fingerprint;
    let cfg = state.config.read().unwrap().clone();
    let client = state.client.clone();

    // 两个初始化预请求的 URL 与请求体：上报指纹 + 上报 CLI 会话存活事件
    let fp_url = format!("{}/alpha/fingerprint/record", cfg.api_base);
    let lc_url = format!("{}/alpha/lifecycle-events", cfg.api_base);
    let fp_body = serde_json::to_value(&fingerprint).unwrap_or_else(|_| json!({}));
    let version = cc_version(state);
    let lc_body = json!({
        "eventType": "cli_session_exists",
        "metadata": {
            "sessionId": format!("sess_{}", uuid::Uuid::new_v4().to_string().replace('-', "")[..16].to_string()),
            "cliVersion": version,
            "mode": "interactive",
            "os": "win32-x64",
        },
    });

    // 单个预请求：15s 超时兜底，失败仅记日志不阻断主流程
    let post = |url: String, body: Value| {
        let client = client.clone();
        let headers = headers.clone();
        async move {
            let res = tokio::time::timeout(Duration::from_secs(15), async {
                client.post(&url).headers(headers).json(&body).send().await
            })
            .await;
            match res {
                Ok(Ok(r)) if r.status().is_success() => log::info("Fingerprint/lifecycle event sent"),
                Ok(Ok(r)) => log::warn(&format!("CC pre-request failed: {}", r.status())),
                Ok(Err(e)) => log::warn(&format!("CC pre-request error: {e}")),
                Err(_) => log::warn("CC pre-request timeout"),
            }
        }
    };

    // 两个预请求并发发送，全部完成后才安排下次刷新时间
    let fp = post(fp_url, fp_body);
    let lc = post(lc_url, lc_body);
    futures_util::join!(fp, lc);

    let mut rng = rand::thread_rng();
    let jitter = rng.gen_range(0..AppState::init_jitter_ms());
    let next_at = now + AppState::init_refresh_ms() + jitter;
    state
        .key_states
        .lock()
        .unwrap()
        .get_mut(api_key)
        .map(|s| s.next_init_at = next_at);
    log::info("Fingerprint/lifecycle next refresh scheduled");
}

/// 构造 CC 上游公共请求头：JSON 内容类型、CLI 环境标识、Bearer 鉴权与 CLI 版本号。
fn base_headers(state: &AppState, api_key: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    headers.insert("x-cli-environment", "production".parse().unwrap());
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {api_key}").parse().unwrap(),
    );
    headers.insert(
        "x-command-code-version",
        cc_version(state).parse().unwrap(),
    );
    headers
}

/// 转发到 CC API /alpha/generate。
///
/// - `body`：已由 convert 模块构造好的 CLI 信封请求体；
/// - `incoming_headers`：下游客户端请求头，用于透传会话 ID；
/// - 返回上游原始 `Response`（调用方负责读取 NDJSON 流与状态码）。
pub async fn forward_to_cc(
    state: &AppState,
    body: &Value,
    api_key: &str,
    incoming_headers: &HeaderMap,
) -> Result<Response, reqwest::Error> {
    let cfg = state.config.read().unwrap().clone();
    let url = format!("{}/alpha/generate", cfg.api_base);
    let session_id = get_session_id(state, incoming_headers, api_key);
    let slug = fake_project_slug(&session_id);

    // 在公共头基础上补齐 CLI 会话/项目/链路追踪等伪装头
    let mut headers = base_headers(state, api_key);
    headers.insert("x-session-id", session_id.parse().unwrap());
    headers.insert("x-co-flag", "false".parse().unwrap());
    headers.insert("x-taste-learning", "false".parse().unwrap());
    headers.insert("x-project-slug", slug.parse().unwrap());
    headers.insert("traceparent", generate_traceparent().parse().unwrap());

    state
        .client
        .post(&url)
        .headers(headers)
        .json(body)
        .send()
        .await
}

/// 从 npm registry 刷新 CC 版本（启动时 + 每 24h）。
pub async fn refresh_cc_version(state: &AppState) {
    let res = tokio::time::timeout(Duration::from_secs(10), async {
        state
            .client
            .get("https://registry.npmjs.org/command-code/latest")
            .send()
            .await
    })
    .await;
    match res {
        Ok(Ok(r)) if r.status().is_success() => {
            if let Ok(pkg) = r.json::<Value>().await {
                if let Some(v) = pkg.get("version").and_then(|v| v.as_str()) {
                    *state.cc_version.write().unwrap() = v.to_string();
                    log::info(&format!("CC version refreshed from npm: {v}"));
                    return;
                }
            }
        }
        Ok(Ok(r)) => log::warn(&format!("CC version fetch failed: {}", r.status())),
        Ok(Err(e)) => log::warn(&format!("CC version fetch error: {e}")),
        Err(_) => log::warn("CC version fetch timeout"),
    }
    log::warn("CC version fetch failed, using current");
}

/// 模型列表：Provider API 动态拉取（按配置间隔缓存），失败回退硬编码列表。
///
/// 返回 `(模型列表, 是否为硬编码回退)`；缓存未过期时直接命中缓存不发请求。
pub async fn fetch_models(state: &AppState, api_key: Option<&str>) -> (Vec<ModelInfo>, bool) {
    let cfg = state.config.read().unwrap().clone();
    let now = now_millis();
    // 缓存非空且未过期时直接命中；用独立块提前释放读锁，避免后续写缓存时死锁
    {
        let cache = state.models.read().unwrap();
        if !cache.models.is_empty() && now - cache.fetched_at < cfg.model_refresh_interval_ms {
            return (cache.models.clone(), false);
        }
    }

    if let Some(key) = api_key {
        if cfg.use_provider_models {
            let url = format!("{}/provider/v1/models", cfg.api_base);
            let headers = base_headers(state, key);
            match tokio::time::timeout(Duration::from_secs(10), async {
                state.client.get(&url).headers(headers).send().await
            })
            .await
            {
                Ok(Ok(r)) if r.status().is_success() => {
                    if let Ok(data) = r.json::<Value>().await {
                        if let Some(arr) = data.get("data").and_then(|d| d.as_array()) {
                            let models: Vec<ModelInfo> = arr
                                .iter()
                                .filter_map(|m| {
                                    let id = m.get("id").and_then(|v| v.as_str())?;
                                    Some(ModelInfo {
                                        id: id.to_string(),
                                        name: id.to_string(),
                                    })
                                })
                                .collect();
                            if !models.is_empty() {
                                *state.models.write().unwrap() = super::state::ModelsCache {
                                    models: models.clone(),
                                    fetched_at: now,
                                };
                                log::info(&format!("Fetched {} models from Provider API", models.len()));
                                return (models, false);
                            }
                        }
                    }
                }
                Ok(Ok(r)) => log::warn(&format!("Provider models fetch failed: {}", r.status())),
                Ok(Err(e)) => log::warn(&format!("Provider models fetch error: {e}")),
                Err(_) => log::warn("Provider models fetch timeout"),
            }
        }
    } else if cfg.use_provider_models {
        log::info("未提供 API Key，使用内置模型列表");
    }

    log::warn("Provider models fetch failed, using hardcoded list");
    (
        HARDCODED_MODELS
            .iter()
            .map(|(id, name)| ModelInfo {
                id: id.to_string(),
                name: name.to_string(),
            })
            .collect(),
        true,
    )
}
