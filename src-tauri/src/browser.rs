//! 浏览器打开的平台适配：为浏览器授权登录提供「隐私模式」打开能力。
//!
//! `tauri-plugin-opener` 的 `open_url` 只能指定应用名、无法透传浏览器隐私参数，
//! 故此处按平台直接调用浏览器可执行文件，优先选择支持隐私模式的浏览器：
//!
//! - macOS：Chrome `--incognito` / Edge `--inprivate`；两者均未安装时回退 Safari
//!   （Safari 无命令行隐私入口，以普通窗口打开并如实上报 `private: false`）。
//! - Windows：Edge `--inprivate` / Chrome `--incognito`；均未安装时回退默认浏览器。
//! - Linux：Chrome `--incognito` / Chromium / Firefox `--private-window`；
//!   均未安装时回退默认浏览器。
//!
//! 返回值 `OpenBrowserOutcome` 携带实际使用的浏览器与是否真正进入隐私模式，
//! 前端据此提示（例如 Safari 只能普通打开）。

use std::path::Path;
use std::process::Command;

use crate::i18n;

/// 打开结果：实际使用的浏览器标识与是否进入隐私模式。
#[derive(Debug, Clone, serde::Serialize)]
pub struct OpenBrowserOutcome {
    /// 浏览器标识：`chrome` / `edge` / `firefox` / `safari` / `default`。
    pub browser: &'static str,
    /// 是否真正以隐私模式打开（Safari 回退时为 false，前端据此提示）。
    pub private: bool,
}

/// 隐私模式打开浏览器授权页。
///
/// 返回实际浏览器与隐私模式状态；无可用的隐私模式浏览器时回退普通打开并如实上报。
pub fn open_auth_browser(url: &str, private: bool) -> Result<OpenBrowserOutcome, String> {
    if !private {
        // 非隐私模式：交给系统默认浏览器（沿用 opener 的打开语义）
        open_default(url)?;
        return Ok(OpenBrowserOutcome { browser: "default", private: false });
    }
    open_private(url)
}

/// 用系统默认浏览器打开（`open` / `cmd /C start` / `xdg-open`）。
fn open_default(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(url).spawn();
    result
        .map(|_| ())
        .map_err(|e| i18n::err_args("browser_open_failed", &[&e.to_string()]))
}

/// 尝试以隐私模式打开；无可用浏览器时回退系统默认并上报 `private: false`。
fn open_private(url: &str) -> Result<OpenBrowserOutcome, String> {
    // 各平台候选浏览器：`(应用, 隐私参数, 浏览器标识)`。
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str], &str)] = &[
        ("Google Chrome", &["--incognito"], "chrome"),
        ("Microsoft Edge", &["--inprivate"], "edge"),
    ];
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str], &str)] = &[
        ("msedge", &["--inprivate"], "edge"),
        ("chrome", &["--incognito"], "chrome"),
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: &[(&str, &[&str], &str)] = &[
        ("google-chrome", &["--incognito"], "chrome"),
        ("chromium", &["--incognito"], "chrome"),
        ("firefox", &["--private-window"], "firefox"),
    ];

    for (app, args, id) in candidates {
        if !app_available(app) {
            continue;
        }
        let spawned = spawn_private(app, args, url);
        if spawned.is_ok() {
            return Ok(OpenBrowserOutcome { browser: id, private: true });
        }
    }

    // 无隐私模式浏览器：回退系统默认，如实上报未进入隐私模式
    open_default(url)?;
    Ok(OpenBrowserOutcome { browser: "default", private: false })
}

/// 以隐私参数启动浏览器。
///
/// macOS 经 `open -na <应用> --args <隐私参数> <url>` 唤起（`-n` 新开实例、
/// `-a` 按应用名解析）；其余平台直接调用可执行文件并追加参数。
fn spawn_private(app: &str, args: &[&str], url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        // `--args` 分隔符必须保留：其后的参数才会传给应用。缺了它 `open` 会把
        // `--incognito` 当作自身选项解析并报错退出，浏览器实际没有打开。
        cmd.arg("-na").arg(app).arg("--args").args(args).arg(url);
        cmd.spawn().map(|_| ())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut cmd = Command::new(app);
        cmd.args(args).arg(url);
        cmd.spawn().map(|_| ())
    }
}

/// 浏览器应用是否可用。
///
/// - macOS：检查 `/Applications` 下的 .app 是否存在（`open -a` 找不到时会弹
///   系统提示框，故先探测，避免打扰用户）。
/// - 其它平台：检查可执行文件是否在 PATH 中。
fn app_available(app: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        Path::new(&format!("/Applications/{app}.app")).exists()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Command::new("sh")
            .args(["-c", &format!("command -v {app}")])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不存在的浏览器应探测为不可用（不依赖具体平台是否装了浏览器）。
    #[test]
    fn non_existent_app_unavailable() {
        assert!(!app_available("NoSuchBrowser_68proxy"));
    }
}
