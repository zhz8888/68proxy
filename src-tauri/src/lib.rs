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
    proxy_state: Arc<proxy::state::AppState>,
}

#[tauri::command]
async fn proxy_start(app: AppHandle) -> Result<Value, String> {
    start_proxy_inner(&app).await
}

#[tauri::command]
async fn proxy_stop(app: AppHandle) -> Result<Value, String> {
    stop_proxy_inner(&app);
    Ok(status_value(&app))
}

#[tauri::command]
async fn proxy_restart(app: AppHandle) -> Result<Value, String> {
    stop_proxy_inner(&app);
    tokio::time::sleep(Duration::from_millis(500)).await;
    start_proxy_inner(&app).await
}

#[tauri::command]
fn proxy_status(app: AppHandle) -> Value {
    status_value(&app)
}

#[tauri::command]
fn config_get(app: AppHandle) -> proxy::config::Config {
    let ctx = app.state::<AppCtx>();
    let mut cfg = proxy::config::Config::load(&ctx.config_path);
    cfg.api_key = String::new();
    cfg
}

#[tauri::command]
fn config_save(app: AppHandle, mut config: proxy::config::Config) -> Result<Value, String> {
    config.validate()?;
    let ctx = app.state::<AppCtx>();
    // API Key 由 api_key_set / api_key_delete 管理，config_save 不接收该字段，保存前保留原值
    let stored = proxy::config::Config::load(&ctx.config_path);
    config.api_key = stored.api_key;
    config.save(&ctx.config_path)?;
    let prev = ctx.proxy_state.config.read().unwrap().clone();
    let needs_restart =
        (prev.port != config.port || prev.host != config.host) && ctx.proxy_state.is_running();
    *ctx.proxy_state.config.write().unwrap() = config.clone();
    proxy::log::info("配置已保存");
    Ok(json!({ "needs_restart": needs_restart }))
}

#[tauri::command]
fn api_key_get(app: AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    let key = credentials::load_api_key(&ctx.config_path)?;
    Ok(json!({
        "has_key": key.is_some(),
        "masked": key.as_deref().map(credentials::mask_key).unwrap_or_default(),
    }))
}

#[tauri::command]
fn api_key_set(app: AppHandle, key: String) -> Result<(), String> {
    let ctx = app.state::<AppCtx>();
    credentials::save_api_key(&ctx.config_path, &key)
}

#[tauri::command]
fn api_key_delete(app: AppHandle) -> Result<(), String> {
    let ctx = app.state::<AppCtx>();
    credentials::delete_api_key(&ctx.config_path)
}

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

#[tauri::command]
fn logs_get(limit: Option<usize>, after_seq: Option<u64>) -> Value {
    json!(proxy::log::get_logs(limit.unwrap_or(200), after_seq.unwrap_or(0)))
}

#[tauri::command]
fn logs_clear() {
    proxy::log::clear_logs();
}

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

#[tauri::command]
fn requests_get(app: AppHandle, limit: Option<usize>) -> Value {
    let ctx = app.state::<AppCtx>();
    json!(ctx.proxy_state.recent_requests(limit.unwrap_or(20)))
}

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

#[tauri::command]
fn autostart_get(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

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

fn emit_status(app: &AppHandle) {
    let value = status_value(app);
    let _ = app.emit("proxy://status", value);
}

async fn start_proxy_inner(app: &AppHandle) -> Result<Value, String> {
    let ctx = app.state::<AppCtx>();
    if ctx.proxy_state.is_running() {
        return Ok(status_value(app));
    }
    // 需要已保存的 API Key 才能启动代理
    let stored_key = credentials::load_api_key(&ctx.config_path).map_err(|e| format!("读取 API Key 失败: {e}"))?;
    if stored_key.is_none() {
        return Err("未保存 API Key：请先在「配置 → 凭据」保存 user_ 开头的 Key 后再启动代理".into());
    }
    let cfg = proxy::config::Config::load(&ctx.config_path);
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

fn find_pid_for_port(port: u16) -> Option<u32> {
    find_pids_for_port(port).first().copied()
}

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

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

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
            port_check,
            port_free,
            autostart_get,
            autostart_set,
        ])
        .setup(|app| {
            let config_path = app.path().app_config_dir()?.join("config.json");
            let cfg = proxy::config::Config::load(&config_path);
            if !config_path.exists() {
                let _ = cfg.save(&config_path);
            }

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

            let _ = credentials::load_api_key(&config_path);

            app.manage(AppCtx {
                config_path,
                proxy_state: proxy::state::AppState::new(cfg.clone()),
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
