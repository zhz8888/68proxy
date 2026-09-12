//! 后端国际化：错误码生成与日志文案的语言选择。
//!
//! 前端界面文案由前端的语言包负责；后端只做两件事：
//! 1. 把「会给用户看的错误/提示」编码为**带前缀的消息码**而不是自然语言，
//!    由前端按当前语言翻译（码表见 src/i18n/locales/*.json 的 `errors` / `messages` 节）：
//!    - `err:<code>` / `err:<code>:["参数…"]` —— 错误（toast.error / 错误条）
//!    - `msg:<code>` / `msg:<code>:["参数…"]` —— 成功类提示（toast.success）
//!    参数用 JSON 字符串数组编码，因此参数内含 `:` 或引号也不会破坏解析。
//! 2. 日志文案按当前界面语言本地化：日志会写入日志缓冲、日志文件与导出文件，
//!    都是给人读的文本，不适合用码表示，故在此内置中英短语，用 `pick` 选择。
//!
//! 语言状态用进程级原子量保存（而非从 AppHandle 取配置），因为日志可能由
//! 没有 AppHandle 的后台线程（代理转发、定时清理）产生。

use std::sync::atomic::{AtomicU8, Ordering};

/// 当前界面语言：0 = 简体中文（默认），1 = 英文。
static LANG: AtomicU8 = AtomicU8::new(0);

/// 设置当前语言（`zh` / `en`，其它值按中文处理）。
pub fn set_lang(lang: &str) {
    LANG.store(if lang == "en" { 1 } else { 0 }, Ordering::Relaxed);
}

/// 当前是否为英文界面。
pub fn is_en() -> bool {
    LANG.load(Ordering::Relaxed) == 1
}

/// 按当前语言在中文/英文短语间选择（日志等给人读的文本用）。
pub fn pick<'a>(zh: &'a str, en: &'a str) -> &'a str {
    if is_en() {
        en
    } else {
        zh
    }
}

/// 把参数数组编码为 JSON 字符串数组（如 `["8080","已占用"]`）。
fn encode_args(args: &[&str]) -> String {
    serde_json::to_string(args).unwrap_or_else(|_| "[]".into())
}

/// 生成错误消息码：`err:<code>`。
pub fn err(code: &str) -> String {
    format!("err:{code}")
}

/// 生成带参数的错误消息码：`err:<code>:["参数…"]`（前端按 p0/p1… 插值）。
pub fn err_args(code: &str, args: &[&str]) -> String {
    format!("err:{code}:{}", encode_args(args))
}

/// 生成成功类消息码：`msg:<code>`。
pub fn msg(code: &str) -> String {
    format!("msg:{code}")
}

/// 生成带参数的成功类消息码：`msg:<code>:["参数…"]`。
pub fn msg_args(code: &str, args: &[&str]) -> String {
    format!("msg:{code}:{}", encode_args(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_roundtrip_shape() {
        assert_eq!(err("settings_store_uninitialized"), "err:settings_store_uninitialized");
        assert_eq!(err_args("port_listen_failed", &["8080", "busy"]), "err:port_listen_failed:[\"8080\",\"busy\"]");
        assert_eq!(msg("port_not_in_use"), "msg:port_not_in_use");
        assert_eq!(msg_args("port_freed", &["1234"]), "msg:port_freed:[\"1234\"]");
    }

    #[test]
    fn args_with_colon_are_safe() {
        // 参数内含冒号/引号时不应破坏码的分隔（值整体在 JSON 数组中）
        assert_eq!(
            err_args("proxy_start_failed", &["a:b", "he said \"hi\""]),
            "err:proxy_start_failed:[\"a:b\",\"he said \\\"hi\\\"\"]"
        );
    }

    #[test]
    fn lang_switch_and_pick() {
        set_lang("en");
        assert!(is_en());
        assert_eq!(pick("中文", "English"), "English");
        set_lang("zh");
        assert!(!is_en());
        assert_eq!(pick("中文", "English"), "中文");
        // 非法值按中文处理
        set_lang("fr");
        assert!(!is_en());
    }
}
