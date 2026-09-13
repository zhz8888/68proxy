use axum::http::HeaderMap;
use rand::Rng;
use reqwest::Response;
use serde_json::{json, Value};
use std::time::Duration;

use super::fingerprint;
use super::log;
use super::state::{now_millis, AppState, KeyState, ModelInfo, SessionEntry};

/// 兜底模型回退列表（动态拉取失败时使用），按当前生效的模型表推导，
/// 保证与模型清单一致（数据来自 SQLite，未落库时回退内置表）。
pub fn hardcoded_models() -> Vec<ModelInfo> {
    super::pricing::all_models()
        .iter()
        .map(|m| ModelInfo {
            id: m.id.clone(),
            name: if m.name.is_empty() { m.id.clone() } else { m.name.clone() },
        })
        .collect()
}

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

/// 判断字符串能否安全用作 HTTP 头值：仅可见 ASCII（可含空格），排除控制字符与多字节字符。
///
/// 会话 ID 最终会写入 `x-session-id` 头，而候选来源（请求体 prompt_cache_key）由下游
/// 任意填写，故必须过滤，否则 HeaderValue 解析失败。
fn is_header_safe(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
}

/// 解析本次请求应使用的会话 ID。
///
/// 优先透传下游客户端头 `x-session-id` / `x-claude-code-session-id` / `session_id`
/// （长度 ≥8 才采信），其次是请求体的 prompt_cache_key，否则回退到本地为该账户
/// （按 userId）维护的会话（见 ensure_session）。
fn get_session_id(
    state: &AppState,
    headers: &HeaderMap,
    user_id: &str,
    prompt_cache_key: Option<&str>,
) -> String {
    for name in ["x-session-id", "x-claude-code-session-id", "session_id"] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
            if v.len() >= 8 {
                return v.to_string();
            }
        }
    }
    if let Some(k) = prompt_cache_key {
        // 请求体字段不可信：非可见 ASCII（如含中文）会让后续写入请求头时解析失败
        if k.len() >= 8 && is_header_safe(k) {
            return k.to_string();
        }
    }
    ensure_session(state, user_id)
}

/// 取该账户（按 userId 标识）当前有效会话；不存在或已过期时生成新 UUID 会话并
/// 按 12h+抖动 设置过期。以 userId 为键，同一账户重新登录换 key 时会话延续。
fn ensure_session(state: &AppState, user_id: &str) -> String {
    let now = now_millis();
    if let Some(entry) = state.sessions.lock().unwrap().get(user_id) {
        if now < entry.expires_at {
            return entry.session_id.clone();
        }
    }
    let mut rng = rand::thread_rng();
    let jitter = rng.gen_range(0..AppState::session_jitter_ms());
    let session_id = uuid::Uuid::new_v4().to_string();
    state.sessions.lock().unwrap().insert(
        user_id.to_string(),
        SessionEntry {
            session_id: session_id.clone(),
            expires_at: now + AppState::session_duration_ms() + jitter,
        },
    );
    // user_id 来自上游 whoami，可能是任意字符串：按字符截断避免字节切片落在字符中间 panic
    let short_id: String = user_id.chars().take(8).collect();
    log::info(&format!(
            "{} {short_id}",
            crate::i18n::pick("会话已创建，用户：", "Session created for user")
        ));
    session_id
}

/// 取该账户（按 userId 标识）的伪装状态；首次访问时优先从磁盘恢复指纹
/// （同一 userId 跨重启复用同一设备身份，换 key 也延续），无记录或读取失败时
/// 新生成并写盘。未注入路径则仅存内存。
fn get_or_create_key_state(state: &AppState, user_id: &str) -> KeyState {
    let mut states = state.key_states.lock().unwrap();
    if let Some(s) = states.get(user_id) {
        return s.clone();
    }
    let path = state.fingerprint_path.lock().unwrap().clone();
    let id = fingerprint::key_id(user_id);
    let fingerprint = match path
        .as_deref()
        .and_then(|p| fingerprint::load_store(p).remove(&id))
    {
        Some(fp) => {
            log::info(crate::i18n::pick("已从磁盘恢复账户指纹", "Fingerprint restored for user"));
            fp
        }
        None => {
            let fp = fingerprint::generate();
            if let Some(p) = path.as_deref() {
                if let Err(e) = fingerprint::remember(p, &id, &fp) {
                    log::warn(&format!(
                        "{}: {e}",
                        crate::i18n::pick(
                            "指纹持久化失败，本次仅存内存",
                            "Failed to persist the fingerprint; kept in memory only"
                        )
                    ));
                }
            }
            log::info(crate::i18n::pick("已为账户生成新指纹", "Fingerprint generated for user"));
            fp
        }
    };
    let ks = KeyState {
        fingerprint,
        next_init_at: 0,
    };
    states.insert(user_id.to_string(), ks.clone());
    ks
}

/// 测试用：暴露指定 userId 的指纹（触发一次「恢复或生成」逻辑）。
#[cfg(test)]
pub fn key_fingerprint_for_test(state: &AppState, user_id: &str) -> fingerprint::Fingerprint {
    get_or_create_key_state(state, user_id).fingerprint
}
/// 读取当前模拟的 command-code CLI 版本号。
pub fn cc_version(state: &AppState) -> String {
    state.cc_version.read().unwrap().clone()
}

/// 初始化预请求（fingerprint/record + lifecycle-events），8h + 2h 抖动刷新。
/// `api_key` 用于构造请求头，`user_id` 用于键控初始化状态（换 key 不重置）。
pub async fn ensure_initialized(state: &AppState, api_key: &str, user_id: &str) {
    let now = now_millis();
    let next = get_or_create_key_state(state, user_id).next_init_at;
    if now < next {
        return;
    }

    let cfg = state.config.read().unwrap().clone();
    let mut headers = base_headers(state, api_key);
    // ZDR 模式开启时预请求也携带 x-cmd-zdr（与生成请求一致）
    if cfg.zdr {
        headers.insert("x-cmd-zdr", "1".parse().unwrap());
    }
    // 指纹同样以 user_id 键控：同一账户换 key 后保持同一设备身份
    let fingerprint = get_or_create_key_state(state, user_id).fingerprint;
    let client = state.client();

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
                Ok(Ok(r)) if r.status().is_success() => log::info(crate::i18n::pick(
                    "指纹/生命周期预请求已发送",
                    "Fingerprint/lifecycle event sent",
                )),
                Ok(Ok(r)) => log::warn(&format!(
                    "{}: {}",
                    crate::i18n::pick("Command Code 预请求失败", "Command Code pre-request failed"),
                    r.status()
                )),
                Ok(Err(e)) => log::warn(&format!(
                    "{}: {e}",
                    crate::i18n::pick("Command Code 预请求出错", "Command Code pre-request error")
                )),
                Err(_) => log::warn(crate::i18n::pick("Command Code 预请求超时", "Command Code pre-request timeout")),
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
    // 与读取处（第 187 行）保持同一键控键，否则 user_id 条目的 next_init_at
    // 永远停在 0，8h 节流会失效、每个请求都重发两个预请求
    state
        .key_states
        .lock()
        .unwrap()
        .get_mut(user_id)
        .map(|s| s.next_init_at = next_at);
    log::info(crate::i18n::pick(
        "指纹/生命周期预请求已排定下次刷新",
        "Fingerprint/lifecycle next refresh scheduled",
    ));
}

/// 构造 Command Code 上游公共请求头：JSON 内容类型、CLI 环境标识、Bearer 鉴权与 CLI 版本号。
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
    headers.insert("User-Agent", "cli".parse().unwrap());
    headers
}

/// 转发到 Command Code API /alpha/generate。
///
/// - `body`：已由 convert 模块构造好的 CLI 信封请求体；
/// - `api_key`：上游账户 key（构造 Bearer 与伪造头）；
/// - `user_id`：账户唯一标识（会话键控，换 key 时会话延续）；
/// - `incoming_headers`：下游客户端请求头，用于透传会话 ID 与 zdr 开关；
/// - `prompt_cache_key`：请求体的 prompt_cache_key（兼作会话 ID 候选）；
/// - 返回上游原始 `Response`（调用方负责读取 NDJSON 流与状态码）。
pub async fn forward_to_cc(
    state: &AppState,
    body: &Value,
    api_key: &str,
    user_id: &str,
    incoming_headers: &HeaderMap,
    prompt_cache_key: Option<&str>,
) -> Result<Response, reqwest::Error> {
    let cfg = state.config.read().unwrap().clone();
    let url = format!("{}/alpha/generate", cfg.api_base);
    let session_id = get_session_id(state, incoming_headers, user_id, prompt_cache_key);
    let slug = fake_project_slug(&session_id);

    // 在公共头基础上补齐 CLI 会话/项目/链路追踪等伪装头
    let mut headers = base_headers(state, api_key);
    // session_id 已由 get_session_id 保证为可见 ASCII；此处仍用 try_from 兜底，
    // 避免任何异常输入导致 parse().unwrap() panic（release 下 panic=abort 会整进程退出）
    let session_header = reqwest::header::HeaderValue::try_from(session_id.as_str())
        .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("session"));
    headers.insert("x-session-id", session_header);
    headers.insert("x-taste-learning", "false".parse().unwrap());
    headers.insert("x-project-slug", slug.parse().unwrap());
    headers.insert("traceparent", generate_traceparent().parse().unwrap());
    // ZDR 模式：配置开启或客户端请求头显式要求时发送 x-cmd-zdr: 1
    if cfg.zdr || incoming_headers.get("x-cmd-zdr").and_then(|v| v.to_str().ok()) == Some("1") {
        headers.insert("x-cmd-zdr", "1".parse().unwrap());
    }

    state
        .client()
        .post(&url)
        .headers(headers)
        .json(body)
        .send()
        .await
}

/// 从 npm registry 刷新 Command Code 版本（启动时 + 每 24h）。
pub async fn refresh_cc_version(state: &AppState) {
    refresh_cc_version_from(state, "https://registry.npmjs.org/command-code/latest").await;
}

/// 从给定 registry URL 拉取最新版本号并写回状态（URL 可注入供测试）。
pub(crate) async fn refresh_cc_version_from(state: &AppState, url: &str) {
    let res = tokio::time::timeout(Duration::from_secs(10), async {
        state.client().get(url).send().await
    })
    .await;
    match res {
        Ok(Ok(r)) if r.status().is_success() => {
            if let Ok(pkg) = r.json::<Value>().await {
                if let Some(v) = pkg.get("version").and_then(|v| v.as_str()) {
                    *state.cc_version.write().unwrap() = v.to_string();
                    log::info(&format!(
                    "{} {v}",
                    crate::i18n::pick("已从 npm 刷新 Command Code 版本：", "Command Code version refreshed from npm:")
                ));
                    return;
                }
            }
        }
        Ok(Ok(r)) => log::warn(&format!(
                    "{}: {}",
                    crate::i18n::pick("Command Code 版本获取失败", "Command Code version fetch failed"),
                    r.status()
                )),
        Ok(Err(e)) => log::warn(&format!(
                    "{}: {e}",
                    crate::i18n::pick("Command Code 版本获取出错", "Command Code version fetch error")
                )),
        Err(_) => log::warn(crate::i18n::pick("Command Code 版本获取超时", "Command Code version fetch timeout")),
    }
    log::warn(crate::i18n::pick(
        "Command Code 版本获取失败，沿用当前值",
        "Command Code version fetch failed, using current",
    ));
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
        if !cache.models.is_empty() && now - cache.fetched_at < cfg.model_refresh_interval_secs * 1000 {
            return (cache.models.clone(), false);
        }
    }

    if let Some(key) = api_key {
        if cfg.use_provider_models {
            let url = format!("{}/provider/v1/models", cfg.api_base);
            let headers = base_headers(state, key);
            match tokio::time::timeout(Duration::from_secs(10), async {
                state.client().get(&url).headers(headers).send().await
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
                                log::info(&format!(
                                    "{} {}",
                                    crate::i18n::pick("已从 Provider API 拉取模型数：", "Fetched models from Provider API:"),
                                    models.len()
                                ));
                                return (models, false);
                            }
                        }
                    }
                }
                Ok(Ok(r)) => log::warn(&format!(
                    "{}: {}",
                    crate::i18n::pick("Provider 模型拉取失败", "Provider models fetch failed"),
                    r.status()
                )),
                Ok(Err(e)) => log::warn(&format!(
                    "{}: {e}",
                    crate::i18n::pick("Provider 模型拉取出错", "Provider models fetch error")
                )),
                Err(_) => log::warn(crate::i18n::pick("Provider 模型拉取超时", "Provider models fetch timeout")),
            }
        }
    } else if cfg.use_provider_models {
        log::info(crate::i18n::pick(
            "未提供 API Key，使用内置模型列表",
            "No API key provided; using the built-in model list",
        ));
    }

    log::warn(crate::i18n::pick(
        "Provider 模型拉取失败，使用内置模型列表",
        "Provider models fetch failed, using hardcoded list",
    ));
    (hardcoded_models(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 伪造项目 slug：取 sessionId 前 4 位 hex 选词拼名；无法成词时回退 cc-proxy。
    #[test]
    fn fake_project_slug_format() {
        let slug = fake_project_slug("abcd1234-xxxx");
        assert!(slug.starts_with("users-dev-projects-"), "清洗后的路径 slug: {slug}");
        assert!(slug.contains("abcd"));
        // 短 id 回退 0000 也不 panic
        let slug2 = fake_project_slug("zz");
        assert!(!slug2.is_empty());
        // 非 hex 的前 4 位回退 0000 选词，slug 仍非空（清洗回退分支为防御性代码）
        let slug3 = fake_project_slug("****-****");
        assert!(slug3.starts_with("users-dev-projects-"));
    }

    /// 会话 ID 解析优先级：下游头 ≥8 采信 → prompt_cache_key 可见 ASCII 采信 → 本地会话。
    #[test]
    fn get_session_id_priority() {
        let state = AppState::new(crate::proxy::config::Config::default());
        let mut headers = HeaderMap::new();
        // 1) 下游头命中
        headers.insert("x-session-id", "sess-from-header-01".parse().unwrap());
        let id = get_session_id(&state, &headers, "u1", Some("12345678"));
        assert_eq!(id, "sess-from-header-01");
        // 2) 短头被忽略，prompt_cache_key 采信
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", "short".parse().unwrap());
        let id = get_session_id(&state, &headers, "u1", Some("cache-key-123456"));
        assert_eq!(id, "cache-key-123456");
        // 3) 含中文的 prompt_cache_key 不安全 → 回退本地会话
        let id = get_session_id(&state, &headers, "u1", Some("含中文的会话键-不可用作头"));
        assert!(!id.contains("中"));
        // 4) 全部缺失 → 本地生成并复用
        let empty = HeaderMap::new();
        let a = get_session_id(&state, &empty, "u1", None);
        let b = get_session_id(&state, &empty, "u1", None);
        assert_eq!(a, b, "同账户会话应粘滞");
        // 过期后更换新会话
        {
            let mut sessions = state.sessions.lock().unwrap();
            sessions.get_mut("u1").unwrap().expires_at = 0;
        }
        let c = get_session_id(&state, &empty, "u1", None);
        assert_ne!(a, c);
    }
}
