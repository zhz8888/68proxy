use serde_json::{json, Map, Value};

use super::state::now_secs;

pub const DEFAULT_MODEL: &str = "deepseek/deepseek-v4-flash";
const MAX_TOKENS_CAP: u64 = 200_000;
const DEFAULT_MAX_TOKENS: u64 = 64_000;
const ENV_STRING: &str = "win32-x64, Node.js v24.16.0";

fn try_parse_json(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| json!({}))
}

fn as_str_or_empty(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// OpenAI Chat Completions 请求 → CC 请求体（CLI 信封格式）。
pub fn build_cc_request(openai_req: &Value) -> Value {
    let model = openai_req
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_MODEL)
        .to_string();
    let messages = openai_req
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // system/developer 消息提取为顶层 system（OpenAI 新规范用 developer 承载 system prompt）
    let is_system_role =
        |m: &&Value| matches!(m.get("role").and_then(|r| r.as_str()), Some("system") | Some("developer"));
    let system_msgs: Vec<&Value> = messages.iter().filter(is_system_role).collect();
    let system_prompt = system_msgs
        .iter()
        .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let chat_messages: Vec<&Value> = messages
        .iter()
        .filter(|m| !is_system_role(m))
        .collect();

    // tool_call_id → tool_name 反查表
    let mut tool_name_map: Map<String, Value> = Map::new();
    for msg in &chat_messages {
        if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
            if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        let name = tc
                            .pointer("/function/name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        tool_name_map.insert(id.to_string(), json!(name));
                    }
                }
            }
        }
    }

    let cc_messages: Vec<Value> = chat_messages
        .iter()
        .map(|msg| match msg.get("role").and_then(|r| r.as_str()) {
            Some("user") => {
                let content = msg.get("content");
                let parts = match content {
                    Some(Value::String(s)) => json!([{ "type": "text", "text": s }]),
                    Some(Value::Array(arr)) => {
                        let mapped: Vec<Value> = arr
                            .iter()
                            .map(|part| {
                                if part.get("type").and_then(|t| t.as_str()) == Some("image_url") {
                                    let url = part
                                        .pointer("/image_url/url")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    json!({ "type": "image", "image": url })
                                } else {
                                    part.clone()
                                }
                            })
                            .collect();
                        Value::Array(mapped)
                    }
                    other => json!([{ "type": "text", "text": other.map(|v| v.to_string()).unwrap_or_default() }]),
                };
                json!({ "role": "user", "content": parts })
            }
            Some("assistant") => {
                let mut parts: Vec<Value> = Vec::new();
                match msg.get("content") {
                    Some(Value::String(s)) => {
                        if !s.is_empty() {
                            parts.push(json!({ "type": "text", "text": s }));
                        }
                    }
                    Some(Value::Array(arr)) => {
                        for part in arr {
                            if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                                parts.push(part.clone());
                            }
                        }
                    }
                    _ => {}
                }
                if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        let args = tc
                            .pointer("/function/arguments")
                            .and_then(|v| v.as_str())
                            .map(try_parse_json)
                            .unwrap_or_else(|| json!({}));
                        parts.push(json!({
                            "type": "tool-call",
                            "toolCallId": as_str_or_empty(tc.get("id").unwrap_or(&Value::Null)),
                            "toolName": tc.pointer("/function/name").and_then(|v| v.as_str()).unwrap_or(""),
                            "input": args,
                        }));
                    }
                }
                json!({ "role": "assistant", "content": parts })
            }
            Some("tool") => {
                let tool_call_id = as_str_or_empty(msg.get("tool_call_id").unwrap_or(&Value::Null));
                let tool_name = tool_name_map
                    .get(&tool_call_id)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_name = if tool_name.is_empty() {
                    as_str_or_empty(msg.get("name").unwrap_or(&Value::Null))
                } else {
                    tool_name
                };
                let output = match msg.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => String::new(),
                };
                json!({
                    "role": "tool",
                    "content": [{
                        "type": "tool-result",
                        "toolCallId": tool_call_id,
                        "toolName": tool_name,
                        "output": { "type": "text", "value": output },
                    }],
                })
            }
            _ => (*msg).clone(),
        })
        .collect();

    let max_tokens = openai_req
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_TOKENS)
        .min(MAX_TOKENS_CAP);

    let mut body = json!({
        "config": {
            "workingDir": "",
            "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "environment": ENV_STRING,
            "structure": [],
            "isGitRepo": false,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": [],
        },
        "memory": Value::Null,
        "taste": Value::Null,
        "skills": "",
        "permissionMode": "standard",
        "params": {
            "model": model,
            "messages": cc_messages,
            "max_tokens": max_tokens,
            "stream": true,
        },
    });

    let params = body
        .get_mut("params")
        .and_then(|p| p.as_object_mut())
        .expect("params is object");

    if !system_prompt.is_empty() {
        params.insert("system".into(), json!(system_prompt));
    }
    if let Some(t) = openai_req.get("temperature") {
        params.insert("temperature".into(), t.clone());
    }
    if let Some(r) = openai_req.get("reasoning_effort") {
        params.insert("reasoning_effort".into(), r.clone());
    }
    if let Some(tools) = openai_req.get("tools").and_then(|v| v.as_array()) {
        if !tools.is_empty() {
            let mapped: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "type": t.get("type").and_then(|v| v.as_str()).unwrap_or("function"),
                        "name": t.pointer("/function/name").and_then(|v| v.as_str()).unwrap_or(""),
                        "description": t.pointer("/function/description").and_then(|v| v.as_str()).unwrap_or(""),
                        "input_schema": t.pointer("/function/parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                    })
                })
                .collect();
            params.insert("tools".into(), Value::Array(mapped));
        }
    }
    if let Some(tc) = openai_req.get("tool_choice") {
        let mapped = match tc {
            Value::String(s) => {
                let t = match s.as_str() {
                    "auto" => "auto",
                    "none" => "none",
                    "required" => "any",
                    _ => "auto",
                };
                json!({ "type": t })
            }
            Value::Object(o) if o.get("type").and_then(|v| v.as_str()) == Some("function") => {
                let name = o
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                json!({ "type": "tool", "name": name })
            }
            other => other.clone(),
        };
        params.insert("tool_choice".into(), mapped);
    }
    if let Some(p) = openai_req.get("parallel_tool_calls") {
        params.insert("parallel_tool_calls".into(), p.clone());
    }

    // 注：threadId 变量已生成但按当前协议不写入请求体，保持实际行为一致。
    body
}

/// Anthropic Messages 请求 → OpenAI Chat Completions 请求。
pub fn convert_anthropic_to_openai(anthropic_req: &Value) -> Value {
    // 1. system
    let mut system_prompt = String::new();
    if let Some(sys) = anthropic_req.get("system") {
        match sys {
            Value::String(s) => system_prompt = s.clone(),
            Value::Array(arr) => {
                system_prompt = arr
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            _ => {}
        }
    }

    // 2. messages
    let mut tool_name_from_id: Map<String, Value> = Map::new();
    let mut openai_messages: Vec<Value> = Vec::new();
    if !system_prompt.is_empty() {
        openai_messages.push(json!({ "role": "system", "content": system_prompt }));
    }

    let messages = anthropic_req
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for msg in &messages {
        match msg.get("role").and_then(|r| r.as_str()) {
            Some("assistant") => {
                let mut text_content = String::new();
                let mut tool_calls: Vec<Value> = Vec::new();
                let blocks = msg
                    .get("content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_else(|| {
                        vec![json!({
                            "type": "text",
                            "text": msg.get("content").and_then(|c| c.as_str()).unwrap_or(""),
                        })]
                    });
                for block in &blocks {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                text_content.push_str(t);
                            }
                        }
                        Some("tool_use") => {
                            let id = as_str_or_empty(block.get("id").unwrap_or(&Value::Null));
                            let name = as_str_or_empty(block.get("name").unwrap_or(&Value::Null));
                            if !id.is_empty() {
                                tool_name_from_id.insert(id.clone(), json!(name));
                            }
                            let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                            tool_calls.push(json!({
                                "id": id,
                                "type": "function",
                                "function": { "name": name, "arguments": input.to_string() },
                            }));
                        }
                        _ => {}
                    }
                }
                let mut assistant_msg = json!({
                    "role": "assistant",
                    "content": if text_content.is_empty() { Value::Null } else { json!(text_content) },
                });
                if !tool_calls.is_empty() {
                    assistant_msg["tool_calls"] = Value::Array(tool_calls);
                }
                openai_messages.push(assistant_msg);
            }
            Some("user") => {
                let mut text_content = String::new();
                let mut tool_results: Vec<Value> = Vec::new();
                match msg.get("content") {
                    Some(Value::String(s)) => text_content = s.clone(),
                    Some(Value::Array(arr)) => {
                        for block in arr {
                            match block.get("type").and_then(|t| t.as_str()) {
                                Some("text") => {
                                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                        text_content.push_str(t);
                                    }
                                }
                                Some("tool_result") => tool_results.push(block.clone()),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                if !text_content.is_empty() {
                    openai_messages.push(json!({ "role": "user", "content": text_content }));
                }
                for tr in &tool_results {
                    let tool_use_id = as_str_or_empty(tr.get("tool_use_id").unwrap_or(&Value::Null));
                    let content = match tr.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Array(arr)) => arr
                            .iter()
                            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join(""),
                        other => other.map(|v| v.to_string()).unwrap_or_default(),
                    };
                    openai_messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "name": tool_name_from_id.get(&tool_use_id).and_then(|v| v.as_str()).unwrap_or(""),
                        "content": content,
                    }));
                }
            }
            _ => {}
        }
    }

    let model = anthropic_req
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_MODEL)
        .to_string();
    let max_tokens = anthropic_req
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_TOKENS);

    let mut openai_req = json!({
        "model": model,
        "messages": openai_messages,
        "max_tokens": max_tokens,
        "stream": anthropic_req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
    });

    // 4. tools
    if let Some(tools) = anthropic_req.get("tools").and_then(|v| v.as_array()) {
        if !tools.is_empty() {
            let mapped: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                            "description": t.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                            "parameters": t.get("input_schema").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                        },
                    })
                })
                .collect();
            openai_req["tools"] = Value::Array(mapped);
        }
    }

    // 5. tool_choice
    if let Some(tc) = anthropic_req.get("tool_choice") {
        let t = tc.get("type").and_then(|v| v.as_str()).unwrap_or("auto");
        let mapped = match t {
            "any" => json!("required"),
            "tool" => json!({ "type": "function", "function": { "name": tc.get("name").and_then(|v| v.as_str()).unwrap_or("") } }),
            "none" => json!("none"),
            _ => json!("auto"),
        };
        openai_req["tool_choice"] = mapped;
    }

    // 6. optional params
    if let Some(t) = anthropic_req.get("temperature") {
        openai_req["temperature"] = t.clone();
    }
    if let Some(p) = anthropic_req.get("top_p") {
        openai_req["top_p"] = p.clone();
    }
    if let Some(ss) = anthropic_req.get("stop_sequences") {
        openai_req["stop"] = ss.clone();
    }
    if let Some(uid) = anthropic_req.pointer("/metadata/user_id") {
        openai_req["user"] = uid.clone();
    }

    // 7. thinking → reasoning_effort
    if let Some(thinking) = anthropic_req.get("thinking") {
        let t = thinking.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if t == "adaptive" {
            let effort = thinking
                .get("effort")
                .and_then(|v| v.as_str())
                .unwrap_or("medium");
            openai_req["reasoning_effort"] = json!(effort);
        } else if let Some(budget) = thinking.get("budget_tokens").and_then(|v| v.as_u64()) {
            let effort = if budget >= 10_000 {
                "high"
            } else if budget >= 5_000 {
                "medium"
            } else {
                "low"
            };
            openai_req["reasoning_effort"] = json!(effort);
        }
    }

    openai_req
}

pub fn map_finish_reason(reason: &str) -> String {
    match reason {
        "tool-calls" => "tool_calls".into(),
        "length" => "length".into(),
        "stop" => "stop".into(),
        other if other.is_empty() => "stop".into(),
        other => other.to_string(),
    }
}

pub fn map_anthropic_stop_reason(reason: &str) -> &'static str {
    match reason {
        "tool_calls" => "tool_use",
        "length" => "max_tokens",
        "stop" => "end_turn",
        _ => "end_turn",
    }
}

/// 非流式 OpenAI 响应构建。
pub fn build_openai_response(
    completion_id: &str,
    model: &str,
    full_text: &str,
    reasoning_content: &str,
    tool_calls: Option<&[Value]>,
    finish_reason: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> Value {
    let mut message = json!({ "role": "assistant", "content": if full_text.is_empty() { Value::Null } else { json!(full_text) } });
    if let Some(tcs) = tool_calls {
        message["tool_calls"] = Value::Array(tcs.to_vec());
    }
    if !reasoning_content.is_empty() {
        message["reasoning_content"] = json!(reasoning_content);
    }
    json!({
        "id": completion_id,
        "object": "chat.completion",
        "created": now_secs(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
            "prompt_tokens_details": { "cached_tokens": cached_tokens },
        },
    })
}

/// 非流式 Anthropic 响应构建。
pub fn build_anthropic_response(
    message_id: &str,
    model: &str,
    full_text: &str,
    tool_calls: Option<&[Value]>,
    finish_reason: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: Option<u64>,
) -> Value {
    let mut content: Vec<Value> = Vec::new();
    if !full_text.is_empty() {
        content.push(json!({ "type": "text", "text": full_text }));
    }
    if let Some(tcs) = tool_calls {
        for tc in tcs {
            let input = tc
                .pointer("/function/arguments")
                .and_then(|v| v.as_str())
                .map(try_parse_json)
                .unwrap_or_else(|| json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": tc.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "name": tc.pointer("/function/name").and_then(|v| v.as_str()).unwrap_or(""),
                "input": input,
            }));
        }
    }
    json!({
        "id": message_id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": map_anthropic_stop_reason(finish_reason),
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_creation_input_tokens": cache_write_tokens,
            "cache_read_input_tokens": cached_tokens,
        },
    })
}
