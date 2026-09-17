use serde_json::{json, Map, Value};

use super::fingerprint::DeviceProfile;
use super::state::now_secs;

/// 请求未指定 model 时使用的默认模型。
pub const DEFAULT_MODEL: &str = "deepseek/deepseek-v4-flash";
/// 上游 max_tokens 上限（超过则截断）。
const MAX_TOKENS_CAP: u64 = 200_000;
/// 请求未指定 max_tokens 时的默认值。
const DEFAULT_MAX_TOKENS: u64 = 64_000;

/// 尽力解析 JSON 字符串，失败时返回空对象 `{}`（用于 tool_call 的 arguments）。
fn try_parse_json(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| json!({}))
}

/// CLI 发送前会重写部分工具名（resolveToolNameAlias / ow 表）。
const TOOL_NAME_ALIASES: &[(&str, &str)] = &[
    ("bash_output", "shell_output"),
    ("task_output", "shell_output"),
    ("tool_search", "search_tools"),
    ("read_multiple_files", "read_file"),
];

/// 按 CLI 的别名表重写工具名；未收录的工具名原样透传。
fn to_wire_tool_name(name: &str) -> String {
    TOOL_NAME_ALIASES
        .iter()
        .find(|(from, _)| *from == name)
        .map(|(_, to)| (*to).to_string())
        .unwrap_or_else(|| name.to_string())
}

/// 取 JSON 值的字符串内容，非字符串或缺失时返回空串。
fn as_str_or_empty(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// OpenAI Chat Completions 请求 → Command Code 请求体（CLI 信封格式）。
///
/// 主要转换：system/developer 消息提取为 params.system 块数组（对齐 CLI 的 toWireSystem，
/// 非末块补 \n、cache_control 逐块保留；为空且开关开启时发空格占位，阻止上游注入默认提示词）；
/// user/assistant/tool 消息转为 Command Code 的 content parts 结构（text/image/tool-call/tool-result）；
/// assistant 的 reasoning_content 与 content 内 reasoning part 回传为 `{type:"reasoning"}`；
/// tools 恒下发（无工具时为空数组，对齐 CLI）且去 type、工具名按别名表重写；
/// tool_choice 的 required 映射为 any；max_tokens 缺省 64000 并封顶 200000；stream 恒为 true。
///
/// - `empty_system_placeholder`：无 system 时是否发 `" "` 占位，防止 Command Code 上游注入
///   约 7.5K token 的默认提示词；
/// - `profile`：设备档案（workingDir / environment 与指纹同源，避免自相矛盾）；
/// - `cli_mode`：信封 mode（agent | learning | …，独立于 lifecycle 的 cli_session_mode）。
pub fn build_cc_request(
    openai_req: &Value,
    empty_system_placeholder: bool,
    profile: &DeviceProfile,
    cli_mode: &str,
) -> Value {
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

    // system/developer 消息提取为块数组：CLI 的 toWireSystem 形态（非末块补 \n，
    // cache_control 逐块保留）。旧注释「数组会被上游拒绝」来自更早协议版本，
    // 已被 command-code@1.53.1 源码推翻 —— 块数组是 CLI 原生形态。
    let is_system_role =
        |m: &&Value| matches!(m.get("role").and_then(|r| r.as_str()), Some("system") | Some("developer"));
    let system_msgs: Vec<&Value> = messages.iter().filter(is_system_role).collect();
    let mut system_blocks: Vec<Value> = Vec::new();
    for m in &system_msgs {
        match m.get("content") {
            Some(Value::String(s)) if !s.is_empty() => {
                system_blocks.push(json!({ "type": "text", "text": s }));
            }
            Some(Value::Array(parts)) => {
                for part in parts {
                    let text = part
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string();
                    if text.is_empty() && part.get("cache_control").is_none() {
                        continue;
                    }
                    let mut block = json!({ "type": "text", "text": text });
                    if let Some(cc) = part.get("cache_control") {
                        block["cache_control"] = cc.clone();
                    }
                    system_blocks.push(block);
                }
            }
            // 空字符串已在上面的 String 分支被跳过；此处仅兜非字符串类型
            // （否则 Value::String("") 会被 to_string 序列化成两个字面引号的文本块）
            Some(other) if !other.is_null() && !other.is_string() => {
                system_blocks.push(json!({ "type": "text", "text": other.to_string() }));
            }
            _ => {}
        }
    }
    // 非最后一块补 \n（CLI 的 toWireSystem 行为）
    for i in 0..system_blocks.len().saturating_sub(1) {
        if let Some(t) = system_blocks[i].get_mut("text") {
            if let Some(s) = t.as_str() {
                *t = json!(format!("{s}\n"));
            }
        }
    }
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
                                    // CC CLI 真实格式: { type: "image", image: "data:<mime>;base64,...", mimeType: "<mime>" }
                                    let mut image_part = json!({ "type": "image", "image": url });
                                    if let Some(media_type) = url
                                        .strip_prefix("data:")
                                        .and_then(|rest| rest.split(';').next())
                                        .filter(|m| !m.is_empty())
                                    {
                                        image_part["mimeType"] = json!(media_type);
                                    }
                                    image_part
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
                // 思考内容必须回传：Command Code 在 thinking 模式下校验 reasoning 是否随历史带回，
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
                    // CLI 的 toWireToolOutput：只取文本块，用 '\n' 拼接（不 JSON 序列化）
                    Some(Value::Array(arr)) => arr
                        .iter()
                        .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n"),
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

    // prompt_cache_key：缓存按前缀计算，system 正是最前的那段前缀，故把断点落在
    // system 最后一块（块数组是 CLI 的原生形态，对应 systemSections[].cache）。
    // 客户端已在任意消息块 / system 块上打过断点就保留；否则若给了 OpenAI 系的
    // prompt_cache_key，在 system 末块补 ephemeral 断点。
    let has_cache_marker = system_blocks.iter().any(|b| b.get("cache_control").is_some())
        || cc_messages.iter().any(|m| {
            m.get("content")
                .and_then(|c| c.as_array())
                .map(|parts| parts.iter().any(|p| p.get("cache_control").is_some()))
                .unwrap_or(false)
        });
    if let Some(cache_key) = openai_req.get("prompt_cache_key").and_then(|v| v.as_str()) {
        if !cache_key.is_empty() && !has_cache_marker {
            if let Some(last) = system_blocks.last_mut() {
                last["cache_control"] = json!({ "type": "ephemeral" });
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
            // 伪造的项目目录（不再发宿主真实 cwd）；environment 用伪装的平台词，与指纹保持自洽
            "workingDir": profile.project_dir,
            "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "environment": profile.platform,
            "structure": [],
            "isGitRepo": false,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": [],
        },
        "memory": Value::Null,
        "taste": Value::Null,
        "skills": Value::Null, // CLI 发 null，不是空串
        "permissionMode": "standard",
        // 信封 mode（独立于 lifecycle metadata 的 cli_session_mode）
        "mode": cli_mode,
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

    if !system_blocks.is_empty() {
        params.insert("system".into(), Value::Array(system_blocks));
    } else if empty_system_placeholder {
        // 上游在 params.system 缺省时会注入自身约 7.5K token 的默认提示词
        // （进入默认上下文/前缀路径），既产生大量 cached tokens 又污染对话。
        // 发一个非空占位即可绕过注入。
        //
        // 占位内容必须是「非空白字符」：部分 provider（moonshotai/Kimi-K2.5、
        // zai-org/GLM-5、MiniMaxAI/MiniMax-M2.5 等）会校验并拒绝全空白 system
        // （报 "The system field can't be blank"），导致这些模型整体不可用。
        // 句点既能通过所有 provider 的校验，也同样是极短的显式 system。
        params.insert("system".into(), json!([{ "type": "text", "text": "." }]));
    }
    if let Some(t) = openai_req.get("temperature") {
        params.insert("temperature".into(), t.clone());
    }
    if let Some(r) = openai_req.get("reasoning_effort") {
        params.insert("reasoning_effort".into(), r.clone());
    }
    // CLI 总是下发 tools（没有工具时是空数组）—— 空数组与缺键在 wire 上可观测，这里对齐。
    // CLI 的 toWireTools：只有 name / description / input_schema，没有 type 字段。
    let tools = openai_req
        .get("tools")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mapped: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": to_wire_tool_name(t.pointer("/function/name").and_then(|v| v.as_str()).unwrap_or("")),
                "description": t.pointer("/function/description").and_then(|v| v.as_str()).unwrap_or(""),
                "input_schema": t.pointer("/function/parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            })
        })
        .collect();
    params.insert("tools".into(), Value::Array(mapped));
    if let Some(tc) = openai_req.get("tool_choice") {
        // OpenAI 语义 → Command Code 语义：required 对应 any；指定函数对应 tool + name
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
    // 采样与停止序列：Anthropic 的 top_p / stop_sequences 经上游归一为 OpenAI 命名后
    // 必须在此转发，否则客户端设置的停止序列会被静默忽略、生成不会在预期处停止
    if let Some(p) = openai_req.get("top_p") {
        params.insert("top_p".into(), p.clone());
    }
    if let Some(s) = openai_req.get("stop") {
        params.insert("stop".into(), s.clone());
    }

    body
}

/// 冲刷累积中的 assistant 消息：清理空字段后（无内容则丢弃）入队。
fn flush_pending(pending: &mut Option<Value>, messages: &mut Vec<Value>) {
    if let Some(mut p) = pending.take() {
        // 空字段不保留，避免 Command Code 校验拒绝
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
                        // reasoning 分支可能已先创建了 pending（其中不含 tool_calls）：
                        // 这里必须补建 tool_calls 数组再追加，避免对 Null 取数组而 panic
                        let p = pending.get_or_insert_with(|| json!({ "role": "assistant", "content": Value::Null }));
                        if !p.get("tool_calls").map(|v| v.is_array()).unwrap_or(false) {
                            p["tool_calls"] = json!([]);
                        }
                        if let Some(tcs) = p["tool_calls"].as_array_mut() {
                            tcs.push(json!({
                                "id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or(""),
                                "type": "function",
                                "function": {
                                    "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                                    "arguments": item.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}"),
                                },
                            }));
                        }
                    }
                    Some("function_call_output") => {
                        flush_pending(&mut pending, &mut messages);
                        let output = match item.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(Value::Array(arr)) => arr
                                .iter()
                                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Some(v) => serde_json::to_string(v).unwrap_or_default(),
                            None => String::new(),
                        };
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or(""),
                            "content": output,
                        }));
                    }
                    // OpenAI 规范里 input 数组的联合类型第一个成员是 EasyInputMessage，它的
                    // required 只有 role 与 content —— type 是可选的（SDK 示例普遍写作
                    // { role: 'user', content: 'hi' }）。type 缺失但有 role 时按 message
                    // 处理（落进下面的默认分支靠 role 兜底），否则这类 item 会被静默丢弃。
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

/// Command Code 完成结果 → OpenAI Responses 非流式响应体。
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

/// Anthropic 图片块 → OpenAI `image_url.url` 字符串。
///
/// `base64` 源拼成 `data:<media_type>;base64,<data>`（上游据此还原图片），
/// `url` 源原样透传；缺字段或未知 source 类型返回 None（调用方记日志）。
fn anthropic_image_to_url(block: &Value) -> Option<String> {
    let source = block.get("source")?;
    match source.get("type").and_then(|t| t.as_str()) {
        Some("base64") => {
            let media_type = source.get("media_type").and_then(|v| v.as_str())?;
            let data = source.get("data").and_then(|v| v.as_str())?;
            Some(format!("data:{media_type};base64,{data}"))
        }
        Some("url") => source.get("url").and_then(|v| v.as_str()).map(str::to_string),
        _ => None,
    }
}

/// Anthropic Messages 请求 → OpenAI Chat Completions 请求（供 build_cc_request 复用）。
///
/// 主要转换：system 字符串或 text 块数组归并为 system 消息；assistant 的 text/tool_use
/// 块合并为 content + tool_calls；user 的 text/image 块转为 content（图片转 image_url）、
/// tool_result 块拆为独立的 role=tool 消息（工具名通过 tool_use_id 反查）；
/// tools/tool_choice/thinking 等参数按对应语义映射，
/// 其中 thinking 的 budget_tokens 阈值（≥10000 高 / ≥5000 中 / 其余低）折算为 reasoning_effort。
pub fn convert_anthropic_to_openai(anthropic_req: &Value) -> Value {
    // 1. system
    let mut system_prompt = String::new();
    let mut system_blocks: Option<Vec<Value>> = None;
    if let Some(sys) = anthropic_req.get("system") {
        match sys {
            Value::String(s) => system_prompt = s.clone(),
            Value::Array(arr) => {
                // 保留 cache_control：build_cc_request 需要块数组才能把断点下发
                // （CLI 的 params.system 就是块数组）
                let blocks: Vec<Value> = arr
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .map(|b| {
                        let mut blk = json!({ "type": "text", "text": b.get("text").and_then(|t| t.as_str()).unwrap_or("") });
                        if let Some(cc) = b.get("cache_control") {
                            blk["cache_control"] = cc.clone();
                        }
                        blk
                    })
                    .collect();
                if !blocks.is_empty() {
                    system_blocks = Some(blocks.clone());
                    system_prompt = blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
            _ => {}
        }
    }

    // 2. messages
    let mut tool_name_from_id: Map<String, Value> = Map::new();
    let mut openai_messages: Vec<Value> = Vec::new();
    if !system_prompt.is_empty() {
        openai_messages.push(json!({
            "role": "system",
            "content": system_blocks.unwrap_or_else(|| vec![json!({ "type": "text", "text": system_prompt })]),
        }));
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
                // 多块 text 保留为块数组（含 cache_control），与 CLI 的 toWireMessages 一致
                let mut text_parts: Vec<Value> = Vec::new();
                let mut text_has_cache = false;
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
                            let mut part = json!({
                                "type": "text",
                                "text": block.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                            });
                            if block.get("cache_control").is_some() {
                                part["cache_control"] = block["cache_control"].clone();
                                text_has_cache = true;
                            }
                            text_parts.push(part);
                        }
                        // Anthropic 的 thinking 块承载思考内容，转成 reasoning_content
                        // 交给 build_cc_request 回传，否则 Command Code 会因缺少 reasoning 而拒绝
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
                // 多块 text / 带断点用块数组（保留 cache_control）；单块纯文本沿用字符串形态
                let assistant_content = if text_parts.len() > 1 || text_has_cache {
                    Value::Array(text_parts)
                } else if text_content.is_empty() {
                    Value::Null
                } else {
                    json!(text_content)
                };
                let mut assistant_msg = json!({
                    "role": "assistant",
                    "content": assistant_content,
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
                // parts 保持原始顺序（text / image_url），与 CLI 的 toWireMessages 一致
                let mut parts: Vec<Value> = Vec::new();
                let mut text_has_cache = false;
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
                                    let mut part = json!({
                                        "type": "text",
                                        "text": block.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                                    });
                                    if block.get("cache_control").is_some() {
                                        part["cache_control"] = block["cache_control"].clone();
                                        text_has_cache = true;
                                    }
                                    parts.push(part);
                                }
                                // Anthropic 图片块（base64 / url）转 OpenAI image_url：
                                // 不转换的话客户端粘贴的截图会被静默丢弃，模型只能看到文本
                                Some("image") => match anthropic_image_to_url(block) {
                                    Some(url) => parts
                                        .push(json!({ "type": "image_url", "image_url": { "url": url } })),
                                    None => super::log::warn(crate::i18n::pick(
                                        "Anthropic 图片块缺少 source/media_type/data，已忽略",
                                        "Anthropic image block missing source/media_type/data; skipped",
                                    )),
                                },
                                Some("tool_result") => tool_results.push(block.clone()),
                                // document 等暂不支持的类型记日志，避免静默丢失用户输入
                                Some(other) => super::log::warn(&format!(
                                    "{}: {other}",
                                    crate::i18n::pick(
                                        "暂不支持的 Anthropic 内容块类型，已忽略",
                                        "Unsupported Anthropic content block type; skipped"
                                    )
                                )),
                                None => {}
                            }
                        }
                    }
                    _ => {}
                }
                // 先推入 tool_result 对应的 tool 消息，再推入该回合的文本/图片：
                // OpenAI 语义要求 role:"tool" 紧随带 tool_calls 的 assistant 消息之后，
                // 若先推 user 文本会把工具结果与其发起消息隔开。
                for tr in &tool_results {
                    let tool_use_id = as_str_or_empty(tr.get("tool_use_id").unwrap_or(&Value::Null));
                    let content = match tr.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        // 对齐 CLI：tool_result 的文本块用 '\n' 拼接（旧版为 join("")）
                        Some(Value::Array(arr)) => arr
                            .iter()
                            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        other => other.map(|v| v.to_string()).unwrap_or_default(),
                    };
                    openai_messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "name": tool_name_from_id.get(&tool_use_id).and_then(|v| v.as_str()).unwrap_or(""),
                        "content": content,
                    }));
                }
                // 注意：content 为字符串时 parts 为空，必须用 textContent 判空（否则整条消息会丢）
                if !parts.is_empty() || !text_content.is_empty() {
                    // 单块纯文本仍用字符串（线格不变）；多块 / 带断点 / 含图片时用块数组（CLI 的形态）
                    let single_text = parts.len() <= 1
                        && (parts.is_empty() || parts[0].get("type").and_then(|t| t.as_str()) == Some("text"))
                        && !text_has_cache;
                    openai_messages.push(json!({
                        "role": "user",
                        "content": if single_text { json!(text_content) } else { Value::Array(parts.clone()) },
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

/// Command Code finishReason → OpenAI finish_reason（tool-calls 归一为 tool_calls，空值视为 stop，未知透传）。
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
///
/// 同时接受上游原始的 `tool-calls` 写法：调用方通常已用 `map_finish_reason` 归一，
/// 但流式路径直接传入原始值时若漏归一，这里仍能正确映射为 tool_use。
pub fn map_anthropic_stop_reason(reason: &str) -> &'static str {
    match reason {
        "tool_calls" | "tool-calls" => "tool_use",
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
