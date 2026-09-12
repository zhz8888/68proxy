use serde_json::{json, Value};

/// Command Code 上游 HTTP 状态 → 下游（OpenAI/Anthropic）状态与错误类型映射。
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 全量状态映射表：每个上游状态都映射到预期的下游状态与错误类型。
    #[test]
    fn status_mapping_table() {
        for (cc, downstream, ty) in [
            (400u16, 400u16, "invalid_request_error"),
            (401, 401, "authentication_error"),
            (402, 429, "rate_limit_error"),
            (403, 401, "authentication_error"),
            (404, 404, "not_found"),
            (422, 400, "invalid_request_error"),
            (429, 429, "rate_limit_error"),
            (500, 502, "upstream_error"),
            (502, 502, "upstream_error"),
            (503, 503, "temporarily_unavailable"),
            (418, 502, "upstream_error"), // 未知状态走兜底
        ] {
            let (s, body) = map_cc_error(cc, "");
            assert_eq!(s, downstream, "上游 {cc} 的状态映射");
            assert_eq!(body["error"]["type"], ty, "上游 {cc} 的错误类型");
        }
    }

    /// 错误消息提取优先级：JSON error.message → 顶层 message → 非 JSON 截断 → 前缀兜底。
    #[test]
    fn message_extraction_priority() {
        let (_, b) = map_cc_error(500, r#"{"error":{"message":"inner msg"}}"#);
        assert_eq!(b["error"]["message"], "inner msg");
        let (_, b) = map_cc_error(500, r#"{"message":"top msg"}"#);
        assert_eq!(b["error"]["message"], "top msg");
        // 非 JSON：保留原文（截取前 200 字符）
        let (_, b) = map_cc_error(500, "plain upstream oops");
        assert_eq!(b["error"]["message"], "plain upstream oops");
        let long = "x".repeat(300);
        let (_, b) = map_cc_error(500, &long);
        assert_eq!(b["error"]["message"].as_str().unwrap().len(), 200);
        // 空体：使用带状态码的前缀兜底
        let (_, b) = map_cc_error(500, "");
        assert_eq!(b["error"]["message"], "Command Code API error (500)");
    }

    /// 429（含上游 402 映射）附带 retry_after；OpenAI/Anthropic 错误体形态正确。
    #[test]
    fn error_shape_and_retry_after() {
        let (s, b) = map_cc_error(429, "");
        assert_eq!(s, 429);
        assert_eq!(b["retry_after"], json!(30));

        let (s, b) = openai_error(502, "upstream_error", "boom", Some(7));
        assert_eq!(s, 502);
        assert_eq!(b["error"]["message"], "boom");
        assert_eq!(b["retry_after"], json!(7));
        let (_, b) = openai_error(500, "upstream_error", "boom", None);
        assert!(b.get("retry_after").is_none());

        let (_, b) = anthropic_error(502, "upstream_error", "boom", Some(9));
        assert_eq!(b["type"], "error");
        assert_eq!(b["error"]["type"], "upstream_error");
        assert_eq!(b["error"]["message"], "boom");
        assert_eq!(b["retry_after"], json!(9));
        let (_, b) = anthropic_error(500, "upstream_error", "boom", None);
        assert!(b.get("retry_after").is_none());
    }
}
