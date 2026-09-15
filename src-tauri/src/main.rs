//! 68Proxy 桌面应用的原生入口：仅负责调用库 crate 的 run() 启动 Tauri 应用。
//! 实际的应用装配、命令注册与代理生命周期逻辑都在 `proxy68_lib`（src/lib.rs）中。

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// 程序入口：启动 Tauri 应用主循环。
fn main() {
    proxy68_lib::run()
}
