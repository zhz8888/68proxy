//! macOS 原生窗口适配：无边框窗口的四角圆角处理。
//!
//! 应用窗口是 `decorations: false` 的自绘无边框窗口，macOS 默认不会给这种
//! 窗口加圆角。这里直接操作 AppKit：拿到窗口的 contentView，开启 layer
//! 并把 layer 的 `cornerRadius` 设为圆角半径、`masksToBounds` 裁切边界，
//! 使窗口四角呈现圆角。仅 macOS 编译（见 lib.rs 的 `#[cfg]` 引用）。

use objc2::rc::Retained;
use objc2_app_kit::{NSColor, NSWindow};
use objc2_quartz_core::CALayer;

/// 窗口圆角半径（逻辑像素）。与常见 macOS 无边框窗口的观感一致。
const CORNER_RADIUS: f64 = 10.0;

/// 把 Tauri 窗口的四个角设为圆角。
///
/// `ns_window` 是 Tauri 在 macOS 下返回的 `NSWindow` 实例指针（`*mut c_void`）。
/// 无边框窗口默认背景不透明（白色），圆角裁切后角落会被白底填满，看不到圆角；
/// 故先把窗口与内容层背景设为透明，再开 layer 设圆角 + 裁切，角落露出桌面。
pub fn apply_rounded_corners(ns_window: *mut std::ffi::c_void) {
    // 指针为 null（窗口不可用）时直接跳过，不影响应用启动
    if ns_window.is_null() {
        return;
    }
    // SAFETY: ns_window 是 Tauri 提供的有效 NSWindow 指针；retain 增加引用计数，
    // 离开作用域时自动释放，不改变窗口生命周期。
    let Some(window) = (unsafe { Retained::retain(ns_window.cast::<NSWindow>()) }) else {
        return;
    };
    // 窗口背景透明：让圆角外的角落露出桌面，而不是白底
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    // contentView 缺失（窗口尚未完成布局）时跳过
    let Some(content_view) = window.contentView() else {
        return;
    };
    // 开启 layer：不开启时 contentView.layer() 恒为 None
    content_view.setWantsLayer(true);
    let Some(layer) = content_view.layer() else {
        return;
    };
    // 内容层背景也透明（默认 layer 背景不透明，会盖住窗口的透明背景）
    layer.setBackgroundColor(None);
    set_rounded(&layer);
}

/// 给 CALayer 设置圆角与边界裁切。
fn set_rounded(layer: &CALayer) {
    layer.setCornerRadius(CORNER_RADIUS);
    layer.setMasksToBounds(true);
}
