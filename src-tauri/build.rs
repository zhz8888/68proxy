//! Tauri 构建脚本：在编译期执行 tauri_build 的默认流程（生成图标资源、权限清单与平台相关配置）。

/// 构建入口：委托 tauri-build 完成 Tauri 应用所需的代码与资源生成。
fn main() {
    tauri_build::build()
}
