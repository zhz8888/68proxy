//! Tauri 应用核心：注册前端可调用的 IPC 命令（代理启停、配置、凭据、日志、
//! 模型列表、端口检查、开机自启等），并在 setup 阶段完成配置加载、托盘构建、
//! 日志/请求事件转发与窗口生命周期钩子的装配。

mod credentials;
mod proxy;

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

/// 重启本地代理：先停止，等待 500ms 释放端口后再启动，返回新的代理状态 JSON。
#[tauri::command]
async fn proxy_restart(app: AppHandle) -> Result<Value, String> {
    stop_proxy_inner(&app);
    tokio::time::sleep(Duration::from_millis(500)).await;
    start_proxy_inner(&app).await
}

/// 查询当前代理状态（是否运行、监听地址、接入 URL、运行时长等）的 JSON。
#[tauri::command]
fn proxy_status(app: AppHandle) -> Value {
    status_value(&app)
}

/// 读取应用配置。出于安全考虑返回前会清空 api_key 字段（Key 由专用命令管理）。
#[tauri::command]
fn config_get(app: AppHandle) -> proxy::config::Config {
    let ctx = app.state::<AppCtx>();
    let mut cfg = ctx.proxy_state.config.read().unwrap().clone();
    cfg.api_key = String::new();
    cfg
}

/// 校验并保存配置：写入 SQLite settings 表（主）+ config.json（镜像兜底），
/// 并同步到内存中的代理状态。
/// 返回 `needs_restart`：端口/主机变更且代理正在运行时需要重启才能生效。
#[tauri::command]
fn config_save(app: AppHandle, mut config: proxy::config::Config) -> Result<Value, String> {
    config.validate()?;
    let ctx = app.state::<AppCtx>();
    // API Key 由 api_key_set / api_key_delete 管理，config_save 不接收该字段，保存前保留原值
    let stored = ctx.proxy_state.config.read().unwrap().clone();
    config.api_key = stored.api_key;
    // 主存 SQLite settings 表
    {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| "设置库未初始化".to_string())?;
        proxy::settings::save_config(conn, &config)?;
    }
    // 镜像到 config.json（兜底），失败仅记 warn 不阻断保存
    if let Err(e) = config.save(&ctx.config_path) {
        proxy::log::warn(&format!("config.json 镜像写入失败: {e}"));
    }
    let prev = ctx.proxy_state.config.read().unwrap().clone();
    let needs_restart =
        (prev.port != config.port || prev.host != config.host) && ctx.proxy_state.is_running();
    *ctx.proxy_state.config.write().unwrap() = config.clone();
    proxy::log::info("配置已保存");
    Ok(json!({ "needs_restart": needs_restart }))
}

/// 查询 API Key 存储状态：返回是否已保存（has_key）与掩码后的展示文本（masked）。
#[tauri::command]
fn api_key_get(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let key = {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| "设置库未初始化".to_string())?;
        credentials::load_api_key(conn)?
    };
    Ok(json!({
        "has_key": key.is_some(),
        "masked": key.as_deref().map(credentials::mask_key).unwrap_or_default(),
    }))
}

/// 保存上游 API Key 到设置库并刷新内存缓存。入参为完整的 Key 字符串。
#[tauri::command]
fn api_key_set(app: AppHandle, key: String) -> Result<(), String> {
    let ctx = app.state::<AppCtx>();
    let guard = ctx.proxy_state.usage.lock().unwrap();
    let conn = guard.as_ref().ok_or_else(|| "设置库未初始化".to_string())?;
    credentials::save_api_key(conn, &key)
}

/// 删除已保存的 API Key（清空设置库字段与内存缓存）。
#[tauri::command]
fn api_key_delete(app: AppHandle) -> Result<(), String> {
    let ctx = app.state::<AppCtx>();
    let guard = ctx.proxy_state.usage.lock().unwrap();
    let conn = guard.as_ref().ok_or_else(|| "设置库未初始化".to_string())?;
    credentials::delete_api_key(conn)
}

/// 获取可用模型列表。`force` 为 true 时先清空缓存再向上游拉取；
/// 返回 `{ data: 模型列表, fallback: 是否使用兜底列表 }`。
#[tauri::command]
async fn models_get(app: AppHandle, force: bool) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    if force {
        *ctx.proxy_state.models.write().unwrap() = proxy::state::ModelsCache {
            models: Vec::new(),
            fetched_at: 0,
        };
    }
    let (list, fallback) =
        proxy::cc_client::fetch_models(&ctx.proxy_state, credentials::cached_key().as_deref())
            .await;
    Ok(json!({ "data": list, "fallback": fallback }))
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
    std::fs::write(&path, out).map_err(|e| format!("日志写入失败: {e}"))?;
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
    Ok(serde_json::to_value(chart).map_err(|e| format!("序列化趋势数据失败: {e}"))?)
}

/// 获取最近 `limit` 条用量明细（默认 20，新在前）。
#[tauri::command]
fn stats_recent(app: AppHandle, limit: Option<usize>) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let conn = proxy::usage::open_usage(&ctx.usage_path)?;
    let recent = proxy::usage::query_recent(&conn, limit.unwrap_or(20))?;
    Ok(serde_json::to_value(recent).map_err(|e| format!("序列化最近用量失败: {e}"))?)
}

/// 清空全部 token 用量统计，返回被清除的记录条数。
#[tauri::command]
fn stats_clear_all(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let conn = proxy::usage::open_usage(&ctx.usage_path)?;
    let count = proxy::usage::clear_all(&conn)?;
    proxy::log::info(&format!("用量统计已清空（{count} 条）"));
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
        return Ok(json!({ "killed": [], "message": "端口未被占用，无需释放" }));
    }
    let mut killed: Vec<u32> = Vec::new();
    for pid in &pids {
        kill_pid(*pid)?;
        killed.push(*pid);
    }
    proxy::log::info(&format!("端口 {port} 已释放，结束进程：{killed:?}"));
    Ok(json!({ "killed": killed, "message": format!("已结束占用进程（PID {}）", killed.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")) }))
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
    let _ = app.emit("proxy://status", value);
}

/// 启动代理的内部实现：校验配置与 API Key、绑定监听端口、在后台任务中运行服务，
/// 并等待运行标志置位后广播状态事件，返回最终代理状态 JSON。已运行时直接返回当前状态。
async fn start_proxy_inner(app: &AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    if ctx.proxy_state.is_running() {
        return Ok(status_value(app));
    }
    // 需要已保存的 API Key 才能启动代理
    let stored_key = {
        let guard = ctx.proxy_state.usage.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| "设置库未初始化".to_string())?;
        credentials::load_api_key(conn).map_err(|e| format!("读取 API Key 失败: {e}"))?
    };
    if stored_key.is_none() {
        return Err("未保存 API Key：请先在「配置 → 凭据」保存 user_ 开头的 Key 后再启动代理".into());
    }
    let cfg = ctx.proxy_state.config.read().unwrap().clone();
    cfg.validate()?;
    *ctx.proxy_state.config.write().unwrap() = cfg.clone();

    let addr = proxy::server::listen_addr(&cfg);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("端口 {} 监听失败，可能已被占用（请先关闭占用程序）: {e}", addr.port()))?;

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
    proxy::log::info(&format!("代理已启动：{}:{}", cfg.host, cfg.port));
    emit_status(app);
    Ok(status_value(app))
}

/// 停止代理的内部实现：发送关停信号、清理请求记录并广播状态；未运行时直接返回。
fn stop_proxy_inner(app: &AppHandle) {
    let ctx = app.state::<AppCtx>();
    if !ctx.proxy_state.is_running() {
        return;
    }
    ctx.proxy_state.mark_stopped();
    ctx.proxy_state.clear_requests();
    proxy::log::info("代理已停止");
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
                if line.contains(&format!(":{port}")) && line.to_uppercase().contains("LISTENING") {
                    if let Some(pid) = line.split_whitespace().last() {
                        if let Ok(p) = pid.parse::<u32>() {
                            if !pids.contains(&p) {
                                pids.push(p);
                            }
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
            .map_err(|e| format!("结束进程失败: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(format!("结束进程失败（PID {pid}）：{msg}"))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Err("当前平台暂不支持结束进程".into())
    }
}

/// 显示主窗口并聚焦（用于托盘点击、唤醒单实例等场景）。
fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// 构建系统托盘：提供显示窗口、启停/重启代理与退出菜单，左键单击托盘图标唤起主窗口。
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", "启动代理", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "停止代理", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "重启代理", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &start, &stop, &restart, &quit])?;

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
                    tokio::time::sleep(Duration::from_millis(500)).await;
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
            api_key_get,
            api_key_set,
            api_key_delete,
            models_get,
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
            // 首次启动：config.json 存在且 settings 表为空时迁移到 SQLite
            proxy::settings::migrate_from_config(&usage_conn, &config_path)?;
            // 从 SQLite 加载配置（缺字段走默认），再应用环境变量覆写
            let mut cfg = proxy::settings::load_config(&usage_conn);
            cfg.apply_env();
            // config.json 无内容时首次落盘默认值（镜像兜底）
            if !config_path.exists() {
                let _ = cfg.save(&config_path);
            }

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
            });
            // 请求摘要 → 前端事件
            let h2 = app.handle().clone();
            proxy::server::set_request_sink(move |info| {
                let _ = h2.emit("proxy://request", info);
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
                    let _ = h3.emit("proxy://stats", serde_json::json!({ "updated": now }));
                }
            });

            // 预热 API Key 内存缓存（设置库）
            {
                let guard = proxy_state.usage.lock().unwrap();
                if let Some(conn) = guard.as_ref() {
                    let _ = credentials::load_api_key(conn);
                }
            }

            app.manage(AppCtx {
                config_path,
                usage_path,
                proxy_state,
            });

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
