use axum::http::HeaderMap;
use rand::Rng;
use reqwest::Response;
use serde_json::{json, Value};
use std::time::Duration;

use super::fingerprint;
use super::log;
use super::state::{now_millis, AppState, KeyState, ModelInfo, SessionEntry};


/// 生成 W3C traceparent 头（`00-{32位trace}-{16位parent}-01`），模拟 CLI 的链路追踪。
pub fn generate_traceparent() -> String {
    let mut rng = rand::thread_rng();
    let trace: String = (0..16).map(|_| format!("{:02x}", rng.gen::<u8>())).collect();
    let parent: String = (0..8).map(|_| format!("{:02x}", rng.gen::<u8>())).collect();
    format!("00-{trace}-{parent}-01")
}

/// CLI 的 slug 规则：对**完整工作目录**做 slugify（@sindresorhus/slugify），空则 "root"，
/// 无随机后缀；同一个 slug 也是 CLI 本地会话目录名。slug 与 config.workingDir 同源：
/// `slug = slugify(workingDir)`。
///
/// 与 DEVICE_PROFILE.projectDir 配合：伪装项目目录恒为伪造值，slug 随项目目录自洽，
/// 不再随会话变化（旧版 fake_project_slug 由 sessionId 派生的行为已废弃）。
pub fn slugify_project_path(p: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    // 去掉盘符前缀（CLI 的 slugify 对完整路径先剥掉 "C:" 这类盘符，大小写不敏感）
    let lower = p.to_lowercase();
    let p = lower.strip_prefix("c:").unwrap_or(&lower);
    for ch in p.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "root".to_string()
    } else {
        out
    }
}

/// 判断字符串能否安全用作 HTTP 头值：仅可见 ASCII（可含空格），排除控制字符与多字节字符。
///
/// 会话 ID 最终会写入 `x-session-id` 头，而候选来源（请求体 prompt_cache_key）由下游
/// 任意填写，故必须过滤，否则 HeaderValue 解析失败。
fn is_header_safe(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
}

/// 会话 ID 是否为合法 UUID（v4 形状）：CLI 的 toWireThreadId 只有合法 UUID 才放进
/// 信封 threadId 字段，否则整键省略。
fn is_uuid(s: &str) -> bool {
    let lens = [8usize, 4, 4, 4, 12];
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip(lens.iter())
            .all(|(p, l)| p.len() == *l && p.bytes().all(|b| b.is_ascii_hexdigit()))
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

/// 取该账户（按 userId 标识）的伪装状态；指纹由 apiKey 确定性派生
/// （同一 userId 跨重启/换进程恒为同一台设备，无需磁盘持久化）。
fn get_or_create_key_state(state: &AppState, user_id: &str) -> KeyState {
    let mut states = state.key_states.lock().unwrap();
    if let Some(s) = states.get(user_id) {
        return s.clone();
    }
    let cfg = state.config.read().unwrap().clone();
    // 指纹由该账户的 apiKey 确定性派生；cc_accounts 里找不到（如测试构造）时
    // 回退用 userId 作为派生源，保证同一账户在同一配置下稳定
    let key = cfg
        .cc_accounts
        .iter()
        .find(|a| a.user_id == user_id)
        .map(|a| a.key.as_str())
        .unwrap_or(user_id);
    let profile = fingerprint::default_device_profile(&cfg.device_project_dir);
    let fingerprint = fingerprint::generate(key, &cfg.fingerprint_salt, &profile);
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
    let profile = fingerprint::default_device_profile(&cfg.device_project_dir);
    // lifecycle metadata 的 mode 是独立枚举（interactive | non-interactive），
    // 与信封 mode（agent | learning | ...）不是同一个值，分开配置（cli_session_mode）
    let lc_body = json!({
        "eventType": "cli_session_exists",
        "metadata": {
            "sessionId": format!("sess_{}", uuid::Uuid::new_v4().to_string().replace('-', "")[..16].to_string()),
            "cliVersion": version,
            "mode": cfg.cli_session_mode,
            "os": format!("{}-{}", profile.platform, profile.arch),
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
    // api_key 来自用户配置 / 账户库，cc_version 来自 npm registry：都可能是带控制字符的
    // 非法头值，用 try_from 兜底而非 parse().unwrap()（release 下 panic=abort 会整进程退出）
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::try_from(format!("Bearer {api_key}"))
            .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("Bearer")),
    );
    headers.insert(
        "x-command-code-version",
        reqwest::header::HeaderValue::try_from(cc_version(state))
            .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("0.0.0")),
    );
    headers.insert("User-Agent", "cli".parse().unwrap());
    headers
}

/// 转发到 Command Code API /alpha/generate。
///
/// - `body`：已由 convert 模块构造好的 CLI 信封请求体（session_id 为合法 UUID 时
///   会补入 threadId 字段，对齐 CLI 的 toWireThreadId）；
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
    // 与 DEVICE_PROFILE.projectDir 同源：slug = slugify(workingDir)，不再随会话变化
    let profile = fingerprint::default_device_profile(&cfg.device_project_dir);
    let slug = slugify_project_path(&profile.project_dir);

    // CLI 的 toWireThreadId：只有合法 UUID 才放进信封，否则整个键省略。
    // session_id 可能来自下游头 / prompt_cache_key（非 UUID），此时不注入。
    let mut wire_body = body.clone();
    if is_uuid(&session_id) {
        wire_body["threadId"] = json!(session_id);
    }

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
        .json(&wire_body)
        .send()
        .await
}

/// 从 npm registry 刷新 Command Code 版本（启动时 + 每 24h）。
pub async fn refresh_cc_version(state: &AppState) {
    refresh_cc_version_from(state, "https://registry.npmjs.org/command-code/latest").await;
}

/// 从给定 registry URL 拉取最新版本号并写回状态（URL 可注入供测试）。
///
/// 版本号同样写回配置的本地缓存（只写该字段），使下次启动无需等待网络即可显示真实版本；
/// 写入前比对旧值，未变化时不落库。
pub(crate) async fn refresh_cc_version_from(state: &AppState, url: &str) {
    let res = tokio::time::timeout(Duration::from_secs(10), async {
        state.client().get(url).send().await
    })
    .await;
    match res {
        Ok(Ok(r)) if r.status().is_success() => {
            if let Ok(pkg) = r.json::<Value>().await {
                if let Some(v) = pkg.get("version").and_then(|v| v.as_str()) {
                    let v = v.to_string();
                    let changed = *state.cc_version.read().unwrap() != v;
                    *state.cc_version.write().unwrap() = v.clone();
                    // 只在版本号确实变化时写库，避免每 24h 的一次无谓写入
                    if changed {
                        state.config.write().unwrap().cc_version_cache = v.clone();
                        persist_cc_version_cache(state);
                    }
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

/// 把当前内存中的版本号缓存写入 SQLite settings 表（单字段写入，不动其它配置）。
///
/// 用单字段写入而非整体 `save_config`：内存配置可能已被环境变量覆写，整体落库会把
/// 仅应作用于本次运行的 env 值写成持久设置。写入失败只记日志——版本号缓存是尽力而为
/// 的优化，写不进去不影响请求转发，下次启动回落到内置占位版本。
///
/// 只写 settings 表（配置主存）；config.json 是旧版的迁移/兜底源，会在用户下次保存
/// 配置时由 `Config::save` 一并带上该字段，无需在此额外写盘。
fn persist_cc_version_cache(state: &AppState) {
    let version = state.cc_version.read().unwrap().clone();
    let conn_guard = state.usage.lock().unwrap();
    let Some(conn) = conn_guard.as_ref() else {
        return;
    };
    // 与 settings 表其它行同格式：存 JSON 序列化值（字符串含引号），读回时按值类型解析
    if let Err(e) = super::settings::save_setting(conn, "cc_version_cache", &json!(version).to_string()) {
        log::warn(&format!(
            "{}: {e}",
            crate::i18n::pick("上游版本号缓存写入失败", "Failed to persist the upstream version cache")
        ));
    }
}

/// 模型列表：Provider 端点动态拉取（公开接口，按配置间隔缓存），成功后整表落库；
/// 失败时回退数据库缓存的列表，再回退内置表。
///
/// 返回 `(模型列表, 是否为兜底列表)`；缓存未过期时直接命中缓存不发请求。
pub async fn fetch_models(state: &AppState) -> (Vec<ModelInfo>, bool) {
    let cfg = state.config.read().unwrap().clone();
    let now = now_millis();
    // 缓存非空且未过期时直接命中；用独立块提前释放读锁，避免后续写缓存时死锁
    {
        let cache = state.models.read().unwrap();
        // 用饱和运算：系统时间回拨会让 now < fetched_at 下溢（debug 下 panic），
        // 配置里的刷新间隔若被设得极大，乘法也会溢出。
        let ttl = cfg.model_refresh_interval_secs.saturating_mul(1000);
        if !cache.models.is_empty() && now.saturating_sub(cache.fetched_at) < ttl {
            return (cache.models.clone(), false);
        }
    }

    if cfg.use_provider_models {
        // 公开端点，无需鉴权
        let url = format!("{}/provider/v1/models", cfg.api_base);
        let resp =
            tokio::time::timeout(Duration::from_secs(10), state.client().get(&url).send()).await;
        match resp {
            Ok(Ok(r)) if r.status().is_success() => {
                let entries = match r.json::<Value>().await {
                    Ok(data) => data
                        .get("data")
                        .and_then(|d| d.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|m| {
                                    serde_json::from_value::<super::models::RemoteModel>(m.clone())
                                        .ok()
                                })
                                .map(super::models::enrich_remote)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                    Err(e) => {
                        log::warn(&format!(
                            "{}: {e}",
                            crate::i18n::pick("Provider 模型响应解析出错", "Provider models response parse error")
                        ));
                        Vec::new()
                    }
                };
                if !entries.is_empty() {
                    *state.models.write().unwrap() = super::state::ModelsCache {
                        models: entries.clone(),
                        fetched_at: now,
                    };
                    super::models::persist_models_for(state, &entries);
                    log::info(&format!(
                        "{} {}",
                        crate::i18n::pick("已从 Provider API 拉取模型数：", "Fetched models from Provider API:"),
                        entries.len()
                    ));
                    return (entries, false);
                }
                log::warn(crate::i18n::pick(
                    "Provider 模型列表为空",
                    "Provider model list is empty",
                ));
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

    log::warn(crate::i18n::pick(
        "模型列表未从 Provider 拉取成功，回退数据库缓存的列表",
        "Model list not fetched from provider; falling back to the cached list",
    ));
    (super::models::load_models_for(state), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 项目 slug 对齐 CLI 规则：slugify(workingDir)，空路径回退 root。
    #[test]
    fn slugify_project_path_format() {
        // 默认伪造项目目录 → users-dev-projects-app（盘符 c: 被剥除）
        let profile = super::fingerprint::default_device_profile("");
        let slug = slugify_project_path(&profile.project_dir);
        assert_eq!(slug, "users-dev-projects-app");
        // 自定义项目目录随其内容 slugify（盘符剥除），且空路径回退 root
        assert_eq!(slugify_project_path("C:\\Users\\me\\proj"), "users-me-proj");
        assert_eq!(slugify_project_path(""), "root");
        // 连续分隔符归一为单个连字符
        assert_eq!(slugify_project_path("a//b__c"), "a-b-c");
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
