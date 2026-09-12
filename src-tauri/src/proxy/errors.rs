use serde_json::{json, Value};

/// CC 上游 HTTP 状态 → 下游（OpenAI/Anthropic）状态与错误类型映射。
///
/// - `cc_status`：上游返回的 HTTP 状态码；
/// - `cc_body`：上游响应体文本，尝试从中提取可读的错误消息（JSON 的 error/message 或 message 字段，
///   非 JSON 时截取前 200 字符）；
/// - 返回：（映射后的下游状态码, 标准错误 JSON 体）。429（含上游 402 额度耗尽）会附带
///   `retry_after: 30` 提示客户端退避。
pub fn map_cc_error(cc_status: u16, cc_body: &str) -> (u16, Value) {
    let mapped = match cc_status {
        400 => (400, "invalid_request_error"),
        401 => (401, "authentication_error"),
        402 => (429, "rate_limit_error"),
        403 => (401, "authentication_error"),
        404 => (404, "not_found"),
        422 => (400, "invalid_request_error"),
        429 => (429, "rate_limit_error"),
        500 => (502, "upstream_error"),
        502 => (502, "upstream_error"),
        503 => (503, "temporarily_unavailable"),
        _ => (502, "upstream_error"),
    };

    let mut message = format!("Command Code API error ({cc_status})");
    if !cc_body.is_empty() {
        if let Ok(parsed) = serde_json::from_str::<Value>(cc_body) {
            if let Some(m) = parsed
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .or_else(|| parsed.get("message").and_then(|v| v.as_str()))
            {
                message = m.to_string();
            }
        } else {
            message = cc_body.chars().take(200).collect();
        }
    }

    if cc_status == 429 {
        return (
            429,
            json!({
                "error": { "message": message, "type": "rate_limit_error" },
                "retry_after": 30,
            }),
        );
    }

    (
        mapped.0,
        json!({ "error": { "message": message, "type": mapped.1 } }),
    )
}

/// 构造 OpenAI 风格错误响应（`{"error": {message, type}}`）。
///
/// - `retry_after`：可选的重试间隔秒数，存在时附加顶层 `retry_after` 字段。
pub fn openai_error(status: u16, err_type: &str, message: &str, retry_after: Option<u64>) -> (u16, Value) {
    let mut body = json!({ "error": { "message": message, "type": err_type } });
    if let Some(ra) = retry_after {
        body["retry_after"] = json!(ra);
    }
    (status, body)
}

/// 构造 Anthropic 风格错误响应（`{"type": "error", "error": {type, message}}`）。
///
/// - `retry_after`：可选的重试间隔秒数，存在时附加顶层 `retry_after` 字段。
pub fn anthropic_error(status: u16, err_type: &str, message: &str, retry_after: Option<u64>) -> (u16, Value) {
    let mut body = json!({
        "type": "error",
        "error": { "type": err_type, "message": message },
    });
    if let Some(ra) = retry_after {
        body["retry_after"] = json!(ra);
    }
    (status, body)
}
