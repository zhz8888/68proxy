use serde_json::Value;
use sha2::{Digest, Sha256};

use super::convert::{map_anthropic_stop_reason, map_finish_reason};
use super::log;
use super::state::now_secs;

/// 生成 Claude 格式的 thinking 假签名。
///
/// Anthropic 对 thinking signature 有密码学校验，第三方代理无法生成真签名；
/// Claude Code 的浅校验只要求 base64 以 'E'（单层）/ 'R'（双层）开头且 payload
/// 首字节为 0x12——此实现恰好满足，让 Command Code 能正常显示 thinking。payload 由思考文本
/// SHA-256 派生，使每个块的签名互不相同。
pub(crate) fn fake_thinking_signature(thinking_text: &str) -> String {
    let source = if thinking_text.is_empty() {
        "dsh-proxy-thinking"
    } else {
        thinking_text
    };
    let seed = Sha256::digest(source.as_bytes());
    let seed = &seed[..seed.len().min(64)];
    let mut raw = Vec::with_capacity(2 + seed.len());
    raw.push(0x12);
    raw.push(seed.len() as u8);
    raw.extend_from_slice(seed);
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(&raw)
}

/// 组装一个 OpenAI chat.completion.chunk SSE 帧（`data: {...}\n\n`）。
///
/// - `delta`：本帧的增量内容（role/content/tool_calls 等）；
/// - `finish_reason`：仅最后一帧携带；
/// - `usage`：可选，仅最后一帧附带 token 统计。
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

/// 用量归一化：上游输出为 0 token（空响应/异常）时把输入与缓存计数一并清零，
/// 避免下游把无效请求计入 token 统计。三个参数为就地修改的计数。
fn normalize_usage(input: &mut u64, output: &mut u64, cached: &mut u64) {
    if *output == 0 {
        *input = 0;
        *cached = 0;
    }
}

/// 读取 Command Code usage 对象中的缓存命中 token 数。
///
/// 上游真实格式为 `inputTokenDetails.cacheReadTokens`（与 inputTokens/outputTokens
/// 并列于 usage 顶层），旧版代理曾用顶层 `cachedInputTokens`，此处保留回退兼容。
pub fn read_cache_read_tokens(u: &Value) -> u64 {
    u.pointer("/inputTokenDetails/cacheReadTokens")
        .and_then(|v| v.as_u64())
        .or_else(|| u.get("cachedInputTokens").and_then(|v| v.as_u64()))
        .unwrap_or(0)
}

/// 读取 Command Code usage 对象中的缓存写入 token 数（`inputTokenDetails.cacheWriteTokens`）；缺失返回 0。
pub fn read_cache_write_tokens(u: &Value) -> u64 {
    u.pointer("/inputTokenDetails/cacheWriteTokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// Command Code NDJSON → OpenAI SSE 翻译器。
pub struct OpenAiTranslator {
    /// 下游响应体的 completion id（跨帧保持不变）。
    completion_id: String,
    /// 响应创建时间（Unix 秒）。
    created: u64,
    model: String,
    /// 已输出的 chunk 计数，首帧需额外携带 role 字段。
    chunk_index: u32,
    /// 下一个 tool_call 的 index（OpenAI tool_calls 增量按下标拼接）。
    tool_call_index: u32,
    /// finish-step 事件提前记录的 finish_reason，finish 事件缺省时回退使用。
    finish_reason: Option<String>,
    /// 最近一次解析到的 Command Code 事件类型（供请求追踪展示）。
    pub last_cc_event: String,
    /// 上游回报的输入 token 数。
    pub input_tokens: u64,
    /// 上游回报的输出 token 数。
    pub output_tokens: u64,
    /// 命中缓存的输入 token 数。
    pub cached_tokens: u64,
    /// 写入缓存的输入 token 数（映射为成本估算的 cache_write）。
    pub cache_write_tokens: u64,
}

impl OpenAiTranslator {
    /// 创建翻译器，`completion_id` 与模型名将出现在每个输出 chunk 中。
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
            cache_write_tokens: 0,
        }
    }

    /// 解析一行 Command Code NDJSON 事件，返回需要下发给下游的 SSE 帧列表（可能为空）。
    ///
    /// 空行、`[DONE]`、注释行与非法 JSON 直接忽略；事件解析失败不会中断流。
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
                // 首个 chunk 额外携带 role 字段，后续帧只发纯增量
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
                // finish_reason 与 usage 通常在 finish-step 就给出，先记录供 finish 帧缺省时回退
                if let Some(fr) = event.get("finishReason").and_then(|v| v.as_str()) {
                    self.finish_reason = Some(map_finish_reason(fr));
                }
                if let Some(u) = event.get("usage") {
                    self.input_tokens = u.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    self.output_tokens = u.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    self.cached_tokens = read_cache_read_tokens(u);
                    self.cache_write_tokens = read_cache_write_tokens(u);
                }
            }
            "finish" => {
                let fr = self
                    .finish_reason
                    .clone()
                    .unwrap_or_else(|| map_finish_reason(event.get("finishReason").and_then(|v| v.as_str()).unwrap_or("stop")));
                // 仅在带 usage 时更新计数：上游偶发不回 totalUsage，此时保留
                // finish-step 已记录的值，不要用 0 覆盖（否则会把已出正文的流误判为空响应）
                if let Some(u) = event.get("totalUsage").cloned().or_else(|| event.get("usage").cloned()) {
                    let mut input = u.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let mut output = u.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let mut cached = read_cache_read_tokens(&u);
                    normalize_usage(&mut input, &mut output, &mut cached);
                    self.input_tokens = input;
                    self.output_tokens = output;
                    self.cached_tokens = cached;
                    self.cache_write_tokens = read_cache_write_tokens(&u);
                }
                let usage = serde_json::json!({
                    "prompt_tokens": self.input_tokens,
                    "completion_tokens": self.output_tokens,
                    "total_tokens": self.input_tokens + self.output_tokens,
                    "prompt_tokens_details": { "cached_tokens": self.cached_tokens },
                });
                out.push(make_chunk(&self.completion_id, self.created, &self.model, serde_json::json!({}), Some(&fr), Some(usage)));
            }
            "error" => {
                let msg = event
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .or_else(|| event.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("Unknown error");
                log::warn(&format!("Command Code stream error: {msg}"));
            }
            "reasoning-end" | "provider-metadata" | "tool-input-start" | "tool-input-delta" | "tool-input-end" | "tool-error" | "text-end" => {}
            other => {
                log::warn(&format!("Unknown Command Code event type: {other}"));
            }
        }
        out
    }

    /// 是否已向下游产出过内容帧（正文/推理/工具调用）。
    ///
    /// 零输出判定以此为准而非 output_tokens：上游若未回报 usage，output_tokens 可能为 0，
    /// 但正文其实已经流式发出，不能据此补发“空响应”错误帧。
    pub fn produced_content(&self) -> bool {
        self.chunk_index > 0
    }

    /// 流正常结束时的终止帧 `data: [DONE]`。
    pub fn done_event(&self) -> String {
        "data: [DONE]\n\n".to_string()
    }

    /// 上游零输出（空响应）时下发的错误帧，伪装成限流并提示 10s 后重试。
    pub fn zero_output_error_frame(&self) -> String {
        let body = serde_json::json!({
            "error": { "message": "Empty response from upstream (zero output tokens)", "type": "rate_limit_error" },
            "retry_after": 10,
        });
        format!("data: {body}\n\n")
    }
}

/// 组装带事件名的 SSE 帧（`event: {name}\ndata: {payload}\n\n`）。
fn sse_event(name: &str, payload: Value) -> String {
    format!("event: {name}\ndata: {payload}\n\n")
}

/// Command Code NDJSON → OpenAI Responses SSE 事件翻译器。
/// 事件序列：response.created → output_item.added/content_part.added
/// → output_text.delta…/function_call_arguments.delta → 各 done → response.completed。
pub struct ResponsesTranslator {
    /// 下游响应体的 response id（跨事件保持不变）。
    response_id: String,
    /// 响应创建时间（Unix 秒）。
    created_at: u64,
    model: String,
    /// 每个事件的递增序号（Responses 规范要求 sequence_number）。
    seq: u32,
    /// 下一个 output 条目的全局下标（message 与 function_call 共享编号）。
    next_output_index: u32,
    /// 是否有尚未关闭的 message 文本条目。
    text_open: bool,
    /// 当前文本条目的 item id。
    text_item_id: String,
    /// 当前文本条目在 output 数组中的下标。
    text_item_index: u32,
    /// 当前文本条目累计的完整文本（done 事件需要全文）。
    item_text: String,
    /// 是否有尚未关闭的 reasoning（思考）条目。
    reasoning_open: bool,
    /// 当前 reasoning 条目的 item id。
    reasoning_item_id: String,
    /// 当前 reasoning 条目在 output 数组中的下标。
    reasoning_item_index: u32,
    /// 当前 reasoning 条目累计的完整思考文本。
    reasoning_text: String,
    /// 已完成的 output 条目，写入最终 response.completed 的 output 数组。
    output_items: Vec<Value>,
    /// 上游 finishReason 原始值（"length" 时最终置为 incomplete）。
    stop_reason: Option<String>,
    /// 最近一次解析到的 Command Code 事件类型（供请求追踪展示）。
    pub last_cc_event: String,
    /// 上游回报的输入 token 数。
    pub input_tokens: u64,
    /// 上游回报（或增量估算）的输出 token 数。
    pub output_tokens: u64,
    /// 命中缓存的输入 token 数。
    pub cached_tokens: u64,
    /// 写入缓存的输入 token 数（映射为成本估算的 cache_write）。
    pub cache_write_tokens: u64,
    /// 是否已产出过内容（正文/思考/工具调用）；零输出判定以此为准。
    produced_content: bool,
    /// 流中已出现过 error 事件，finalize 时不再补发完成事件。
    pub has_error: bool,
}

impl ResponsesTranslator {
    /// 创建翻译器，`response_id` 与模型名将出现在所有响应事件中。
    pub fn new(model: &str, response_id: &str) -> Self {
        Self {
            response_id: response_id.to_string(),
            created_at: now_secs(),
            model: model.to_string(),
            seq: 0,
            next_output_index: 0,
            text_open: false,
            text_item_id: String::new(),
            text_item_index: 0,
            item_text: String::new(),
            reasoning_open: false,
            reasoning_item_id: String::new(),
            reasoning_item_index: 0,
            reasoning_text: String::new(),
            output_items: Vec::new(),
            stop_reason: None,
            last_cc_event: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            cache_write_tokens: 0,
            produced_content: false,
            has_error: false,
        }
    }

    /// 构造 response 对象骨架（不含 usage/incomplete_details，由调用方补充）。
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

    /// 组装带事件名的 Responses SSE 帧，payload 自动带 `type` 与递增的 `sequence_number`
    /// （Responses 协议要求）。
    fn sse(&mut self, name: &str, mut payload: Value) -> String {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("sequence_number".into(), serde_json::json!(self.seq));
        }
        self.seq += 1;
        sse_event(name, payload)
    }

    /// 流开始时的 response.created 事件（status 为 in_progress、output 为空）。
    pub fn response_start(&mut self) -> String {
        self.sse(
            "response.created",
            serde_json::json!({ "type": "response.created", "response": self.skeleton("in_progress", vec![]) }),
        )
    }

    /// 是否已向下游产出过内容条目（正文/思考/工具调用）。
    ///
    /// 零输出判定以此为准而非 output_tokens：上游未回报 usage 时 output_tokens 可能为 0。
    pub fn produced_content(&self) -> bool {
        self.produced_content
    }

    /// 按 Responses 协议字段名组装 usage 对象。
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
        out.push(self.sse(
            "response.output_text.done",
            serde_json::json!({ "type": "response.output_text.done", "item_id": item_id, "output_index": idx, "content_index": 0, "text": text }),
        ));
        out.push(self.sse(
            "response.content_part.done",
            serde_json::json!({ "type": "response.content_part.done", "item_id": item_id, "output_index": idx, "content_index": 0, "part": { "type": "output_text", "text": text, "annotations": [] } }),
        ));
        let item = serde_json::json!({
            "type": "message", "id": item_id, "role": "assistant", "status": "completed",
            "content": [{ "type": "output_text", "text": text, "annotations": [] }],
        });
        out.push(self.sse(
            "response.output_item.done",
            serde_json::json!({ "type": "response.output_item.done", "output_index": idx, "item": item }),
        ));
        self.output_items.push(item);
    }

    /// 关闭当前未完结的 reasoning 条目，发出 summary 收尾事件与 output_item.done。
    fn close_reasoning_item(&mut self, out: &mut Vec<String>) {
        if !self.reasoning_open {
            return;
        }
        self.reasoning_open = false;
        let item_id = self.reasoning_item_id.clone();
        let idx = self.reasoning_item_index;
        let text = std::mem::take(&mut self.reasoning_text);
        out.push(self.sse(
            "response.reasoning_summary_text.done",
            serde_json::json!({ "type": "response.reasoning_summary_text.done", "item_id": item_id, "output_index": idx, "summary_index": 0, "text": text }),
        ));
        out.push(self.sse(
            "response.reasoning_summary_part.done",
            serde_json::json!({ "type": "response.reasoning_summary_part.done", "item_id": item_id, "output_index": idx, "summary_index": 0,
                "part": { "type": "summary_text", "text": text } }),
        ));
        let item = serde_json::json!({
            "type": "reasoning", "id": item_id, "status": "completed",
            "summary": [{ "type": "summary_text", "text": text }],
        });
        out.push(self.sse(
            "response.output_item.done",
            serde_json::json!({ "type": "response.output_item.done", "output_index": idx, "item": item }),
        ));
        self.output_items.push(item);
    }

    /// 打开一个新的 reasoning 条目（reasoning_summary_part.added）。
    fn open_reasoning_item(&mut self, out: &mut Vec<String>) {
        let idx = self.next_output_index;
        self.next_output_index += 1;
        self.reasoning_item_index = idx;
        self.reasoning_item_id = format!("rs_{}", &uuid::Uuid::new_v4().to_string()[..12]);
        self.reasoning_open = true;
        let item_id = self.reasoning_item_id.clone();
        out.push(self.sse(
            "response.output_item.added",
            serde_json::json!({
                "type": "response.output_item.added", "output_index": idx,
                "item": { "type": "reasoning", "id": item_id, "status": "in_progress", "summary": [] },
            }),
        ));
        out.push(self.sse(
            "response.reasoning_summary_part.added",
            serde_json::json!({
                "type": "response.reasoning_summary_part.added", "item_id": item_id, "output_index": idx, "summary_index": 0,
                "part": { "type": "summary_text", "text": "" },
            }),
        ));
    }
    ///
    /// 文本增量会自动开启/延续 message 条目；tool-call 会先关闭未完结的文本条目，
    /// 再发出 function_call 的完整事件序列（added → arguments.delta/done → item.done）。
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
            "start" | "start-step" | "text-start" | "reasoning-start"
            | "reasoning-end" | "provider-metadata" | "tool-input-start" | "tool-input-delta"
            | "tool-input-end" | "tool-error" | "text-end" => {}
            // 思考增量 → reasoning 条目的 summary 文本增量
            "reasoning-delta" => {
                let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.is_empty() {
                    return out;
                }
                self.produced_content = true;
                if !self.reasoning_open {
                    self.open_reasoning_item(&mut out);
                }
                self.reasoning_text.push_str(text);
                out.push(self.sse(
                    "response.reasoning_summary_text.delta",
                    serde_json::json!({
                        "type": "response.reasoning_summary_text.delta", "item_id": self.reasoning_item_id,
                        "output_index": self.reasoning_item_index, "summary_index": 0, "delta": text,
                    }),
                ));
            }
            "text-delta" => {
                let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.is_empty() {
                    return out;
                }
                self.produced_content = true;
                // 正文开始前先收尾思考条目
                self.close_reasoning_item(&mut out);
                if !self.text_open {
                    let idx = self.next_output_index;
                    self.next_output_index += 1;
                    self.text_item_index = idx;
                    self.text_item_id = format!("msg_{}", &uuid::Uuid::new_v4().to_string()[..12]);
                    self.text_open = true;
                    let item_id = self.text_item_id.clone();
                    out.push(self.sse(
                        "response.output_item.added",
                        serde_json::json!({
                            "type": "response.output_item.added", "output_index": idx,
                            "item": { "type": "message", "id": item_id, "role": "assistant", "status": "in_progress", "content": [] },
                        }),
                    ));
                    out.push(self.sse(
                        "response.content_part.added",
                        serde_json::json!({
                            "type": "response.content_part.added", "item_id": item_id, "output_index": idx, "content_index": 0,
                            "part": { "type": "output_text", "text": "", "annotations": [] },
                        }),
                    ));
                }
                self.item_text.push_str(text);
                out.push(self.sse(
                    "response.output_text.delta",
                    serde_json::json!({
                        "type": "response.output_text.delta", "item_id": self.text_item_id,
                        "output_index": self.text_item_index, "content_index": 0, "delta": text,
                    }),
                ));
                // 上游 finish 前无法得知真实 token，先按每段增量 1 token 粗估，最终以 finish 事件为准
                self.output_tokens += 1;
            }
            "tool-call" => {
                self.produced_content = true;
                self.close_reasoning_item(&mut out);
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
                out.push(self.sse(
                    "response.output_item.added",
                    serde_json::json!({ "type": "response.output_item.added", "output_index": idx, "item": {
                        "type": "function_call", "id": item["id"], "call_id": call_id, "name": name, "arguments": "", "status": "in_progress",
                    } }),
                ));
                out.push(self.sse(
                    "response.function_call_arguments.delta",
                    serde_json::json!({ "type": "response.function_call_arguments.delta", "item_id": item["id"], "output_index": idx, "delta": arguments }),
                ));
                out.push(self.sse(
                    "response.function_call_arguments.done",
                    serde_json::json!({ "type": "response.function_call_arguments.done", "item_id": item["id"], "output_index": idx, "arguments": arguments }),
                ));
                out.push(self.sse(
                    "response.output_item.done",
                    serde_json::json!({ "type": "response.output_item.done", "output_index": idx, "item": item }),
                ));
                self.output_items.push(item);
                // tool call 按固定 20 token 粗估（同上，最终以 finish 事件回报值覆盖）
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
                    let mut cached = read_cache_read_tokens(&u);
                    normalize_usage(&mut input, &mut output, &mut cached);
                    self.input_tokens = input;
                    self.output_tokens = output;
                    self.cached_tokens = cached;
                    self.cache_write_tokens = read_cache_write_tokens(&u);
                }
            }
            "error" => {
                self.has_error = true;
                let msg = event
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .or_else(|| event.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("Unknown Command Code error");
                out.push(self.sse(
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
                log::warn(&format!("Unknown Command Code event type: {other}"));
            }
        }
        out
    }

    /// 流结束收尾：关闭未完结条目并补发 response.completed / response.failed。
    ///
    /// 已出错过（has_error）时返回空；无任何实际内容（文本/思考/工具调用）视为上游
    /// 空响应，发 failed + rate_limit_error；finishReason 为 length 时 status 置
    /// incomplete 并附 max_output_tokens 原因。
    pub fn finalize(&mut self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.has_error {
            return out;
        }
        self.close_reasoning_item(&mut out);
        self.close_text_item(&mut out);
        if self.output_items.is_empty() {
            out.push(self.sse(
                "response.failed",
                serde_json::json!({
                    "type": "response.failed",
                    "response": { "id": self.response_id, "object": "response", "created_at": self.created_at,
                        "status": "failed", "model": self.model, "output": [],
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
        out.push(self.sse(
            "response.completed",
            serde_json::json!({ "type": "response.completed", "response": response }),
        ));
        out
    }
}

/// Command Code NDJSON → Anthropic SSE 翻译器。
pub struct AnthropicTranslator {
    /// 下游响应体的 message id（跨事件保持不变）。
    message_id: String,
    model: String,
    /// 下一个 content block 的 index（text 与 tool_use 共享编号）。
    next_block_index: u32,
    /// 当前打开的 content block index，-1 表示没有打开的块。
    current_block_index: i32,
    /// 当前打开块的类型（"text"、"thinking" 或空）。
    current_block_type: &'static str,
    /// 是否已发出当前块的 content_block_start 事件。
    block_started: bool,
    /// 当前打开的 thinking 块累计文本（关闭时用于派生假签名）。
    current_thinking_text: String,
    /// 上游 inputTokenDetails.noCacheTokens（Anthropic 的 input_tokens 只计非缓存部分）。
    no_cache_tokens: Option<u64>,
    /// 上游 finishReason 映射后的 Anthropic stop_reason。
    stop_reason: Option<String>,
    /// 最近一次解析到的 Command Code 事件类型（供请求追踪展示）。
    pub last_cc_event: String,
    /// 上游回报的输入 token 数。
    pub input_tokens: u64,
    /// 上游回报（或增量估算）的输出 token 数。
    pub output_tokens: u64,
    /// 命中缓存的输入 token 数（映射为 cache_read_input_tokens）。
    pub cached_tokens: u64,
    /// 写入缓存的输入 token 数（映射为 cache_creation_input_tokens，可能缺失）。
    pub cache_write_tokens: Option<u64>,
    /// 是否已向下游产出过内容（正文/思考/工具调用）；零输出判定以此为准。
    pub produced_content: bool,
    /// 流中已出现过 error 事件，finalize 时不再补发完成事件。
    pub has_error: bool,
}

impl AnthropicTranslator {
    /// 创建翻译器，`message_id` 与模型名将出现在 message_start 事件中。
    pub fn new(model: &str, message_id: &str) -> Self {
        Self {
            message_id: message_id.to_string(),
            model: model.to_string(),
            next_block_index: 0,
            current_block_index: -1,
            current_block_type: "",
            block_started: false,
            current_thinking_text: String::new(),
            no_cache_tokens: None,
            stop_reason: None,
            last_cc_event: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            cache_write_tokens: None,
            produced_content: false,
            has_error: false,
        }
    }

    /// 流开始时的 message_start 事件（usage 先置 0，最终以 message_delta 回报为准）。
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

    /// 关闭当前打开的块（text 或 thinking），返回需要下发的帧；无打开块时返回空串。
    ///
    /// thinking 块关闭前先发 `signature_delta`（Anthropic 标准，供 Claude Code 显示
    /// 思考内容），签名由累计的思考文本派生。
    fn close_block(&mut self) -> String {
        if self.block_started {
            let idx = self.current_block_index;
            let block_type = self.current_block_type;
            let mut out = String::new();
            if block_type == "thinking" {
                let signature = fake_thinking_signature(&self.current_thinking_text);
                self.current_thinking_text.clear();
                out.push_str(&format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::json!({ "type": "content_block_delta", "index": idx, "delta": { "type": "signature_delta", "signature": signature } })
                ));
            }
            self.block_started = false;
            self.current_block_type = "";
            out.push_str(&format!(
                "event: content_block_stop\ndata: {}\n\n",
                serde_json::json!({ "type": "content_block_stop", "index": idx })
            ));
            out
        } else {
            String::new()
        }
    }

    /// 确保有一个打开的指定类型块：若当前块类型不符，先关闭旧块再发出新的
    /// content_block_start，返回需要下发的帧（可能包含关闭旧块的事件）。
    fn start_block(&mut self, block_type: &'static str, content_block: Value) -> String {
        if !self.block_started || self.current_block_type != block_type {
            let close = self.close_block();
            self.current_block_index = self.next_block_index as i32;
            self.next_block_index += 1;
            self.current_block_type = block_type;
            self.block_started = true;
            let idx = self.current_block_index;
            close
                + &format!(
                    "event: content_block_start\ndata: {}\n\n",
                    serde_json::json!({ "type": "content_block_start", "index": idx, "content_block": content_block })
                )
        } else {
            String::new()
        }
    }

    /// 确保有一个打开的 text 块。
    fn start_text_block(&mut self) -> String {
        self.start_block("text", serde_json::json!({ "type": "text", "text": "" }))
    }

    /// 确保有一个打开的 thinking 块。
    fn start_thinking_block(&mut self) -> String {
        self.start_block("thinking", serde_json::json!({ "type": "thinking", "thinking": "" }))
    }

    /// 解析一行 Command Code NDJSON 事件，返回需下发的 Anthropic SSE 事件列表（可能为空）。
    ///
    /// 文本增量会自动开启/延续 text 块；tool-call 先关闭 text 块再一次性发出
    /// tool_use 的 start/delta/stop 三帧；reasoning-delta 映射为 thinking 块
    /// （Claude Code 会将其显示为思考内容）。
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
            // Command Code reasoning → Anthropic thinking 块（Claude Code 将其显示为思考内容）
            "reasoning-delta" => {
                let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.is_empty() {
                    return out;
                }
                self.produced_content = true;
                let start_block = self.start_thinking_block();
                if !start_block.is_empty() {
                    out.push(start_block);
                }
                self.current_thinking_text.push_str(text);
                let idx = self.current_block_index;
                out.push(format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::json!({ "type": "content_block_delta", "index": idx, "delta": { "type": "thinking_delta", "thinking": text } })
                ));
                // thinking 文本不计入 output_tokens（Anthropic 的 output_tokens 只含正文）
            }
            "text-delta" => {
                let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
                self.produced_content = true;
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
                self.produced_content = true;
                let close_block = self.close_block();
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
                    let mut cached = read_cache_read_tokens(&u);
                    normalize_usage(&mut input, &mut output, &mut cached);
                    self.input_tokens = input;
                    self.output_tokens = output;
                    self.cached_tokens = cached;
                    self.cache_write_tokens = u
                        .pointer("/inputTokenDetails/cacheWriteTokens")
                        .and_then(|v| v.as_u64());
                    if let Some(nc) = u.pointer("/inputTokenDetails/noCacheTokens").and_then(|v| v.as_u64()) {
                        self.no_cache_tokens = Some(nc);
                    }
                }
                // 上游未回报 usage 时保留逐 delta 估算，不清零：清零会让 finalize
                // 把已流式输出的正文误判为空响应（发 rate_limit_error 而不发收尾事件）
            }
            "error" => {
                self.has_error = true;
                let msg = event
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .or_else(|| event.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("Unknown Command Code error");
                out.push(format!(
                    "event: error\ndata: {}\n\n",
                    serde_json::json!({ "type": "error", "error": { "type": "internal_error", "message": msg } })
                ));
            }
            "reasoning-end" | "provider-metadata" | "tool-input-start" | "tool-input-delta" | "tool-input-end" | "tool-error" | "text-end" => {}
            other => {
                log::warn(&format!("Unknown Command Code event type: {other}"));
            }
        }
        out
    }

    /// 流结束收尾：关闭打开的块（thinking 先发 signature_delta）后补发
    /// message_delta + message_stop；从未产出任何内容时改发 error 帧（rate_limit_error +
    /// retry_after），已出错过则返回空。
    pub fn finalize(&mut self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.has_error {
            return out;
        }
        let close_block = self.close_block();
        if !close_block.is_empty() {
            out.push(close_block);
        }
        // 以「是否产出过内容」而非 output_tokens==0 判定空响应：上游未回报 usage 时
        // output_tokens 可能为 0，但正文已经流式输出，不能误发限流错误
        if !self.produced_content {
            out.push(format!(
                "event: error\ndata: {}\n\n",
                serde_json::json!({
                    "type": "error",
                    "error": { "type": "rate_limit_error", "message": "Empty response from upstream (zero output tokens)" },
                    "retry_after": 10,
                })
            ));
        } else {
            // Anthropic 的 input_tokens 只计非缓存部分：优先取上游
            // noCacheTokens，缺失时用总数减缓存命中与缓存写入估算。
            let input_tokens = match self.no_cache_tokens {
                Some(nc) => nc,
                None => self
                    .input_tokens
                    .saturating_sub(self.cached_tokens)
                    .saturating_sub(self.cache_write_tokens.unwrap_or(0)),
            };
            out.push(format!(
                "event: message_delta\ndata: {}\n\n",
                serde_json::json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": self.stop_reason.clone().unwrap_or_else(|| "end_turn".into()) },
                    "usage": {
                        "output_tokens": self.output_tokens,
                        "cache_read_input_tokens": self.cached_tokens,
                        "cache_creation_input_tokens": self.cache_write_tokens,
                        "input_tokens": input_tokens,
                    },
                })
            ));
            out.push("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string());
        }
        out
    }
}
