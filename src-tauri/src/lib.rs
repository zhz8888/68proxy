//! Tauri 应用核心：注册前端可调用的 IPC 命令（代理启停、配置、凭据、日志、
//! 模型列表、端口检查、开机自启等），并在 setup 阶段完成配置加载、托盘构建、
//! 日志/请求事件转发与窗口生命周期钩子的装配。

mod credentials;
/// 后端国际化：错误码生成与日志文案语言选择（详见模块文档）。
mod i18n;
mod proxy;

/// 开发调试桥接：仅在 debug 构建编译，让浏览器直连前端页面时也能调用后端命令。
#[cfg(debug_assertions)]
mod dev_bridge;

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_autostart::ManagerExt;

/// Tauri 托管状态：配置路径 + 代理共享状态。
struct AppCtx {
    config_path: PathBuf,
    usage_path: PathBuf,
    proxy_state: Arc<proxy::state::AppState>,
}

/// 启动本地代理。返回启动后的代理状态 JSON；未保存 API Key 或端口被占用时返回错误。
#[tauri::command]
async fn proxy_start(app: AppHandle) -> Result<Value, String> {
    start_proxy_inner(&app).await
}

/// 停止本地代理，返回停止后的代理状态 JSON。
#[tauri::command]
async fn proxy_stop(app: AppHandle) -> Result<Value, String> {
    stop_proxy_inner(&app);
    Ok(status_value(&app))
}

/// 重启本地代理：先停止，等待旧监听真正释放端口后再启动，返回新的代理状态 JSON。
#[tauri::command]
async fn proxy_restart(app: AppHandle) -> Result<Value, String> {
    stop_proxy_inner(&app);
    wait_port_released(&app).await;
    start_proxy_inner(&app).await
}

/// 查询当前代理状态（是否运行、监听地址、接入 URL、运行时长等）的 JSON。
#[tauri::command]
fn proxy_status(app: AppHandle) -> Value {
    status_value(&app)
}

/// 读取应用配置。出于安全考虑返回前会清空本地转发 Key 与 Command Code 账户列表
/// （均由专用命令管理）。
#[tauri::command]
fn config_get(app: AppHandle) -> proxy::config::Config {
    let ctx = app.state::<AppCtx>();
    let mut cfg = ctx.proxy_state.config.read().unwrap().clone();
    cfg.local_api_key = String::new();
    cfg.cc_accounts = Vec::new();
    cfg
}

/// 校验并保存配置：写入 SQLite settings 表（主）+ config.json（镜像兜底），
/// 并同步到内存中的代理状态。
/// 返回 `needs_restart`：端口/主机变更且代理正在运行时需要重启才能生效。
#[tauri::command]
fn config_save(app: AppHandle, mut config: proxy::config::Config) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    // 本地转发 Key 与 Command Code 账户由 local_key_* / account_* 管理，config_save 不接收，
    // 保存前保留原值
    let stored = ctx.proxy_state.config.read().unwrap().clone();
    config.local_api_key = stored.local_api_key;
    config.cc_accounts = stored.cc_accounts;
    // 账户使用规则由 account_routing_set 专门管理，此处保留原值避免被前端默认值覆盖
    config.account_strategy = stored.account_strategy;
    config.preferred_account_id = stored.preferred_account_id;
    // 主题由 theme_set 专门管理，同样保留原值
    config.theme = stored.theme;
    // 语言由 language_set 专门管理，同样保留原值
    config.language = stored.language;
    // 校验放在保留字段之后：被保留的字段不应触发校验失败
    config.validate()?;
    // 主存 SQLite settings 表
    {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        proxy::settings::save_config(conn, &config)?;
    }
    // 镜像到 config.json（兜底），失败仅记 warn 不阻断保存
    if let Err(e) = config.save(&ctx.config_path) {
        proxy::log::warn(&format!(
            "{}: {e}",
            i18n::pick("config.json 镜像写入失败", "Failed to write the config.json mirror")
        ));
    }
    let prev = ctx.proxy_state.config.read().unwrap().clone();
    let needs_restart =
        (prev.port != config.port || prev.host != config.host) && ctx.proxy_state.is_running();
    // 出站代理配置变更时热更新 HTTP 客户端（重建带代理的 client），无需重启
    let proxy_changed = prev.proxy_mode != config.proxy_mode
        || prev.proxy_type != config.proxy_type
        || prev.proxy_host != config.proxy_host
        || prev.proxy_port != config.proxy_port
        || prev.proxy_username != config.proxy_username
        || prev.proxy_password != config.proxy_password;
    *ctx.proxy_state.config.write().unwrap() = config.clone();
    if proxy_changed {
        if let Err(e) = ctx.proxy_state.rebuild_client(&config) {
            // 重建失败（如代理地址非法）不阻断保存，仅记录并保留旧 client
            proxy::log::warn(&format!(
                "{}: {e}",
                i18n::pick("代理客户端重建失败，沿用旧配置", "Failed to rebuild the proxy client; keeping the previous one")
            ));
        }
    }
    proxy::log::info(i18n::pick("配置已保存", "Settings saved"));
    Ok(json!({ "needs_restart": needs_restart }))
}

/// 查询本地转发 Key 状态：返回是否已生成（has_key）与掩码后的展示文本（masked）。
#[tauri::command]
fn local_key_get(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let key = {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        credentials::load_local_key(conn)?
    };
    Ok(json!({
        "has_key": key.is_some(),
        "masked": key.as_deref().map(credentials::mask_key).unwrap_or_default(),
    }))
}

/// 保存本地转发 Key（sk_ 开头）到设置库并刷新内存缓存；同步内存配置。入参为完整的 Key。
#[tauri::command]
fn local_key_set(app: AppHandle, key: String) -> Result<(), String> {
    let ctx = app.state::<AppCtx>();
    {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        credentials::save_local_key(conn, &key)?;
    }
    ctx.proxy_state.config.write().unwrap().local_api_key = key.trim().to_string();
    Ok(())
}

/// 随机生成一个新的本地转发 Key（sk- + 32 位随机 hex）并保存，返回掩码展示文本。
#[tauri::command]
fn local_key_generate(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let key = {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        let key = credentials::generate_local_key();
        credentials::save_local_key(conn, &key)?;
        key
    };
    ctx.proxy_state.config.write().unwrap().local_api_key = key.clone();
    Ok(json!({ "key": key, "masked": credentials::mask_key(&key) }))
}

/// 删除已保存的本地转发 Key（清空设置库字段、内存缓存与内存配置）。
#[tauri::command]
fn local_key_delete(app: AppHandle) -> Result<(), String> {
    let ctx = app.state::<AppCtx>();
    {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        credentials::delete_local_key(conn)?;
    }
    ctx.proxy_state.config.write().unwrap().local_api_key = String::new();
    Ok(())
}

/// 查询 Command Code 账户列表：返回掩码 key、userId、显示名与来源，条目带下标供删除。
#[tauri::command]
fn account_list(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let accounts = {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        credentials::load_accounts(conn)?
    };
    Ok(json!({
        "accounts": accounts
            .iter()
            .enumerate()
            .map(|(i, a)| json!({
                "index": i,
                "masked": credentials::mask_key(&a.key),
                "userId": a.user_id,
                "userName": a.user_name,
                "source": a.source,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// 新增一个 Command Code 账户：先调用上游 whoami 验证 key 并补全 userId/userName，再入库。
/// 可选 `user_name` 作为自定义显示名（缺省用 whoami 返回的 userName）。
#[tauri::command]
async fn account_add(app: AppHandle, key: String, user_name: Option<String>) -> Result<(), String> {
    let key = key.trim().to_string();
    if !key.starts_with("user_") {
        return Err(i18n::err("account_key_prefix"));
    }
    let ctx = app.state::<AppCtx>();
    let api_base = ctx.proxy_state.config.read().unwrap().api_base.clone();
    let (user_id, default_name) =
        credentials::verify_account_key(&ctx.proxy_state.client(), &api_base, &key).await?;
    let display = user_name.unwrap_or(default_name);
    let accounts = {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        credentials::add_account(
            conn,
            &proxy::config::Account {
                key: key.clone(),
                user_id: user_id.clone(),
                user_name: display.clone(),
                source: "manual".into(),
                added_at: proxy::state::now_secs(),
            },
        )?
    };
    ctx.proxy_state.config.write().unwrap().cc_accounts = accounts;
    Ok(())
}

/// 更新指定 userId 账户的自定义显示名；落库后同步内存配置。
#[tauri::command]
fn account_rename(app: AppHandle, user_id: String, user_name: String) -> Result<(), String> {
    let ctx = app.state::<AppCtx>();
    let accounts = {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        credentials::rename_account(conn, &user_id, &user_name)?
    };
    ctx.proxy_state.config.write().unwrap().cc_accounts = accounts;
    Ok(())
}

/// 移除指定下标的 Command Code 账户（0 起），下标越界时返回错误；落库后同步内存配置。
#[tauri::command]
fn account_remove(app: AppHandle, index: usize) -> Result<(), String> {
    let ctx = app.state::<AppCtx>();
    let accounts = {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        credentials::remove_account_at(conn, index)?
    };
    ctx.proxy_state.config.write().unwrap().cc_accounts = accounts;
    Ok(())
}

/// 启动浏览器授权登录：后端起 loopback 回调服务器，返回授权 URL 供前端打开浏览器。
#[tauri::command]
async fn auth_login_start(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let url = proxy::auth_login::start_auth_login(&ctx.proxy_state).await?;
    let port = ctx
        .proxy_state
        .auth_login
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.port)
        .unwrap_or(0);
    Ok(json!({ "url": url, "port": port }))
}

/// 查询浏览器授权登录结果：`pending`（等待中）/ `success`（含账户信息）/ `denied` / `failed` / `idle`。
/// success 时按 userId 去重入库并同步内存配置。
#[tauri::command]
async fn auth_login_poll(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let value = proxy::auth_login::poll_auth_login(&ctx.proxy_state);
    if value.get("status").and_then(|v| v.as_str()) == Some("success") {
        let api_key = value["account"]["key"].as_str().unwrap_or("").to_string();
        let user_id = value["account"]["userId"].as_str().unwrap_or("").to_string();
        let user_name = value["account"]["userName"].as_str().unwrap_or("").to_string();
        let accounts = {
            let guard = ctx.proxy_state.usage.lock().unwrap();
            let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
            credentials::add_account(
                conn,
                &proxy::config::Account {
                    key: api_key,
                    user_id: user_id.clone(),
                    user_name,
                    source: "oauth".into(),
                    added_at: proxy::state::now_secs(),
                },
            )?
        };
        ctx.proxy_state.config.write().unwrap().cc_accounts = accounts;
    }
    Ok(value)
}

/// 取消进行中的浏览器授权登录。
#[tauri::command]
fn auth_login_cancel(app: AppHandle) -> Result<(), String> {
    let ctx = app.state::<AppCtx>();
    proxy::auth_login::cancel_auth_login(&ctx.proxy_state);
    Ok(())
}

/// 获取可用模型列表。`force` 为 true 时先清空缓存再向上游拉取；
/// 返回 `{ data: 模型列表, fallback: 是否为兜底列表 }`。
/// 拉取成功后整表落库（models 表），失败时回退数据库缓存 / 内置表。
#[tauri::command]
async fn models_get(app: AppHandle, force: bool) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    if force {
        *ctx.proxy_state.models.write().unwrap() = proxy::state::ModelsCache {
            models: Vec::new(),
            fetched_at: 0,
        };
    }
    let (list, fallback) = proxy::cc_client::fetch_models(&ctx.proxy_state).await;
    Ok(json!({ "data": list, "fallback": fallback }))
}

/// 获取当前生效的模型价格表（促销、分档费率与闲/忙时信息），供前端模型页展示。
///
/// 数据源为 SQLite `model_pricing` 表（首次启动由内置表播种，见 models 模块）。
#[tauri::command]
fn models_catalog() -> Value {
    proxy::pricing::catalog_json()
}

/// 覆盖写入模型价格（数据更新用）：按模型 ID UPSERT 落库并刷新运行时注册表，
/// 使价目/促销/闲忙时更新无需重新发版；未提及的旧模型保留，同 ID 的旧数据被覆盖。
#[tauri::command]
fn models_catalog_update(
    app: AppHandle,
    models: Vec<proxy::pricing::ModelPricing>,
    source: Option<String>,
) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let (updated, all) = {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        let n = proxy::models::upsert_pricing(conn, &models, source.as_deref().unwrap_or("manual"))?;
        (n, proxy::models::load_pricing(conn))
    };
    proxy::pricing::set_models(all);
    proxy::log::info(&format!(
        "{} {updated}",
        i18n::pick("模型价格已更新", "Model pricing updated:")
    ));
    Ok(json!({ "updated": updated }))
}

/// 获取当前 Command Code 账户的套餐信息与各模型准入结果（标注模型页的可用性）。
///
/// `force` 为 true 时跳过 5 分钟缓存，强制重新拉取上游套餐数据。
#[tauri::command]
async fn plan_status(app: AppHandle, force: bool) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let account_key = credentials::accounts_from_state(&ctx.proxy_state)
        .first()
        .map(|a| a.key.clone());
    let plan = proxy::plans::plan_context(&ctx.proxy_state, account_key.as_deref(), force).await;
    Ok(proxy::plans::plan_status_json(&plan))
}

/// 获取全部 Command Code 账户的额度快照（套餐、月/购买/赠送余额、5 小时与周窗口限额、组织限额）。
///
/// 命中额度缓存（60s TTL）时直接复用，未命中才向上游拉取并回填；返回顺序与账户列表一致。
#[tauri::command]
async fn accounts_quota(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let list = proxy::quota::snapshot_all(&ctx.proxy_state).await;
    Ok(serde_json::to_value(list)
        .map_err(|e| i18n::err_args("serialize_failed", &[&e.to_string()]))?)
}

/// 读取界面主题：`system`（跟随系统）/ `dark` / `light`。
#[tauri::command]
fn theme_get(app: AppHandle) -> Value {
    let ctx = app.state::<AppCtx>();
    json!({ "theme": ctx.proxy_state.config.read().unwrap().theme })
}

/// 保存界面主题并持久化（settings 表 + 内存状态）；非法值报错。
///
/// 主题即时生效（前端切换 `<html>` 的 dark 类），此命令只负责持久化，无需重启。
#[tauri::command]
fn theme_set(app: AppHandle, theme: String) -> Result<(), String> {
    if !matches!(theme.as_str(), "system" | "dark" | "light") {
        return Err(i18n::err("theme_invalid"));
    }
    let ctx = app.state::<AppCtx>();
    let updated = {
        let mut cfg = ctx.proxy_state.config.write().unwrap();
        cfg.theme = theme;
        cfg.clone()
    };
    let guard = ctx.proxy_state.usage.lock().unwrap();
    let conn = guard
        .as_ref()
        .ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
    proxy::settings::save_config(conn, &updated)
}

/// 读取界面语言：`zh`（简体中文）/ `en`（英文）。
#[tauri::command]
fn language_get(app: AppHandle) -> Value {
    let ctx = app.state::<AppCtx>();
    json!({ "language": ctx.proxy_state.config.read().unwrap().language })
}

/// 保存界面语言并持久化（settings 表 + 内存状态 + 后端日志语言）；非法值报错。
///
/// 语言即时生效（前端切换 i18next 语言），此命令只负责持久化与后端日志语言同步，无需重启。
#[tauri::command]
fn language_set(app: AppHandle, language: String) -> Result<(), String> {
    if !matches!(language.as_str(), "zh" | "en") {
        return Err(i18n::err("language_invalid"));
    }
    let ctx = app.state::<AppCtx>();
    // 先持久化再改内存/日志语言：落库失败时前端会弹错，此时不应让后端语言与库中值分叉
    let updated = {
        let cfg = ctx.proxy_state.config.read().unwrap();
        let mut next = cfg.clone();
        next.language = language.clone();
        next
    };
    {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard
            .as_ref()
            .ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        proxy::settings::save_config(conn, &updated)?;
    }
    // 落库成功后同步内存配置、后端日志语言与托盘菜单
    ctx.proxy_state.config.write().unwrap().language = language.clone();
    i18n::set_lang(&language);
    refresh_tray_menu(&app);
    Ok(())
}

/// 读取账户使用规则：`{ strategy, preferred_account_id }`。
#[tauri::command]
fn account_routing_get(app: AppHandle) -> Value {
    let ctx = app.state::<AppCtx>();
    let cfg = ctx.proxy_state.config.read().unwrap();
    json!({
        "strategy": cfg.account_strategy,
        "preferred_account_id": cfg.preferred_account_id,
    })
}

/// 保存账户使用规则并持久化（settings 表 + 内存状态）。
///
/// `strategy`：`round_robin`（轮询，默认）/ `priority`（优先消耗指定账户 + 会话粘滞）；
/// `preferred_account_id`：优先消耗的 userId（空字符串表示自动取剩余额度最多者）。
/// 切换规则时清空全部会话绑定，使新规则立即对所有会话生效。
#[tauri::command]
fn account_routing_set(
    app: AppHandle,
    strategy: String,
    #[allow(non_snake_case)] preferred_account_id: String,
) -> Result<(), String> {
    if !matches!(strategy.as_str(), "round_robin" | "priority") {
        return Err(i18n::err("strategy_invalid"));
    }
    let ctx = app.state::<AppCtx>();
    let updated = {
        let mut cfg = ctx.proxy_state.config.write().unwrap();
        cfg.account_strategy = strategy;
        cfg.preferred_account_id = preferred_account_id;
        cfg.clone()
    };
    {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| i18n::err("settings_store_uninitialized"))?;
        proxy::settings::save_config(conn, &updated)?;
    }
    // 规则变更后原有绑定可能不再符合预期，全部清除以便按新规则重选
    ctx.proxy_state.account_bindings.lock().unwrap().clear();
    proxy::log::info(i18n::pick("账户使用规则已更新", "Account usage rules updated"));
    Ok(())
}

/// 清除全部会话→账户绑定（手动切换账户时调用，强制所有会话按新规则重选）。
#[tauri::command]
fn account_bindings_clear(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let n = {
        let mut bindings = ctx.proxy_state.account_bindings.lock().unwrap();
        let n = bindings.len();
        bindings.clear();
        n
    };
    proxy::log::info(&format!(
        "{} {n} {}",
        i18n::pick("已清除", "Cleared"),
        i18n::pick("条账户会话绑定", "account session bindings")
    ));
    Ok(json!({ "cleared": n }))
}

/// 获取指定 userId 账户的额度快照（账户详情用）。
///
/// `userId` 为账户唯一标识；未找到该账户时返回错误。
#[tauri::command]
async fn account_quota(app: AppHandle, #[allow(non_snake_case)] userId: String) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let accounts = credentials::accounts_from_state(&ctx.proxy_state);
    let Some(a) = accounts.iter().find(|a| a.user_id == userId) else {
        return Err(i18n::err_args("account_not_found", &[&userId]));
    };
    let name = if a.user_name.is_empty() { a.user_id.clone() } else { a.user_name.clone() };
    let masked = credentials::mask_key(&a.key);
    let quota =
        proxy::quota::fetch_account_quota(&ctx.proxy_state, &name, &masked, &a.key).await;
    Ok(serde_json::to_value(quota)
        .map_err(|e| i18n::err_args("serialize_failed", &[&e.to_string()]))?)
}

/// 增量拉取内存日志：`limit` 最多返回条数（默认 200），`after_seq` 只返回序号大于它的条目。
#[tauri::command]
fn logs_get(limit: Option<usize>, after_seq: Option<u64>) -> Value {
    json!(proxy::log::get_logs(limit.unwrap_or(200), after_seq.unwrap_or(0)))
}

/// 清空内存中的全部日志条目。
#[tauri::command]
fn logs_clear() {
    proxy::log::clear_logs();
}

/// 将最近 1000 条日志按「[时间] [级别] 消息」格式导出写入指定文件，返回导出条数。
#[tauri::command]
fn logs_export(path: String) -> Result<usize, String> {
    let entries = proxy::log::get_logs(1000, 0);
    let total = entries.len();
    let mut out = String::new();
    for e in entries {
        let ts = chrono::DateTime::from_timestamp_millis(e.ts as i64)
            .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        out.push_str(&format!("[{ts}] [{level}] {msg}\n", level = e.level, msg = e.msg));
    }
    std::fs::write(&path, out)
        .map_err(|e| i18n::err_args("export_failed", &[&e.to_string()]))?;
    Ok(total)
}

/// 获取最近的中继请求记录，`limit` 为最大条数（默认 20）。
#[tauri::command]
fn requests_get(app: AppHandle, limit: Option<usize>) -> Value {
    let ctx = app.state::<AppCtx>();
    json!(ctx.proxy_state.recent_requests(limit.unwrap_or(20)))
}

/// 获取 token 用量汇总统计。`period` 取值 today / 24h / 7d / 30d / 60d / all。
#[tauri::command]
fn stats_get(app: AppHandle, period: String) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let period = proxy::usage::Period::parse(&period);
    let conn = proxy::usage::open_usage(&ctx.usage_path)?;
    let stats = proxy::usage::get_stats(&conn, period)?;
    Ok(proxy::usage::stats_to_json(&stats))
}

/// 获取 token 用量趋势图数据。`period` 取值同 stats_get。
#[tauri::command]
fn stats_chart(app: AppHandle, period: String) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let period = proxy::usage::Period::parse(&period);
    let conn = proxy::usage::open_usage(&ctx.usage_path)?;
    let chart = proxy::usage::get_chart(&conn, period)?;
    Ok(serde_json::to_value(chart)
        .map_err(|e| i18n::err_args("serialize_failed", &[&e.to_string()]))?)
}

/// 获取最近 `limit` 条用量明细（默认 20，新在前）。
#[tauri::command]
fn stats_recent(app: AppHandle, limit: Option<usize>) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let conn = proxy::usage::open_usage(&ctx.usage_path)?;
    let recent = proxy::usage::query_recent(&conn, limit.unwrap_or(20))?;
    Ok(serde_json::to_value(recent)
        .map_err(|e| i18n::err_args("serialize_failed", &[&e.to_string()]))?)
}

/// 清空全部 token 用量统计，返回被清除的记录条数。
#[tauri::command]
fn stats_clear_all(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let conn = proxy::usage::open_usage(&ctx.usage_path)?;
    let count = proxy::usage::clear_all(&conn)?;
    proxy::log::info(&format!(
        "{} {count}",
        i18n::pick("用量统计已清空，条数：", "Usage stats cleared, records:")
    ));
    Ok(json!({ "cleared": count }))
}

/// 检查本机端口是否被占用：尝试绑定 127.0.0.1:port，返回 `{ in_use, pid }`（pid 仅 Windows 可解析）。
#[tauri::command]
async fn port_check(port: u16) -> Result<Value, String> {
    match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(_) => Ok(json!({ "in_use": false, "pid": Value::Null })),
        Err(_) => Ok(json!({ "in_use": true, "pid": find_pid_for_port(port).map(|p| json!(p)).unwrap_or(Value::Null) })),
    }
}

/// 结束占用指定端口的所有进程（用户在配置中选择的端口）。
#[tauri::command]
async fn port_free(port: u16) -> Result<Value, String> {
    let pids = find_pids_for_port(port);
    if pids.is_empty() {
        // 非 Windows 平台无法枚举/结束进程，给出准确提示而非「未被占用」
        #[cfg(not(windows))]
        return Ok(json!({
            "killed": [],
            "message": i18n::msg("port_free_unsupported"),
        }));
        #[cfg(windows)]
        return Ok(json!({ "killed": [], "message": i18n::msg("port_not_in_use") }));
    }
    let mut killed: Vec<u32> = Vec::new();
    for pid in &pids {
        kill_pid(*pid)?;
        killed.push(*pid);
    }
    proxy::log::info(&format!(
        "{} {port} {} {killed:?}",
        i18n::pick("端口", "Port"),
        i18n::pick("已释放，结束进程：", "released, killed processes:")
    ));
    Ok(json!({ "killed": killed, "message": i18n::msg_args("port_freed", &[&killed.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")]) }))
}

/// 查询是否已开启开机自启。
#[tauri::command]
fn autostart_get(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// 设置开机自启开关：`enabled` 为目标状态，与当前状态一致时不做任何操作。
#[tauri::command]
fn autostart_set(app: AppHandle, enabled: bool) -> Result<(), String> {
    let current = app.autolaunch().is_enabled().unwrap_or(false);
    if current == enabled {
        return Ok(());
    }
    if enabled {
        app.autolaunch().enable().map_err(|e| e.to_string())
    } else {
        app.autolaunch().disable().map_err(|e| e.to_string())
    }
}

/// 汇总当前代理状态为前端约定的 JSON 结构（供 proxy_status 命令与事件推送复用）。
fn status_value(app: &AppHandle) -> Value {
    let ctx = app.state::<AppCtx>();
    let running = ctx.proxy_state.is_running();
    let cfg = ctx.proxy_state.config.read().unwrap().clone();
    let started = *ctx.proxy_state.started_at.lock().unwrap();
    let uptime = started.map(|s| (proxy::state::now_millis().saturating_sub(s)) / 1000);
    json!({
        "running": running,
        "port": cfg.port,
        "host": cfg.host,
        "url": format!("http://127.0.0.1:{}/v1", cfg.port),
        "anthropic_url": format!("http://127.0.0.1:{}", cfg.port),
        "cc_version": proxy::cc_client::cc_version(&ctx.proxy_state),
        "uptime_secs": uptime.unwrap_or(0),
    })
}

/// 向所有前端窗口广播 `proxy://status` 状态变更事件。
fn emit_status(app: &AppHandle) {
    let value = status_value(app);
    let _ = app.emit("proxy://status", value.clone());
    // 调试桥接：让浏览器直连的页面也能收到状态推送
    #[cfg(debug_assertions)]
    dev_bridge::publish("proxy://status", value);
}

/// 启动代理的内部实现：校验配置与 API Key、绑定监听端口、在后台任务中运行服务，
/// 并等待运行标志置位后广播状态事件，返回最终代理状态 JSON。已运行时直接返回当前状态。
async fn start_proxy_inner(app: &AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    if ctx.proxy_state.is_running() {
        return Ok(status_value(app));
    }
    // 需要已生成本地转发 Key 且至少一个 Command Code 账户才能启动代理（与运行时鉴权使用同一内存源）
    if credentials::cached_local_key().is_none() {
        return Err(i18n::err("local_key_missing"));
    }
    if credentials::accounts_from_state(&ctx.proxy_state).is_empty() {
        return Err(i18n::err("no_cc_account"));
    }
    let cfg = ctx.proxy_state.config.read().unwrap().clone();
    cfg.validate()?;
    *ctx.proxy_state.config.write().unwrap() = cfg.clone();

    let addr = proxy::server::listen_addr(&cfg);
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        i18n::err_args("port_listen_failed", &[&addr.port().to_string(), &e.to_string()])
    })?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    *ctx.proxy_state.shutdown.lock().unwrap() = Some(tx);
    let st = ctx.proxy_state.clone();
    tauri::async_runtime::spawn(async move {
        let _ = proxy::server::serve(listener, st, rx, true).await;
    });

    // 最多等待 3 秒（60 次 × 50ms），确认后台服务已将运行标志置位
    for _ in 0..60 {
        if ctx.proxy_state.is_running() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // 等待结束后必须确认服务确实进入运行态，否则不能谎报启动成功
    if !ctx.proxy_state.is_running() {
        return Err(i18n::err("proxy_start_failed"));
    }
    proxy::log::info(&format!(
        "{} {}:{}",
        i18n::pick("代理已启动", "Proxy started"),
        cfg.host,
        cfg.port
    ));
    emit_status(app);
    Ok(status_value(app))
}

/// 等待代理监听端口真正释放：轮询尝试绑定配置地址，成功即可再次启动。
///
/// 取代固定 sleep(500ms)：长连接/慢请求场景下旧 listener 可能尚未关闭，
/// 立即重启会因端口占用失败。最多等待 5 秒，超时也让调用方继续尝试（由 bind 报错兜底）。
async fn wait_port_released(app: &AppHandle) {
    let ctx = app.state::<AppCtx>();
    let cfg = ctx.proxy_state.config.read().unwrap().clone();
    let addr = proxy::server::listen_addr(&cfg);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        // 端口已释放：能成功绑定即可退出（绑定后立即释放）
        if std::net::TcpListener::bind(addr).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 停止代理的内部实现：发送关停信号、清理请求记录并广播状态；未运行时直接返回。
fn stop_proxy_inner(app: &AppHandle) {
    let ctx = app.state::<AppCtx>();
    if !ctx.proxy_state.is_running() {
        return;
    }
    ctx.proxy_state.mark_stopped();
    ctx.proxy_state.clear_requests();
    proxy::log::info(i18n::pick("代理已停止", "Proxy stopped"));
    emit_status(app);
}

/// 查找占用指定端口的所有进程 PID（仅 Windows 通过 netstat 解析，其他平台返回空列表）。
fn find_pids_for_port(port: u16) -> Vec<u32> {
    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("netstat")
            .args(["-ano"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut pids: Vec<u32> = Vec::new();
            for line in text.lines() {
                if !line.to_uppercase().contains("LISTENING") {
                    continue;
                }
                // 按列解析本地地址（Proto Local Foreign State PID），取最后一个冒号后的
                // 端口做整数比较：不能用 `:{port}` 子串匹配，否则 :80 会误命中 :8080
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() < 2 {
                    continue;
                }
                let Some(local_port) = cols[1]
                    .rsplit(':')
                    .next()
                    .and_then(|s| s.parse::<u16>().ok())
                else {
                    continue;
                };
                if local_port != port {
                    continue;
                }
                if let Some(pid) = cols.last() {
                    if let Ok(p) = pid.parse::<u32>() {
                        if !pids.contains(&p) {
                            pids.push(p);
                        }
                    }
                }
            }
            return pids;
        }
        Vec::new()
    }
    #[cfg(not(windows))]
    {
        let _ = port;
        Vec::new()
    }
}

/// 查找占用指定端口的第一个进程 PID，找不到时返回 None。
fn find_pid_for_port(port: u16) -> Option<u32> {
    find_pids_for_port(port).first().copied()
}

/// 强制结束指定 PID 的进程（仅 Windows 通过 taskkill 实现，其他平台返回错误）。
fn kill_pid(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output()
            .map_err(|e| i18n::err_args("kill_failed", &[&e.to_string()]))?;
        if output.status.success() {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(i18n::err_args(
                "kill_process_failed",
                &[&pid.to_string(), &msg],
            ))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Err(i18n::err("platform_unsupported"))
    }
}

/// 显示主窗口并聚焦（用于托盘点击、唤醒单实例等场景）。
fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// 构建托盘菜单（文案按当前语言；语言切换时重建即调用本函数）。
fn tray_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let show = MenuItem::with_id(app, "show", i18n::pick("显示窗口", "Show Window"), true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", i18n::pick("启动代理", "Start Proxy"), true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", i18n::pick("停止代理", "Stop Proxy"), true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", i18n::pick("重启代理", "Restart Proxy"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", i18n::pick("退出", "Quit"), true, None::<&str>)?;
    Menu::with_items(app, &[&show, &start, &stop, &restart, &quit])
}

/// 语言切换后重建托盘菜单，使菜单文案跟随界面语言（托盘在 setup 只创建一次）。
fn refresh_tray_menu(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id("main-tray") {
        match tray_menu(app) {
            Ok(menu) => {
                let _ = tray.set_menu(Some(menu));
            }
            Err(e) => proxy::log::warn(&format!("托盘菜单刷新失败: {e}")),
        }
    }
}

/// 构建系统托盘：提供显示窗口、启停/重启代理与退出菜单，左键单击托盘图标唤起主窗口。
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = tray_menu(app)?;

    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().expect("default window icon").clone())
        .tooltip("68proxy")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_window(app),
            "start" => {
                let h = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = start_proxy_inner(&h).await;
                });
            }
            "stop" => stop_proxy_inner(app),
            "restart" => {
                let h = app.clone();
                tauri::async_runtime::spawn(async move {
                    stop_proxy_inner(&h);
                    wait_port_released(&h).await;
                    let _ = start_proxy_inner(&h).await;
                });
            }
            "quit" => {
                stop_proxy_inner(app);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// 应用主入口：注册插件与全部 IPC 命令，在 setup 中加载配置、同步开机自启、
/// 接好日志/请求事件转发、构建托盘，并按配置决定启动时是否隐藏窗口与自动启动代理；
/// 关闭窗口时按 close_to_tray 配置选择隐藏到托盘或直接退出。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_window(app);
        }))
        .invoke_handler(tauri::generate_handler![
            proxy_start,
            proxy_stop,
            proxy_restart,
            proxy_status,
            config_get,
            config_save,
            local_key_get,
            local_key_set,
            local_key_generate,
            local_key_delete,
            account_list,
            account_add,
            account_rename,
            account_remove,
            auth_login_start,
            auth_login_poll,
            auth_login_cancel,
            models_get,
            models_catalog,
            models_catalog_update,
            plan_status,
            accounts_quota,
            account_quota,
            account_routing_get,
            account_routing_set,
            account_bindings_clear,
            theme_get,
            theme_set,
            language_get,
            language_set,
            logs_get,
            logs_clear,
            logs_export,
            requests_get,
            stats_get,
            stats_chart,
            stats_recent,
            stats_clear_all,
            port_check,
            port_free,
            autostart_get,
            autostart_set,
        ])
        .setup(|app| {
            let config_path = app.path().app_config_dir()?.join("config.json");
            let usage_path = app.path().app_config_dir()?.join("usage.sqlite");

            // 初始化设置库 + 流量统计库（同一 SQLite 文件），并注入代理状态
            let usage_conn = proxy::usage::init_usage(&usage_path)?;
            // 模型信息首次启动落库（空表用内置表播种）并载入运行时注册表；
            // 之后模型数据以数据库为准，内置表仅作兜底
            match proxy::models::init_and_load(&usage_conn) {
                Ok(models) => proxy::pricing::set_models(models),
                Err(e) => proxy::log::warn(&format!(
                    "{}: {e}",
                    i18n::pick(
                        "模型信息初始化失败，使用内置兜底表",
                        "Model info init failed, using the built-in fallback table"
                    )
                )),
            }
            // 首次启动：config.json 存在且 settings 表为空时迁移到 SQLite
            proxy::settings::migrate_from_config(&usage_conn, &config_path)?;
            // 旧版 api_key 行迁移到 cc_accounts 后清理，避免每次启动重复迁移
            let _ = proxy::settings::purge_legacy_api_key(&usage_conn);
            // 从 SQLite 加载配置（缺字段走默认）
            let mut cfg = proxy::settings::load_config(&usage_conn);
            // 后端日志语言跟随配置（此后产生的日志按该语言输出）
            i18n::set_lang(&cfg.language);
            // 首次落盘镜像时使用「未叠加环境变量」的副本：否则下次启动
            // migrate_from_config 会把 env 值当作普通设置导入 settings 表并永久生效
            if !config_path.exists() {
                let _ = cfg.save(&config_path);
            }
            // 再应用环境变量覆写（仅作用于本次运行的内存配置）
            cfg.apply_env();

            let proxy_state = proxy::state::AppState::new(cfg.clone());
            *proxy_state.usage.lock().unwrap() = Some(usage_conn);
            // 注入指纹持久化路径：同一 API Key 重启后复用同一设备指纹
            proxy_state
                .set_fingerprint_path(app.path().app_config_dir()?.join(proxy::fingerprint::STORE_FILE));

            // 开机自启跟随配置
            if cfg.autostart {
                let _ = app.autolaunch().enable();
            } else {
                let _ = app.autolaunch().disable();
            }

            // 日志 → 前端事件
            let h = app.handle().clone();
            proxy::log::set_sink(move |entry| {
                let _ = h.emit("proxy://log", entry);
                #[cfg(debug_assertions)]
                dev_bridge::publish(
                    "proxy://log",
                    serde_json::to_value(entry).unwrap_or(serde_json::Value::Null),
                );
            });
            // 请求摘要 → 前端事件
            let h2 = app.handle().clone();
            proxy::server::set_request_sink(move |info| {
                let _ = h2.emit("proxy://request", info);
                #[cfg(debug_assertions)]
                dev_bridge::publish(
                    "proxy://request",
                    serde_json::to_value(info).unwrap_or(serde_json::Value::Null),
                );
            });
            // 用量更新 → 前端事件（节流 200ms，避免高频请求刷爆事件流）
            let h3 = app.handle().clone();
            let last_emit = std::sync::Arc::new(std::sync::Mutex::new(0u64));
            let last_emit2 = last_emit.clone();
            proxy::usage::set_usage_sink(move || {
                let now = proxy::state::now_millis();
                let mut last = last_emit2.lock().unwrap();
                if now.saturating_sub(*last) >= 200 {
                    *last = now;
                    let payload = serde_json::json!({ "updated": now });
                    let _ = h3.emit("proxy://stats", payload.clone());
                    #[cfg(debug_assertions)]
                    dev_bridge::publish("proxy://stats", payload);
                }
            });

            // 预热本地转发 Key 内存缓存（账户列表随 AppState.config 走，无需预热）
            {
                let guard = proxy_state.usage.lock().unwrap();
                if let Some(conn) = guard.as_ref() {
                    let _ = credentials::load_local_key(conn);
                }
            }

            app.manage(AppCtx {
                config_path,
                usage_path,
                proxy_state,
            });

            // 启动开发调试桥接（仅 debug 构建），使浏览器直连前端页面也能调用后端命令
            #[cfg(debug_assertions)]
            dev_bridge::start(app.handle().clone());

            build_tray(app.handle())?;

            // 启动时隐藏窗口
            if !cfg.show_window_on_start {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }

            // 自动启动代理
            if cfg.auto_start_proxy {
                let h = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    let _ = start_proxy_inner(&h).await;
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // 开启「关闭到托盘」时拦截窗口关闭：仅隐藏窗口，代理继续后台运行
            if let WindowEvent::CloseRequested { api, .. } = event {
                let ctx = window.app_handle().state::<AppCtx>();
                let close_to_tray = ctx.proxy_state.config.read().unwrap().close_to_tray;
                if close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
