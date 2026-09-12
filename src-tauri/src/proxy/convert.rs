use serde_json::{json, Map, Value};

use super::state::now_secs;

/// 请求未指定 model 时使用的默认模型。
pub const DEFAULT_MODEL: &str = "deepseek/deepseek-v4-flash";
/// 上游 max_tokens 上限（超过则截断）。
const MAX_TOKENS_CAP: u64 = 200_000;
/// 请求未指定 max_tokens 时的默认值。
const DEFAULT_MAX_TOKENS: u64 = 64_000;
/// 写入 CLI 信封 config.environment 的伪装运行环境描述。
const ENV_STRING: &str = "win32-x64, Node.js v24.16.0";

/// 尽力解析 JSON 字符串，失败时返回空对象 `{}`（用于 tool_call 的 arguments）。
fn try_parse_json(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| json!({}))
}

/// 取 JSON 值的字符串内容，非字符串或缺失时返回空串。
fn as_str_or_empty(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// 判断 CC 消息列表里是否已存在 cache_control 标记（用于避免重复注入）。
fn has_cache_marker(cc_messages: &[Value]) -> bool {
    cc_messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_array())
            .map(|parts| parts.iter().any(|p| p.get("cache_control").is_some()))
            .unwrap_or(false)
    })
}

/// OpenAI Chat Completions 请求 → CC 请求体（CLI 信封格式）。
///
/// 主要转换：system/developer 消息提取为 params.system（为空且开关开启时发空格占位，
/// 阻止上游注入默认提示词）；user/assistant/tool 消息转为 CC 的 content parts 结构
/// （text/image/tool-call/tool-result）；assistant 的 reasoning_content 与 content 内
/// reasoning part 回传为 `{type:"reasoning"}`；tools 扁平化为 `{type, name, description,
/// input_schema}`；tool_choice 的 required 映射为 any；max_tokens 缺省 64000 并封顶
/// 200000；stream 恒为 true（上游只支持流式）。
///
/// - `empty_system_placeholder`：无 system 时是否发 `" "` 占位，防止 CC 上游注入
///   约 7.5K token 的默认提示词。
pub fn build_cc_request(openai_req: &Value, empty_system_placeholder: bool) -> Value {
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

    let mut cc_messages: Vec<Value> = chat_messages
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
                // 思考内容必须回传：CC 在 thinking 模式下校验 reasoning 是否随历史带回，
                // 丢弃会让上游直接拒绝。reasoning 须置于文本之前。
                let reasoning_field = msg
                    .get("reasoning_content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !reasoning_field.is_empty() {
                    parts.push(json!({ "type": "reasoning", "text": reasoning_field }));
                }
                let mut reasoning_part_seen = !reasoning_field.is_empty();
                match msg.get("content") {
                    Some(Value::String(s)) if !s.is_empty() => {
                        parts.push(json!({ "type": "text", "text": s }));
                    }
                    Some(Value::String(_)) | None => {}
                    Some(Value::Array(arr)) => {
                        for part in arr {
                            match part.get("type").and_then(|t| t.as_str()) {
                                Some("text") => parts.push(part.clone()),
                                // 客户端直接把 reasoning 放在 content 数组里时同样透传；
                                // 已有 reasoning_content 字段则不重复
                                Some("reasoning") if !reasoning_part_seen => {
                                    reasoning_part_seen = true;
                                    parts.push(part.clone());
                                }
                                _ => {}
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
                    Some(v) => serde_json::to_string(v).unwrap_or_default(),
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

    // prompt_cache_key：在首个 user 消息的最后一个 text 块上注入 cache_control，
    // 让上游把该块之前的内容作为 prompt cache 命中（OpenAI 风格缓存标记）。
    // 消息中已有任何 cache_control 标记时跳过注入，避免重复标记。
    if let Some(cache_key) = openai_req.get("prompt_cache_key").and_then(|v| v.as_str()) {
        if !cache_key.is_empty() && !has_cache_marker(&cc_messages) {
            if let Some(first_user) = cc_messages
                .iter_mut()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            {
                if let Some(content) = first_user.get_mut("content").and_then(|c| c.as_array_mut()) {
                    if let Some(last_text) = content
                        .iter_mut()
                        .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
                        .last()
                    {
                        last_text["cache_control"] = json!({ "type": "ephemeral" });
                    }
                }
            }
        }
    }

    let max_tokens = openai_req
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_TOKENS)
        .min(MAX_TOKENS_CAP);

    let mut body = json!({
        "config": {
            "workingDir": std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
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
    } else if empty_system_placeholder {
        // 上游在 params.system 缺省时会注入自身约 7.5K token 的默认提示词
        // （进入默认上下文/前缀路径），既产生大量 cached tokens 又污染对话。
        // 发一个空格占位即可绕过。
        params.insert("system".into(), json!(" "));
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
        // OpenAI 语义 → CC 语义：required 对应 any；指定函数对应 tool + name
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

/// 冲刷累积中的 assistant 消息：清理空字段后（无内容则丢弃）入队。
fn flush_pending(pending: &mut Option<Value>, messages: &mut Vec<Value>) {
    if let Some(mut p) = pending.take() {
        // 空字段不保留，避免 CC 校验拒绝
        let tool_calls_empty = p
            .get("tool_calls")
            .and_then(|t| t.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true);
        if tool_calls_empty {
            p.as_object_mut().map(|o| o.remove("tool_calls"));
        }
        let reasoning_empty = p
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .map(|s| s.is_empty())
            .unwrap_or(true);
        if reasoning_empty {
            p.as_object_mut().map(|o| o.remove("reasoning_content"));
        }
        let keep = p.get("content").map(|c| !c.is_null()).unwrap_or(false)
            || p.get("tool_calls").is_some()
            || p.get("reasoning_content").is_some();
        if keep {
            messages.push(p);
        }
    }
}

/// OpenAI Responses 请求 → Chat Completions 请求（供 build_cc_request 复用）。
/// 支持 Codex CLI 等客户端：input 字符串/条目数组、instructions、reasoning 回灌、
/// function_call 回灌等。Responses 把 reasoning / message / function_call 拆成并列
/// item，Chat 要求它们挂在同一条 assistant 消息上，故用 pending 累积再冲刷。
pub fn convert_responses_to_openai(resp: &Value) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(instructions) = resp.get("instructions").and_then(|v| v.as_str()) {
        if !instructions.is_empty() {
            messages.push(json!({ "role": "system", "content": instructions }));
        }
    }

    // 累积中的 assistant 消息：reasoning / message(assistant) / function_call 都并入它
    let mut pending: Option<Value> = None;
    let reasoning_of = |item: &Value| -> String {
        if let Some(arr) = item.get("summary").and_then(|v| v.as_array()) {
            let t = arr
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("");
            if !t.is_empty() {
                return t;
            }
        }
        if let Some(arr) = item.get("content").and_then(|v| v.as_array()) {
            let t = arr
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("");
            if !t.is_empty() {
                return t;
            }
        }
        item.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string()
    };

    match resp.get("input") {
        Some(Value::String(s)) => messages.push(json!({ "role": "user", "content": s })),
        Some(Value::Array(items)) => {
            for item in items {
                if let Value::String(s) = item {
                    messages.push(json!({ "role": "user", "content": s }));
                    continue;
                }
                match item.get("type").and_then(|v| v.as_str()) {
                    // reasoning 条目不回灌上游：转成 reasoning_content 并入 assistant
                    Some("reasoning") => {
                        let t = reasoning_of(item);
                        if !t.is_empty() {
                            let p = pending.get_or_insert_with(|| json!({ "role": "assistant", "content": Value::Null }));
                            p["reasoning_content"] = json!(t);
                        }
                    }
                    Some("function_call") => {
                        let p = pending.get_or_insert_with(|| json!({ "role": "assistant", "content": Value::Null, "tool_calls": [] }));
                        let tcs = p["tool_calls"].as_array_mut().expect("tool_calls is array");
                        tcs.push(json!({
                            "id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or(""),
                            "type": "function",
                            "function": {
                                "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                                "arguments": item.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}"),
                            },
                        }));
                    }
                    Some("function_call_output") => {
                        flush_pending(&mut pending, &mut messages);
                        let output = match item.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(Value::Array(arr)) => arr
                                .iter()
                                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join(""),
                            Some(v) => serde_json::to_string(v).unwrap_or_default(),
                            None => String::new(),
                        };
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or(""),
                            "content": output,
                        }));
                    }
                    _ => {
                        let role = match item.get("role").and_then(|v| v.as_str()).unwrap_or("user") {
                            "assistant" => "assistant",
                            "system" | "developer" => "system",
                            _ => "user",
                        };
                        let content = item.get("content");
                        if role == "assistant" {
                            // assistant 消息累积进 pending（文本、reasoning 与 function_call 合并）
                            let text = content
                                .map(|c| match c {
                                    Value::String(s) => s.clone(),
                                    Value::Array(parts) => parts
                                        .iter()
                                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                                        .collect::<Vec<_>>()
                                        .join(""),
                                    _ => String::new(),
                                })
                                .unwrap_or_default();
                            let p = pending.get_or_insert_with(|| json!({ "role": "assistant", "content": Value::Null }));
                            if !text.is_empty() {
                                p["content"] = json!(text);
                            }
                        } else {
                            flush_pending(&mut pending, &mut messages);
                            // user/system：保留 content 数组结构（text/image），字符串原样
                            let msg = match content {
                                Some(Value::String(s)) => json!({ "role": role, "content": s }),
                                Some(Value::Array(parts)) => {
                                    let mapped: Vec<Value> = parts
                                        .iter()
                                        .filter_map(|p| {
                                            match p.get("type").and_then(|t| t.as_str()) {
                                                Some("input_text") | Some("output_text") | Some("text") => Some(json!({
                                                    "type": "text",
                                                    "text": p.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                                                })),
                                                Some("input_image") => Some(json!({
                                                    "type": "image_url",
                                                    "image_url": { "url": p.get("image_url").and_then(|v| v.as_str()).unwrap_or("") },
                                                })),
                                                _ => None,
                                            }
                                        })
                                        .collect();
                                    if mapped.is_empty() {
                                        continue;
                                    }
                                    json!({ "role": role, "content": mapped })
                                }
                                _ => continue,
                            };
                            messages.push(msg);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    flush_pending(&mut pending, &mut messages);

    let mut openai_req = json!({
        "model": resp.get("model").and_then(|v| v.as_str()).unwrap_or(DEFAULT_MODEL),
        "messages": messages,
        "stream": resp.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
    });
    if let Some(m) = resp.get("max_output_tokens").and_then(|v| v.as_u64()) {
        openai_req["max_tokens"] = json!(m);
    }
    if let Some(t) = resp.get("temperature") {
        openai_req["temperature"] = t.clone();
    }
    if let Some(effort) = resp.pointer("/reasoning/effort").cloned() {
        openai_req["reasoning_effort"] = effort;
    } else if let Some(r) = resp.get("reasoning_effort") {
        openai_req["reasoning_effort"] = r.clone();
    }
    // Responses 工具为扁平结构，且可能混有内置工具（web_search 等），仅保留 function
    if let Some(tools) = resp.get("tools").and_then(|v| v.as_array()) {
        let mapped: Vec<Value> = tools
            .iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("function"))
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "description": t.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                        "parameters": t.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                    },
                })
            })
            .collect();
        if !mapped.is_empty() {
            openai_req["tools"] = Value::Array(mapped);
        }
    }
    if let Some(tc) = resp.get("tool_choice") {
        let mapped = match tc {
            Value::String(s) if matches!(s.as_str(), "auto" | "none" | "required") => json!(s),
            Value::Object(o) if o.get("type").and_then(|v| v.as_str()) == Some("function") => {
                json!({ "type": "function", "function": { "name": o.get("name").and_then(|v| v.as_str()).unwrap_or("") } })
            }
            _ => json!("auto"),
        };
        openai_req["tool_choice"] = mapped;
    }
    openai_req
}

/// CC 完成结果 → OpenAI Responses 非流式响应体。
///
/// - `thinking_text`：推理内容，非空时作为首个 output 条目输出 `reasoning` 类型；
/// - `tool_calls`：可选的工具调用列表（OpenAI Chat 格式的 tool_call 对象）；
/// - `finish_reason == "length"` 时 status 置 incomplete 并附 max_output_tokens 原因。
pub fn build_responses_response(
    response_id: &str,
    model: &str,
    full_text: &str,
    thinking_text: &str,
    tool_calls: Option<&[Value]>,
    finish_reason: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> Value {
    let short_id = || uuid::Uuid::new_v4().to_string()[..12].to_string();
    let mut output: Vec<Value> = Vec::new();
    if !thinking_text.is_empty() {
        output.push(json!({
            "type": "reasoning",
            "id": format!("rs_{}", short_id()),
            "status": "completed",
            "summary": [{ "type": "summary_text", "text": thinking_text }],
        }));
    }
    if !full_text.is_empty() {
        output.push(json!({
            "type": "message",
            "id": format!("msg_{}", short_id()),
            "role": "assistant",
            "status": "completed",
            "content": [{ "type": "output_text", "text": full_text, "annotations": [] }],
        }));
    }
    if let Some(tcs) = tool_calls {
        for tc in tcs {
            output.push(json!({
                "type": "function_call",
                "id": format!("fc_{}", short_id()),
                "call_id": tc.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "name": tc.pointer("/function/name").and_then(|v| v.as_str()).unwrap_or(""),
                "arguments": tc.pointer("/function/arguments").and_then(|v| v.as_str()).unwrap_or("{}"),
                "status": "completed",
            }));
        }
    }
    let incomplete = finish_reason == "length";
    let mut body = json!({
        "id": response_id,
        "object": "response",
        "created_at": now_secs(),
        "status": if incomplete { "incomplete" } else { "completed" },
        "error": Value::Null,
        "model": model,
        "output": output,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
            "input_tokens_details": { "cached_tokens": cached_tokens },
            "output_tokens_details": { "reasoning_tokens": 0 },
        },
    });
    if incomplete {
        body["incomplete_details"] = json!({ "reason": "max_output_tokens" });
    } else {
        body["incomplete_details"] = Value::Null;
    }
    body
}

/// Anthropic Messages 请求 → OpenAI Chat Completions 请求（供 build_cc_request 复用）。
///
/// 主要转换：system 字符串或 text 块数组归并为 system 消息；assistant 的 text/tool_use
/// 块合并为 content + tool_calls；user 的 tool_result 块拆为独立的 role=tool 消息
/// （工具名通过 tool_use_id 反查）；tools/tool_choice/thinking 等参数按对应语义映射，
/// 其中 thinking 的 budget_tokens 阈值（≥10000 高 / ≥5000 中 / 其余低）折算为 reasoning_effort。
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
                let mut thinking_content = String::new();
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
                        // Anthropic 的 thinking 块承载思考内容，转成 reasoning_content
                        // 交给 build_cc_request 回传，否则 CC 会因缺少 reasoning 而拒绝
                        Some("thinking") => {
                            if let Some(t) = block.get("thinking").and_then(|t| t.as_str()) {
                                thinking_content.push_str(t);
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
                if !thinking_content.is_empty() {
                    assistant_msg["reasoning_content"] = json!(thinking_content);
                }
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

/// CC finishReason → OpenAI finish_reason（tool-calls 归一为 tool_calls，空值视为 stop，未知透传）。
pub fn map_finish_reason(reason: &str) -> String {
    match reason {
        "tool-calls" => "tool_calls".into(),
        "length" => "length".into(),
        "stop" => "stop".into(),
        other if other.is_empty() => "stop".into(),
        other => other.to_string(),
    }
}

/// OpenAI finish_reason → Anthropic stop_reason（tool_use / max_tokens / end_turn，未知一律 end_turn）。
pub fn map_anthropic_stop_reason(reason: &str) -> &'static str {
    match reason {
        "tool_calls" => "tool_use",
        "length" => "max_tokens",
        "stop" => "end_turn",
        _ => "end_turn",
    }
}

/// 非流式 OpenAI 响应构建。
///
/// - `reasoning_content`：非空时以扩展字段写入 assistant 消息（deepseek 风格）；
/// - `tool_calls`：已是 OpenAI tool_call 格式则原样挂到 message.tool_calls；
/// - `full_text` 为空时 content 置 null（纯工具调用响应）。
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
///
/// - `thinking_text`：推理内容，非空时作为首个 content 块输出 `thinking` 类型（附假签名）；
/// - tool_call 的 arguments 字符串会尽力解析为 JSON 对象写入 tool_use.input；
/// - `finish_reason` 经 map_anthropic_stop_reason 折算为 Anthropic 的 stop_reason。
pub fn build_anthropic_response(
    message_id: &str,
    model: &str,
    full_text: &str,
    thinking_text: &str,
    tool_calls: Option<&[Value]>,
    finish_reason: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: Option<u64>,
) -> Value {
    let mut content: Vec<Value> = Vec::new();
    if !thinking_text.is_empty() {
        content.push(json!({
            "type": "thinking",
            "thinking": thinking_text,
            "signature": crate::proxy::sse::fake_thinking_signature(thinking_text),
        }));
    }
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
    // 上游未回报 output token 时按内容长度估算，避免客户端展示/记账为 0
    let tool_count = tool_calls.map(|t| t.len()).unwrap_or(0);
    let est_out = ((full_text.chars().count() + thinking_text.chars().count()) / 4) as u64
        + (tool_count as u64) * 20;
    let output_tokens = if output_tokens == 0 && est_out > 0 {
        est_out
    } else {
        output_tokens
    };
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
