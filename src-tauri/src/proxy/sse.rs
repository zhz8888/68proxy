use serde_json::Value;

use super::convert::{map_anthropic_stop_reason, map_finish_reason};
use super::log;
use super::state::now_secs;

fn make_chunk(id: &str, created: u64, model: &str, delta: Value, finish_reason: Option<&str>, usage: Option<Value>) -> String {
    let mut chunk = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
    });
    if let Some(u) = usage {
        chunk["usage"] = u;
    }
    format!("data: {chunk}\n\n")
}

fn normalize_usage(input: &mut u64, output: &mut u64, cached: &mut u64) {
    if *output == 0 {
        *input = 0;
        *cached = 0;
    }
}

/// CC NDJSON → OpenAI SSE 翻译器。
pub struct OpenAiTranslator {
    completion_id: String,
    created: u64,
    model: String,
    chunk_index: u32,
    tool_call_index: u32,
    finish_reason: Option<String>,
    pub last_cc_event: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
}

impl OpenAiTranslator {
    pub fn new(model: &str, completion_id: &str) -> Self {
        Self {
            completion_id: completion_id.to_string(),
            created: now_secs(),
            model: model.to_string(),
            chunk_index: 0,
            tool_call_index: 0,
            finish_reason: None,
            last_cc_event: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
        }
    }

    pub fn parse_line(&mut self, line: &str) -> Vec<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" || trimmed.starts_with(':') {
            return Vec::new();
        }
        let event: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        let event_type = match event.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => return Vec::new(),
        };
        self.last_cc_event = event_type.to_string();
        let mut out: Vec<String> = Vec::new();

        match event_type {
            "text-start" | "reasoning-start" | "start" | "start-step" => {}
            "text-delta" => {
                let text = event
                    .get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| event.get("delta").and_then(|d| d.as_str()))
                    .unwrap_or("");
                if text.is_empty() {
                    return out;
                }
                let delta = if self.chunk_index == 0 {
                    serde_json::json!({ "role": "assistant", "content": text })
                } else {
                    serde_json::json!({ "content": text })
                };
                self.chunk_index += 1;
                out.push(make_chunk(&self.completion_id, self.created, &self.model, delta, None, None));
            }
            "reasoning-delta" => {
                let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.is_empty() {
                    return out;
                }
                let delta = if self.chunk_index == 0 {
                    serde_json::json!({ "role": "assistant", "reasoning_content": text })
                } else {
                    serde_json::json!({ "reasoning_content": text })
                };
                self.chunk_index += 1;
                out.push(make_chunk(&self.completion_id, self.created, &self.model, delta, None, None));
            }
            "tool-call" => {
                let id = event
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("call_{}_{}", self.tool_call_index, now_secs()));
                let name = event.get("toolName").and_then(|v| v.as_str()).unwrap_or("");
                let args = match event.get("input") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => "{}".to_string(),
                };
                let tc_entry = serde_json::json!({
                    "index": self.tool_call_index,
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": args },
                });
                let delta = if self.chunk_index == 0 {
                    serde_json::json!({ "role": "assistant", "content": Value::Null, "tool_calls": [tc_entry] })
                } else {
                    serde_json::json!({ "tool_calls": [tc_entry] })
                };
                self.chunk_index += 1;
                self.tool_call_index += 1;
                out.push(make_chunk(&self.completion_id, self.created, &self.model, delta, None, None));
            }
            "finish-step" => {
                if let Some(fr) = event.get("finishReason").and_then(|v| v.as_str()) {
                    self.finish_reason = Some(map_finish_reason(fr));
                }
                if let Some(u) = event.get("usage") {
                    self.input_tokens = u.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    self.output_tokens = u.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    self.cached_tokens = u.get("cachedInputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                }
            }
            "finish" => {
                let fr = self
                    .finish_reason
                    .clone()
                    .unwrap_or_else(|| map_finish_reason(event.get("finishReason").and_then(|v| v.as_str()).unwrap_or("stop")));
                let mut u = event
                    .get("totalUsage")
                    .cloned()
                    .or_else(|| event.get("usage").cloned())
                    .unwrap_or_else(|| serde_json::json!({}));
                let mut input = u.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let mut output = u.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let mut cached = u.get("cachedInputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                normalize_usage(&mut input, &mut output, &mut cached);
                u["inputTokens"] = serde_json::json!(input);
                u["outputTokens"] = serde_json::json!(output);
                u["cachedInputTokens"] = serde_json::json!(cached);
                self.input_tokens = input;
                self.output_tokens = output;
                self.cached_tokens = cached;
                let usage = serde_json::json!({
                    "prompt_tokens": input,
                    "completion_tokens": output,
                    "total_tokens": input + output,
                    "prompt_tokens_details": { "cached_tokens": cached },
                });
                out.push(make_chunk(&self.completion_id, self.created, &self.model, serde_json::json!({}), Some(&fr), Some(usage)));
            }
            "error" => {
                let msg = event
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .or_else(|| event.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("Unknown error");
                log::warn(&format!("CC stream error: {msg}"));
            }
            "reasoning-end" | "provider-metadata" | "tool-input-start" | "tool-input-delta" | "tool-input-end" | "tool-error" | "text-end" => {}
            other => {
                log::warn(&format!("Unknown CC event type: {other}"));
            }
        }
        out
    }

    pub fn done_event(&self) -> String {
        "data: [DONE]\n\n".to_string()
    }

    pub fn zero_output_error_frame(&self) -> String {
        let body = serde_json::json!({
            "error": { "message": "Empty response from upstream (zero output tokens)", "type": "rate_limit_error" },
            "retry_after": 10,
        });
        format!("data: {body}\n\n")
    }
}

fn sse_event(name: &str, payload: Value) -> String {
    format!("event: {name}\ndata: {payload}\n\n")
}

/// CC NDJSON → OpenAI Responses SSE 事件翻译器。
/// 事件序列：response.created → output_item.added/content_part.added
/// → output_text.delta…/function_call_arguments.delta → 各 done → response.completed。
pub struct ResponsesTranslator {
    response_id: String,
    created_at: u64,
    model: String,
    next_output_index: u32,
    text_open: bool,
    text_item_id: String,
    text_item_index: u32,
    item_text: String,
    output_items: Vec<Value>,
    stop_reason: Option<String>,
    pub last_cc_event: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub has_error: bool,
}

impl ResponsesTranslator {
    pub fn new(model: &str, response_id: &str) -> Self {
        Self {
            response_id: response_id.to_string(),
            created_at: now_secs(),
            model: model.to_string(),
            next_output_index: 0,
            text_open: false,
            text_item_id: String::new(),
            text_item_index: 0,
            item_text: String::new(),
            output_items: Vec::new(),
            stop_reason: None,
            last_cc_event: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            has_error: false,
        }
    }

    fn skeleton(&self, status: &str, output: Vec<Value>) -> Value {
        serde_json::json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "error": Value::Null,
            "model": self.model,
            "output": output,
        })
    }

    pub fn response_start(&self) -> String {
        sse_event(
            "response.created",
            serde_json::json!({ "type": "response.created", "response": self.skeleton("in_progress", vec![]) }),
        )
    }

    fn usage_value(&self) -> Value {
        serde_json::json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "total_tokens": self.input_tokens + self.output_tokens,
            "input_tokens_details": { "cached_tokens": self.cached_tokens },
            "output_tokens_details": { "reasoning_tokens": 0 },
        })
    }

    /// 关闭当前未完结的 message 条目，返回对应事件与（可选）完整条目。
    fn close_text_item(&mut self, out: &mut Vec<String>) {
        if !self.text_open {
            return;
        }
        self.text_open = false;
        let item_id = self.text_item_id.clone();
        let idx = self.text_item_index;
        let text = std::mem::take(&mut self.item_text);
        out.push(sse_event(
            "response.output_text.done",
            serde_json::json!({ "type": "response.output_text.done", "item_id": item_id, "output_index": idx, "content_index": 0, "text": text }),
        ));
        out.push(sse_event(
            "response.content_part.done",
            serde_json::json!({ "type": "response.content_part.done", "item_id": item_id, "output_index": idx, "content_index": 0, "part": { "type": "output_text", "text": text, "annotations": [] } }),
        ));
        let item = serde_json::json!({
            "type": "message", "id": item_id, "role": "assistant", "status": "completed",
            "content": [{ "type": "output_text", "text": text, "annotations": [] }],
        });
        out.push(sse_event(
            "response.output_item.done",
            serde_json::json!({ "type": "response.output_item.done", "output_index": idx, "item": item }),
        ));
        self.output_items.push(item);
    }

    pub fn process_line(&mut self, line: &str) -> Vec<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" || trimmed.starts_with(':') {
            return Vec::new();
        }
        let event: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        let event_type = match event.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => return Vec::new(),
        };
        self.last_cc_event = event_type.to_string();
        let mut out: Vec<String> = Vec::new();

        match event_type {
            "start" | "start-step" | "text-start" | "reasoning-start" | "reasoning-delta"
            | "reasoning-end" | "provider-metadata" | "tool-input-start" | "tool-input-delta"
            | "tool-input-end" | "tool-error" | "text-end" => {}
            "text-delta" => {
                let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.is_empty() {
                    return out;
                }
                if !self.text_open {
                    let idx = self.next_output_index;
                    self.next_output_index += 1;
                    self.text_item_index = idx;
                    self.text_item_id = format!("msg_{}", &uuid::Uuid::new_v4().to_string()[..12]);
                    self.text_open = true;
                    let item_id = self.text_item_id.clone();
                    out.push(sse_event(
                        "response.output_item.added",
                        serde_json::json!({
                            "type": "response.output_item.added", "output_index": idx,
                            "item": { "type": "message", "id": item_id, "role": "assistant", "status": "in_progress", "content": [] },
                        }),
                    ));
                    out.push(sse_event(
                        "response.content_part.added",
                        serde_json::json!({
                            "type": "response.content_part.added", "item_id": item_id, "output_index": idx, "content_index": 0,
                            "part": { "type": "output_text", "text": "", "annotations": [] },
                        }),
                    ));
                }
                self.item_text.push_str(text);
                out.push(sse_event(
                    "response.output_text.delta",
                    serde_json::json!({
                        "type": "response.output_text.delta", "item_id": self.text_item_id,
                        "output_index": self.text_item_index, "content_index": 0, "delta": text,
                    }),
                ));
                self.output_tokens += 1;
            }
            "tool-call" => {
                self.close_text_item(&mut out);
                let call_id = event
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("call_{}", &uuid::Uuid::new_v4().to_string()[..12]));
                let name = event.get("toolName").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = match event.get("input") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => "{}".to_string(),
                };
                let idx = self.next_output_index;
                self.next_output_index += 1;
                let item = serde_json::json!({
                    "type": "function_call", "id": format!("fc_{}", &uuid::Uuid::new_v4().to_string()[..12]),
                    "call_id": call_id, "name": name, "arguments": arguments, "status": "completed",
                });
                out.push(sse_event(
                    "response.output_item.added",
                    serde_json::json!({ "type": "response.output_item.added", "output_index": idx, "item": {
                        "type": "function_call", "id": item["id"], "call_id": call_id, "name": name, "arguments": "", "status": "in_progress",
                    } }),
                ));
                out.push(sse_event(
                    "response.function_call_arguments.delta",
                    serde_json::json!({ "type": "response.function_call_arguments.delta", "item_id": item["id"], "output_index": idx, "delta": arguments }),
                ));
                out.push(sse_event(
                    "response.function_call_arguments.done",
                    serde_json::json!({ "type": "response.function_call_arguments.done", "item_id": item["id"], "output_index": idx, "arguments": arguments }),
                ));
                out.push(sse_event(
                    "response.output_item.done",
                    serde_json::json!({ "type": "response.output_item.done", "output_index": idx, "item": item }),
                ));
                self.output_items.push(item);
                self.output_tokens += 20;
            }
            "finish-step" | "finish" => {
                if let Some(fr) = event.get("finishReason").and_then(|v| v.as_str()) {
                    self.stop_reason = Some(fr.to_string());
                }
                let u = event
                    .get("totalUsage")
                    .cloned()
                    .or_else(|| event.get("usage").cloned());
                if let Some(u) = u {
                    let mut input = u.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(self.input_tokens);
                    let mut output = u.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(self.output_tokens);
                    let mut cached = u.get("cachedInputTokens").and_then(|v| v.as_u64()).unwrap_or(self.cached_tokens);
                    normalize_usage(&mut input, &mut output, &mut cached);
                    self.input_tokens = input;
                    self.output_tokens = output;
                    self.cached_tokens = cached;
                }
            }
            "error" => {
                self.has_error = true;
                let msg = event
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .or_else(|| event.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("Unknown CC error");
                out.push(sse_event(
                    "response.failed",
                    serde_json::json!({
                        "type": "response.failed",
                        "response": { "id": self.response_id, "object": "response", "created_at": self.created_at,
                            "status": "failed", "model": self.model, "output": [],
                            "error": { "type": "server_error", "message": msg } },
                    }),
                ));
            }
            other => {
                log::warn(&format!("Unknown CC event type: {other}"));
            }
        }
        out
    }

    pub fn finalize(&mut self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.has_error {
            return out;
        }
        self.close_text_item(&mut out);
        if self.output_tokens == 0 {
            out.push(sse_event(
                "response.failed",
                serde_json::json!({
                    "type": "response.failed",
                    "response": { "id": self.response_id, "object": "response", "created_at": self.created_at,
                        "status": "failed", "model": self.model, "output": self.output_items,
                        "error": { "type": "rate_limit_error", "message": "Empty response from upstream (zero output tokens)" } },
                }),
            ));
            return out;
        }
        let items = std::mem::take(&mut self.output_items);
        let mut response = self.skeleton("completed", items);
        if self.stop_reason.as_deref() == Some("length") {
            response["status"] = serde_json::json!("incomplete");
            response["incomplete_details"] = serde_json::json!({ "reason": "max_output_tokens" });
        }
        response["usage"] = self.usage_value();
        out.push(sse_event(
            "response.completed",
            serde_json::json!({ "type": "response.completed", "response": response }),
        ));
        out
    }
}

/// CC NDJSON → Anthropic SSE 翻译器。
pub struct AnthropicTranslator {
    message_id: String,
    model: String,
    next_block_index: u32,
    current_block_index: i32,
    current_block_type: &'static str,
    block_started: bool,
    stop_reason: Option<String>,
    pub last_cc_event: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: Option<u64>,
    pub has_error: bool,
}

impl AnthropicTranslator {
    pub fn new(model: &str, message_id: &str) -> Self {
        Self {
            message_id: message_id.to_string(),
            model: model.to_string(),
            next_block_index: 0,
            current_block_index: -1,
            current_block_type: "",
            block_started: false,
            stop_reason: None,
            last_cc_event: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            cache_write_tokens: None,
            has_error: false,
        }
    }

    pub fn message_start(&self) -> String {
        format!(
            "event: message_start\ndata: {}\n\n",
            serde_json::json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                }
            })
        )
    }

    fn close_text_block(&mut self) -> String {
        if self.block_started && self.current_block_type == "text" {
            self.block_started = false;
            self.current_block_type = "";
            let idx = self.current_block_index;
            return format!(
                "event: content_block_stop\ndata: {}\n\n",
                serde_json::json!({ "type": "content_block_stop", "index": idx })
            );
        }
        String::new()
    }

    fn start_text_block(&mut self) -> String {
        if !self.block_started || self.current_block_type != "text" {
            let close = self.close_text_block();
            self.current_block_index = self.next_block_index as i32;
            self.next_block_index += 1;
            self.current_block_type = "text";
            self.block_started = true;
            let idx = self.current_block_index;
            close
                + &format!(
                    "event: content_block_start\ndata: {}\n\n",
                    serde_json::json!({ "type": "content_block_start", "index": idx, "content_block": { "type": "text", "text": "" } })
                )
        } else {
            String::new()
        }
    }

    pub fn process_line(&mut self, line: &str) -> Vec<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return Vec::new();
        }
        let event: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        let event_type = match event.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => return Vec::new(),
        };
        self.last_cc_event = event_type.to_string();
        let mut out: Vec<String> = Vec::new();

        match event_type {
            "start" | "start-step" | "text-start" | "reasoning-start" => {}
            "reasoning-delta" => {}
            "text-delta" => {
                let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let start_block = self.start_text_block();
                if !start_block.is_empty() {
                    out.push(start_block);
                }
                let idx = self.current_block_index;
                out.push(format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::json!({ "type": "content_block_delta", "index": idx, "delta": { "type": "text_delta", "text": text } })
                ));
                self.output_tokens += 1;
            }
            "tool-call" => {
                let close_block = self.close_text_block();
                if !close_block.is_empty() {
                    out.push(close_block);
                }
                let id = event
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        let suffix: String = uuid::Uuid::new_v4().to_string()[..12].to_string();
                        format!("toolu_{suffix}")
                    });
                let name = event.get("toolName").and_then(|v| v.as_str()).unwrap_or("");
                let input = match event.get("input") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => "{}".to_string(),
                };
                let idx = self.next_block_index;
                self.next_block_index += 1;
                out.push(format!(
                    "event: content_block_start\ndata: {}\n\n",
                    serde_json::json!({ "type": "content_block_start", "index": idx, "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} } })
                ));
                out.push(format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::json!({ "type": "content_block_delta", "index": idx, "delta": { "type": "input_json_delta", "partial_json": input } })
                ));
                out.push(format!(
                    "event: content_block_stop\ndata: {}\n\n",
                    serde_json::json!({ "type": "content_block_stop", "index": idx })
                ));
                self.output_tokens += 20;
            }
            "finish-step" | "finish" => {
                if let Some(fr) = event.get("finishReason").and_then(|v| v.as_str()) {
                    self.stop_reason = Some(map_anthropic_stop_reason(fr).to_string());
                }
                let u = event
                    .get("totalUsage")
                    .cloned()
                    .or_else(|| event.get("usage").cloned());
                if let Some(u) = u {
                    let mut input = u.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(self.input_tokens);
                    let mut output = u.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(self.output_tokens);
                    let mut cached = u.get("cachedInputTokens").and_then(|v| v.as_u64()).unwrap_or(self.cached_tokens);
                    normalize_usage(&mut input, &mut output, &mut cached);
                    self.input_tokens = input;
                    self.output_tokens = output;
                    self.cached_tokens = cached;
                    self.cache_write_tokens = u
                        .pointer("/inputTokenDetails/cacheWriteTokens")
                        .and_then(|v| v.as_u64());
                } else {
                    self.input_tokens = 0;
                    self.output_tokens = 0;
                    self.cached_tokens = 0;
                    self.cache_write_tokens = None;
                }
            }
            "error" => {
                self.has_error = true;
                let msg = event
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .or_else(|| event.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("Unknown CC error");
                out.push(format!(
                    "event: error\ndata: {}\n\n",
                    serde_json::json!({ "type": "error", "error": { "type": "internal_error", "message": msg } })
                ));
            }
            "reasoning-end" | "provider-metadata" | "tool-input-start" | "tool-input-delta" | "tool-input-end" | "tool-error" | "text-end" => {}
            other => {
                log::warn(&format!("Unknown CC event type: {other}"));
            }
        }
        out
    }

    pub fn finalize(&mut self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.has_error {
            return out;
        }
        let close_block = self.close_text_block();
        if !close_block.is_empty() {
            out.push(close_block);
        }
        if self.output_tokens == 0 {
            out.push(format!(
                "event: error\ndata: {}\n\n",
                serde_json::json!({
                    "type": "error",
                    "error": { "type": "rate_limit_error", "message": "Empty response from upstream (zero output tokens)" },
                    "retry_after": 10,
                })
            ));
        } else {
            out.push(format!(
                "event: message_delta\ndata: {}\n\n",
                serde_json::json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": self.stop_reason.clone().unwrap_or_else(|| "end_turn".into()) },
                    "usage": {
                        "output_tokens": self.output_tokens,
                        "cache_read_input_tokens": self.cached_tokens,
                        "cache_creation_input_tokens": self.cache_write_tokens,
                        "input_tokens": self.input_tokens,
                    },
                })
            ));
            out.push("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string());
        }
        out
    }
}
