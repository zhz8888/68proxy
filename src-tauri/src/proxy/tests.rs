//! proxy 模块的单元与集成测试。
//!
//! 单元测试覆盖：三种协议的请求转换、SSE/流事件翻译、错误映射、Key 提取、
//! 设备指纹与项目 slug 生成；集成测试用 axum 搭建 mock Command Code 上游，端到端验证
//! 代理服务的流式/非流式转发、零输出限流、上游错误映射与鉴权行为。

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::cc_client;
use super::config::Config;
use super::convert;
use super::errors;
use super::fingerprint;
use super::server;
use super::sse::{AnthropicTranslator, OpenAiTranslator, ResponsesTranslator};
use super::state::AppState;

/// 集成测试固定使用的本地转发 Key（与 start_proxy_impl 注入的设置库一致）。
const TEST_LOCAL_KEY: &str = "sk-test-local-key-123";

/// 测试辅助：用默认设备档案与默认 cli_mode 调 build_cc_request（转换逻辑测试不关心信封伪装）。
fn build_cc(req: &Value, placeholder: bool) -> Value {
    let profile = fingerprint::default_device_profile("");
    convert::build_cc_request(req, placeholder, &profile, "agent")
}

// ── 单元测试：请求转换 ────────────────────────────────

/// 验证基础 OpenAI 请求转换出的 Command Code 信封：model/system 提取、user 消息转 text parts、
/// max_tokens 透传、permissionMode 固定 standard、config.date 为字符串。
#[test]
fn build_cc_request_basic_envelope() {
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [
            { "role": "system", "content": "你是助手" },
            { "role": "user", "content": "你好" },
        ],
        "max_tokens": 1000,
        "stream": true,
    });
    let cc = build_cc(&req, true);
    assert_eq!(cc["params"]["model"], "deepseek/deepseek-v4-flash");
    // system 对齐 CLI 的 toWireSystem：块数组形态
    assert_eq!(cc["params"]["system"][0]["text"], "你是助手");
    assert_eq!(cc["params"]["messages"][0]["role"], "user");
    assert_eq!(cc["params"]["messages"][0]["content"][0]["type"], "text");
    assert_eq!(cc["params"]["max_tokens"], 1000);
    assert_eq!(cc["permissionMode"], "standard");
    // CLI 总是下发 tools（无工具时是空数组）
    assert_eq!(cc["params"]["tools"], json!([]));
    assert!(cc["config"]["date"].is_string());
}

/// 验证 developer 与 system 两种角色的消息按序合并为顶层 system 字段，
/// 且均不会作为聊天消息原样转发（Command Code API 会拒绝未知角色）。
#[test]
fn build_cc_request_developer_role_merged_into_system() {
    // OpenAI 新客户端以 role: "developer" 发送 system prompt，
    // 需与 system 一并提取为顶层 system，不能原样转发（Command Code API 会报 400）
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [
            { "role": "developer", "content": "系统指令" },
            { "role": "system", "content": "补充说明" },
            { "role": "user", "content": "你好" },
        ],
    });
    let cc = build_cc(&req, true);
    // system 块数组：developer + system 两块，非末块补 \n
    assert_eq!(cc["params"]["system"][0]["text"], "系统指令\n");
    assert_eq!(cc["params"]["system"][1]["text"], "补充说明");
    let msgs = cc["params"]["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
    assert!(!msgs.iter().any(|m| m["role"] == "developer" || m["role"] == "system"));
}

/// 验证多模态与工具场景的转换：image_url 转 image part、assistant tool_calls 转
/// tool-call（arguments 解析为 JSON 对象）、tool 消息转 tool-result 并反查工具名、
/// tool_choice=required 映射为 any、tools 扁平化并携带 input_schema。
#[test]
fn build_cc_request_image_and_tools() {
    let req = json!({
        "model": "xiaomi/mimo-v2.5",
        "messages": [
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "看图" },
                    { "type": "image_url", "image_url": { "url": "data:image/jpeg;base64,xxx" } },
                ],
            },
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "get_weather", "arguments": "{\"city\":\"sh\"}" } }],
            },
            { "role": "tool", "tool_call_id": "call_1", "content": "25°C" },
        ],
        "tools": [{ "type": "function", "function": { "name": "get_weather", "description": "天气", "parameters": { "type": "object" } } }],
        "tool_choice": "required",
    });
    let cc = build_cc(&req, true);
    let user_content = &cc["params"]["messages"][0]["content"];
    assert_eq!(user_content[0]["type"], "text");
    assert_eq!(user_content[1]["type"], "image");
    assert_eq!(user_content[1]["image"], "data:image/jpeg;base64,xxx");
    let assistant = &cc["params"]["messages"][1];
    assert_eq!(assistant["content"][0]["type"], "tool-call");
    assert_eq!(assistant["content"][0]["toolName"], "get_weather");
    assert_eq!(assistant["content"][0]["input"]["city"], "sh");
    let tool = &cc["params"]["messages"][2];
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["content"][0]["toolName"], "get_weather");
    assert_eq!(tool["content"][0]["output"]["value"], "25°C");
    assert_eq!(cc["params"]["tool_choice"]["type"], "any");
    assert_eq!(cc["params"]["tools"][0]["name"], "get_weather");
    assert_eq!(cc["params"]["tools"][0]["input_schema"]["type"], "object");
    // CLI 的 toWireTools：无 type 字段（旧版带 type 已废弃）
    assert!(cc["params"]["tools"][0].get("type").is_none());
}

/// 验证工具名别名映射（CLI 的 ow 表）与 tool 输出文本块 \n 拼接。
#[test]
fn build_cc_request_tool_alias_and_output_join() {
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [
            { "role": "user", "content": "查一下" },
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "bash_output", "arguments": "{}" } }],
            },
            {
                "role": "tool",
                "tool_call_id": "call_1",
                "content": [
                    { "type": "text", "text": "第一行" },
                    { "type": "text", "text": "第二行" },
                ],
            },
        ],
        "tools": [{ "type": "function", "function": { "name": "bash_output" } }],
    });
    let cc = build_cc(&req, true);
    // 别名映射：bash_output → shell_output
    assert_eq!(cc["params"]["tools"][0]["name"], "shell_output");
    // tool 输出：文本块用 \n 拼接（CLI 的 toWireToolOutput）
    assert_eq!(cc["params"]["messages"][2]["content"][0]["output"]["value"], "第一行\n第二行");
}

/// reasoning_effort 白名单：仅上游枚举内的档位放行，非法值丢弃且不下发该字段。
///
/// 回归：客户端传生态里的其它写法（none / default / minimal / 数字等）时，
/// 原实现原样透传给上游，上游以
/// `Invalid option: expected one of "low"|"medium"|"high"|"xhigh"|"max" at "params.reasoning_effort"`
/// 拒绝整个请求（deepseek/deepseek-v4-flash 等报「连接失败」即由此而来）。
#[test]
fn reasoning_effort_is_validated_against_upstream_enum() {
    // 纯函数：合法档位（含大小写/空白差异）归一化，非法与空值返回 None
    for ok in ["low", "medium", "high", "xhigh", "max"] {
        assert_eq!(convert::normalize_reasoning_effort(ok), Some(ok));
    }
    assert_eq!(convert::normalize_reasoning_effort("HIGH"), Some("high"));
    assert_eq!(convert::normalize_reasoning_effort("  high  "), Some("high"));
    for bad in ["none", "default", "minimal", "", "very-high", "1"] {
        assert_eq!(convert::normalize_reasoning_effort(bad), None, "{bad} 应被拒绝");
    }

    // 端到端：非法档位不下发该字段，请求仍能正常构建
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [{ "role": "user", "content": "hi" }],
        "reasoning_effort": "none",
    });
    let cc = build_cc(&req, true);
    assert!(
        cc["params"].get("reasoning_effort").is_none(),
        "非法档位不应下发: {}",
        cc["params"]
    );
    // 合法档位照常下发
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [{ "role": "user", "content": "hi" }],
        "reasoning_effort": "high",
    });
    let cc = build_cc(&req, true);
    assert_eq!(cc["params"]["reasoning_effort"], "high");
}

/// Anthropic thinking.effort 同样受白名单约束（原实现直接透传，非法值会 400）。
///
/// 该折算发生在 Anthropic → OpenAI 转换中，故测试需先经 convert_anthropic_to_openai。
#[test]
fn anthropic_thinking_effort_is_validated() {
    // 非法 effort：不下发 reasoning_effort
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [{ "role": "user", "content": "hi" }],
        "max_tokens": 64,
        "thinking": { "type": "adaptive", "effort": "none" },
    });
    let openai = convert::convert_anthropic_to_openai(&req);
    let cc = build_cc(&openai, true);
    assert!(
        cc["params"].get("reasoning_effort").is_none(),
        "非法 thinking.effort 不应下发: {}",
        cc["params"]
    );
    // 合法 effort：归一化后下发
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [{ "role": "user", "content": "hi" }],
        "max_tokens": 64,
        "thinking": { "type": "adaptive", "effort": "max" },
    });
    let openai = convert::convert_anthropic_to_openai(&req);
    let cc = build_cc(&openai, true);
    assert_eq!(cc["params"]["reasoning_effort"], "max");
}

/// 验证无 system 时 params.system 发非空占位（开关开启），关闭时缺省字段。
#[test]
fn build_cc_request_empty_system_placeholder() {
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [{ "role": "user", "content": "你好" }],
    });
    // 开关开启：无 system 时发句点占位（块数组形态），阻止上游注入默认提示词。
    // 不能用空格：部分 provider（Kimi-K2.5 / GLM-5 / MiniMax-M2.5）会以
    // "The system field can't be blank" 拒绝全空白 system。
    let cc = build_cc(&req, true);
    assert_eq!(cc["params"]["system"][0]["text"], ".");
    // 开关关闭：不写 system 字段
    let cc2 = build_cc(&req, false);
    assert!(cc2["params"].get("system").is_none());
}

/// 验证 assistant 消息的 reasoning 回传：reasoning_content 字段与 content 数组内的
/// reasoning part 均转为 Command Code 的 `{type:"reasoning"}`，且顺序为 [reasoning, text, tool-call]。
#[test]
fn build_cc_request_assistant_reasoning() {
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [
            { "role": "user", "content": "思考后回答" },
            {
                "role": "assistant",
                "content": "结论",
                "reasoning_content": "我先想想",
                "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "f", "arguments": "{}" } }],
            },
        ],
    });
    let cc = build_cc(&req, true);
    let parts = cc["params"]["messages"][1]["content"].as_array().unwrap();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0]["type"], "reasoning");
    assert_eq!(parts[0]["text"], "我先想想");
    assert_eq!(parts[1]["type"], "text");
    assert_eq!(parts[1]["text"], "结论");
    assert_eq!(parts[2]["type"], "tool-call");

    // content 数组内直接携带 reasoning part 时同样透传
    let req2 = json!({
        "model": "m",
        "messages": [
            { "role": "user", "content": "hi" },
            { "role": "assistant", "content": [ { "type": "reasoning", "text": "思考中" }, { "type": "text", "text": "答复" } ] },
        ],
    });
    let cc2 = build_cc(&req2, true);
    let parts2 = cc2["params"]["messages"][1]["content"].as_array().unwrap();
    assert_eq!(parts2[0]["type"], "reasoning");
    assert_eq!(parts2[0]["text"], "思考中");
    assert_eq!(parts2[1]["type"], "text");
}

/// 验证 prompt_cache_key 在首个 user 消息最后一个 text 块注入 cache_control；
/// 消息已有 cache_control 标记时跳过注入。
#[test]
fn build_cc_request_prompt_cache_key() {
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "prompt_cache_key": "cache-abc-123456",
        "messages": [
            { "role": "system", "content": "sys" },
            { "role": "user", "content": [
                { "type": "text", "text": "开头" },
                { "type": "text", "text": "结尾" },
            ] },
        ],
    });
    let cc = build_cc(&req, true);
    // 缓存按前缀计算，system 是最前那段前缀：断点落在 system 末块（对齐 CLI 的 systemSections[].cache）
    let system = cc["params"]["system"].as_array().unwrap();
    assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
    // user 消息不被打断点
    let content = cc["params"]["messages"][0]["content"].as_array().unwrap();
    assert!(content.iter().all(|p| p.get("cache_control").is_none()));

    // 已有 cache_control 时不重复注入
    let req2 = json!({
        "model": "m",
        "prompt_cache_key": "cache-abc-123456",
        "messages": [
            { "role": "system", "content": "sys" },
            { "role": "user", "content": [
                { "type": "text", "text": "x", "cache_control": { "type": "ephemeral" } },
            ] },
        ],
    });
    let cc2 = build_cc(&req2, true);
    let system2 = cc2["params"]["system"].as_array().unwrap();
    assert!(system2.iter().all(|b| b.get("cache_control").is_none()), "已有断点时不再注入");
}

/// 验证 fingerprint 序列化为 camelCase（字段名与上游期望的格式一致）。
#[test]
fn fingerprint_camelcase_serialization() {
    let profile = fingerprint::default_device_profile("");
    let fp = fingerprint::generate("user_abc", "", &profile);
    let v = serde_json::to_value(&fp).unwrap();
    assert!(v["components"]["machineIdHash"].is_string());
    assert!(v["components"]["macHashes"].is_array());
    assert!(v["components"]["osUserHash"].is_string());
    assert!(v["components"]["hostnameHash"].is_string());
    assert!(v["components"]["gitEmailHash"].is_string());
    assert!(v["components"]["cpuModel"].is_string());
    assert!(v["components"]["memGiB"].is_number());
    assert!(v["components"]["osRelease"].is_string());
    assert!(v["components"]["collectorVersion"].is_number());
}

/// 验证 Anthropic 的 thinking 块转成 OpenAI reasoning_content（供 build_cc_request 回传）。
#[test]
fn anthropic_thinking_to_reasoning_content() {
    let req = json!({
        "model": "claude-sonnet-4-6",
        "messages": [
            { "role": "user", "content": "hi" },
            {
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "内部推理" },
                    { "type": "text", "text": "答复" },
                ],
            },
        ],
    });
    let openai = convert::convert_anthropic_to_openai(&req);
    let assistant = &openai["messages"][1];
    assert_eq!(assistant["reasoning_content"], "内部推理");
    assert_eq!(assistant["content"], "答复");
}

/// 验证 Anthropic 非流式响应把 thinking 文本输出为首个 thinking 内容块（附假签名），
/// 且上游未回报 output_tokens 时按内容长度估算。
#[test]
fn build_anthropic_response_with_thinking() {
    let body = convert::build_anthropic_response(
        "msg_1", "claude-sonnet-4-6", "答复", "思考内容", None, "stop", 10, 0, 0, None,
    );
    assert_eq!(body["content"][0]["type"], "thinking");
    assert_eq!(body["content"][0]["thinking"], "思考内容");
    assert!(body["content"][0]["signature"].as_str().unwrap().starts_with('E')
        || body["content"][0]["signature"].as_str().unwrap().starts_with('R'));
    assert_eq!(body["content"][1]["type"], "text");
    // 上游 output_tokens=0 → 按内容长度估算（(答复2+思考内容4)/4 = 1）
    assert!(body["usage"]["output_tokens"].as_u64().unwrap() > 0);
}

/// 验证 Responses 的 reasoning 条目回灌进 assistant 消息的 reasoning_content。
#[test]
fn responses_reasoning_to_assistant() {
    let req = json!({
        "model": "gpt-5-codex",
        "input": [
            { "type": "message", "role": "user", "content": "hi" },
            { "type": "reasoning", "id": "rs_1", "summary": [{ "type": "summary_text", "text": "思考过程" }] },
            { "type": "message", "role": "assistant", "content": "答复" },
        ],
    });
    let openai = convert::convert_responses_to_openai(&req);
    let msgs = openai["messages"].as_array().unwrap();
    let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(assistant["reasoning_content"], "思考过程");
    assert_eq!(assistant["content"], "答复");
}

/// 验证 Anthropic Messages 请求转 OpenAI Chat 格式：system 置顶、assistant 的
/// text/tool_use 块合并为 content+tool_calls、tool_result 拆为 role=tool 消息、
/// tool_choice any→required、thinking budget 12000 折算 reasoning_effort=high、
/// input_schema 映射为 function.parameters。
#[test]
fn anthropic_to_openai_conversion() {
    let req = json!({
        "model": "claude-sonnet-4-6",
        "system": "你很有帮助",
        "max_tokens": 8000,
        "stream": true,
        "tools": [{ "name": "t1", "description": "d", "input_schema": { "type": "object" } }],
        "tool_choice": { "type": "any" },
        "thinking": { "type": "enabled", "budget_tokens": 12000 },
        "messages": [
            { "role": "user", "content": "hi" },
            {
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "hello" },
                    { "type": "tool_use", "id": "tu_1", "name": "t1", "input": { "a": 1 } },
                ],
            },
            {
                "role": "user",
                "content": [
                    { "type": "tool_result", "tool_use_id": "tu_1", "content": "ok" },
                    { "type": "text", "text": "继续" },
                ],
            },
        ],
    });
    let openai = convert::convert_anthropic_to_openai(&req);
    assert_eq!(openai["model"], "claude-sonnet-4-6");
    assert_eq!(openai["messages"][0]["role"], "system");
    assert_eq!(openai["messages"][1]["role"], "user");
    assert_eq!(openai["messages"][2]["role"], "assistant");
    assert_eq!(openai["messages"][2]["tool_calls"][0]["function"]["name"], "t1");
    // tool 消息须紧随 assistant(tool_calls)，同回合的文本随后
    assert_eq!(openai["messages"][3]["role"], "tool");
    assert_eq!(openai["messages"][3]["tool_call_id"], "tu_1");
    assert_eq!(openai["messages"][4]["role"], "user");
    assert_eq!(openai["tool_choice"], "required");
    assert_eq!(openai["reasoning_effort"], "high");
    assert_eq!(openai["tools"][0]["function"]["parameters"]["type"], "object");
}

/// 验证 Responses（Codex CLI 等）请求转 Chat 格式：instructions 转 system、
/// function_call/function_call_output 回灌为 tool_calls 与 role=tool、reasoning
/// 条目不回灌、内置工具（web_search）被过滤仅保留 function、max_output_tokens 与
/// reasoning.effort 映射；并覆盖 input 字符串形态、developer 归并 system、
/// 未知 tool_choice 归一为 auto。
#[test]
fn responses_to_openai_conversion() {
    // Codex CLI 等 Responses 客户端请求转换为 Chat 格式（供 build_cc_request 复用）
    let req = json!({
        "model": "gpt-5-codex",
        "instructions": "你是助手",
        "max_output_tokens": 4096,
        "reasoning": { "effort": "high" },
        "tools": [
            { "type": "web_search" },
            { "type": "function", "name": "shell", "description": "执行命令", "parameters": { "type": "object" } },
        ],
        "tool_choice": "required",
        "input": [
            { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "你好" }] },
            { "type": "reasoning", "id": "rs_1", "summary": [] },
            { "type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}" },
            { "type": "function_call_output", "call_id": "call_1", "output": [{ "type": "output_text", "text": "ok" }] },
        ],
    });
    let openai = convert::convert_responses_to_openai(&req);
    assert_eq!(openai["model"], "gpt-5-codex");
    assert_eq!(openai["max_tokens"], 4096);
    assert_eq!(openai["reasoning_effort"], "high");
    let msgs = openai["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "你是助手");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[1]["content"][0]["text"], "你好");
    // reasoning 条目不回灌
    assert_eq!(msgs[2]["role"], "assistant");
    assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "shell");
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[3]["tool_call_id"], "call_1");
    assert_eq!(msgs[3]["content"], "ok");
    // 内置工具被过滤，仅保留 function 并嵌套
    assert_eq!(openai["tools"].as_array().unwrap().len(), 1);
    assert_eq!(openai["tools"][0]["function"]["name"], "shell");
    assert_eq!(openai["tools"][0]["function"]["parameters"]["type"], "object");
    assert_eq!(openai["tool_choice"], "required");

    // input 字符串形态 + developer role 归并为 system
    let req2 = json!({
        "model": "m",
        "input": [
            { "type": "message", "role": "developer", "content": "系统提示" },
            "纯文本输入",
        ],
    });
    let openai2 = convert::convert_responses_to_openai(&req2);
    assert_eq!(openai2["messages"][0]["role"], "system");
    assert_eq!(openai2["messages"][1]["role"], "user");
    assert_eq!(openai2["messages"][1]["content"], "纯文本输入");
    // 未知 tool_choice 归一为 auto
    let req3 = json!({ "model": "m", "input": "x", "tool_choice": "custom" });
    assert_eq!(convert::convert_responses_to_openai(&req3)["tool_choice"], "auto");
}

/// 验证 Responses 非流式响应体结构：reasoning 与 text、function_call 三类 output 条目、
/// usage 字段命名，以及 finish_reason=length 时 status=incomplete 并附
/// incomplete_details.reason=max_output_tokens。
#[test]
fn build_responses_response_shape() {
    let tool_calls = vec![json!({
        "id": "call_1", "type": "function",
        "function": { "name": "shell", "arguments": "{\"cmd\":\"ls\"}" },
    })];
    let body = convert::build_responses_response("resp_1", "gpt-5-codex", "Hi", "思考中", Some(&tool_calls), "tool_calls", 10, 5, 3);
    assert_eq!(body["id"], "resp_1");
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    // thinking 非空时首先输出 reasoning 条目
    assert_eq!(body["output"][0]["type"], "reasoning");
    assert_eq!(body["output"][0]["summary"][0]["text"], "思考中");
    assert_eq!(body["output"][1]["type"], "message");
    assert_eq!(body["output"][1]["content"][0]["text"], "Hi");
    assert_eq!(body["output"][2]["type"], "function_call");
    assert_eq!(body["output"][2]["call_id"], "call_1");
    assert_eq!(body["output"][2]["name"], "shell");
    assert_eq!(body["usage"]["output_tokens"], 5);
    assert_eq!(body["usage"]["input_tokens_details"]["cached_tokens"], 3);

    let truncated = convert::build_responses_response("resp_2", "m", "abc", "", None, "length", 1, 2, 0);
    assert_eq!(truncated["status"], "incomplete");
    assert_eq!(truncated["incomplete_details"]["reason"], "max_output_tokens");
}

// ── 单元测试：SSE 翻译 ────────────────────────────────

/// 验证 OpenAI 翻译器：text-delta 首帧携带 role 与 content，finish 帧输出
/// finish_reason 与 usage（completion_tokens=5），流尾产出 [DONE] 终止帧。
#[test]
fn openai_translator_streams_text_and_done() {
    let mut t = OpenAiTranslator::new("deepseek/deepseek-v4-flash", "chatcmpl-test");
    let frames = t.parse_line(r#"{"type":"text-delta","text":"Hello"}"#);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("\"content\":\"Hello\""));
    assert!(frames[0].contains("\"role\":\"assistant\""));

    let frames = t.parse_line(
        r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":10,"outputTokens":5,"inputTokenDetails":{"cacheReadTokens":3}}}"#,
    );
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("\"finish_reason\":\"stop\""));
    assert!(frames[0].contains("\"completion_tokens\":5"));
    assert_eq!(t.output_tokens, 5);
    assert!(t.done_event().contains("[DONE]"));
}

/// 验证零输出归一化：上游 outputTokens=0 时输入/缓存 token 计数被清零，
/// 且可生成 rate_limit_error 错误帧。
#[test]
fn openai_translator_zero_output_normalization() {
    let mut t = OpenAiTranslator::new("m", "c");
    t.parse_line(r#"{"type":"finish","totalUsage":{"inputTokens":100,"outputTokens":0,"inputTokenDetails":{"cacheReadTokens":90}}}"#);
    assert_eq!(t.input_tokens, 0);
    assert_eq!(t.cached_tokens, 0);
    assert!(t.zero_output_error_frame().contains("rate_limit_error"));
}

/// 验证 Anthropic 翻译器：message_start 首事件、text-delta 自动开启 text 块并产出
/// content_block_start/text_delta；finish 帧本身不输出事件；finalize 补发
/// message_delta（含 output_tokens=2）与 message_stop。
#[test]
fn anthropic_translator_blocks_and_finalize() {
    let mut t = AnthropicTranslator::new("claude-sonnet-4-6", "msg_test");
    assert!(t.message_start().contains("message_start"));
    let frames = t.process_line(r#"{"type":"text-delta","text":"Hi"}"#);
    assert!(frames.iter().any(|f| f.contains("content_block_start")));
    assert!(frames.iter().any(|f| f.contains("\"type\":\"text_delta\"")));
    let frames = t.process_line(
        r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":7,"outputTokens":2,"inputTokenDetails":{"cacheReadTokens":1}}}"#,
    );
    assert!(frames.is_empty());
    let end = t.finalize();
    assert!(end.iter().any(|f| f.contains("message_delta")));
    assert!(end.iter().any(|f| f.contains("message_stop")));
    assert!(end.iter().any(|f| f.contains("\"output_tokens\":2")));
}

/// 验证 Responses 翻译器事件序列：created → 文本条目 added/part.added/delta；
/// tool-call 前自动关闭文本条目（output_text.done）并发出 function_call 参数
/// delta/done；finish 回报覆盖 token 估算，finalize 补发 response.completed。
#[test]
fn responses_translator_text_and_tool_events() {
    let mut t = ResponsesTranslator::new("gpt-5-codex", "resp_test");
    assert!(t.response_start().contains("response.created"));

    let frames = t.process_line(r#"{"type":"text-delta","text":"Hello"}"#);
    assert!(frames.iter().any(|f| f.contains("response.output_item.added")));
    assert!(frames.iter().any(|f| f.contains("response.content_part.added")));
    assert!(frames.iter().any(|f| f.contains("\"delta\":\"Hello\"")));

    let frames = t.process_line(
        r#"{"type":"tool-call","toolCallId":"call_1","toolName":"shell","input":{"cmd":"ls"}}"#,
    );
    // 文本条目先关闭，再发出 function_call 完整事件序列
    assert!(frames.iter().any(|f| f.contains("response.output_text.done")));
    assert!(frames.iter().any(|f| f.contains("response.function_call_arguments.done")));
    assert!(frames.iter().any(|f| f.contains("\"name\":\"shell\"")));

    t.process_line(
        r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":10,"outputTokens":5,"inputTokenDetails":{"cacheReadTokens":3}}}"#,
    );
    assert_eq!(t.output_tokens, 5);
    let end = t.finalize();
    assert!(end.iter().any(|f| f.contains("response.completed")));
    assert!(end.iter().any(|f| f.contains("\"output_tokens\":5")));
}

/// 验证 Responses 翻译器把 reasoning-delta 翻译成 reasoning 条目的 summary 事件：
/// output_item.added（reasoning）+ reasoning_summary_part.added + summary_text.delta；
/// 正文开始前收尾 reasoning 条目（summary_text.done / summary_part.done）。
#[test]
fn responses_translator_reasoning_events() {
    let mut t = ResponsesTranslator::new("gpt-5-codex", "resp_reason");
    assert!(t.response_start().contains("response.created"));

    let frames = t.process_line(r#"{"type":"reasoning-delta","text":"我在思考"}"#);
    assert!(frames.iter().any(|f| f.contains("response.output_item.added") && f.contains("\"type\":\"reasoning\"")));
    assert!(frames.iter().any(|f| f.contains("response.reasoning_summary_part.added")));
    assert!(frames.iter().any(|f| f.contains("response.reasoning_summary_text.delta") && f.contains("我在思考")));

    // 正文开始先收尾 reasoning 条目
    let frames = t.process_line(r#"{"type":"text-delta","text":"答复"}"#);
    assert!(frames.iter().any(|f| f.contains("response.reasoning_summary_text.done")));
    assert!(frames.iter().any(|f| f.contains("response.reasoning_summary_part.done")));
    assert!(frames.iter().any(|f| f.contains("response.output_text.delta")));

    t.process_line(
        r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":8,"outputTokens":4,"inputTokenDetails":{"cacheReadTokens":0}}}"#,
    );
    let end = t.finalize();
    assert!(end.iter().any(|f| f.contains("response.completed")));
    // 完成响应里同时含 reasoning 与 message 两个条目
    assert!(end.iter().any(|f| f.contains("\"type\":\"reasoning\"") && f.contains("我在思考")));
}

/// 验证 Responses 翻译器仅含 reasoning、无正文时不会被误判为零输出。
#[test]
fn responses_translator_reasoning_only_not_empty() {
    let mut t = ResponsesTranslator::new("m", "resp_r2");
    t.process_line(r#"{"type":"reasoning-delta","text":"只有思考"}"#);
    // 上游必须给出完成信号，否则按「未正常走完」报错（不谎报成功）
    t.process_line(r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":1,"outputTokens":1}}"#);
    let end = t.finalize();
    assert!(end.iter().any(|f| f.contains("response.completed")));
    assert!(!end.iter().any(|f| f.contains("response.failed")));
}

/// 验证 Responses 翻译器异常路径：零输出时 finalize 发 response.failed（限流）；
/// 上游 error 事件即时转 response.failed 并置 has_error，此后 finalize 不再补发事件。
#[test]
fn responses_translator_zero_output_and_error() {
    let mut t = ResponsesTranslator::new("m", "resp_1");
    t.process_line(
        r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":50,"outputTokens":0,"inputTokenDetails":{"cacheReadTokens":40}}}"#,
    );
    let end = t.finalize();
    assert!(end.iter().any(|f| f.contains("response.failed")));
    assert!(end.iter().any(|f| f.contains("rate_limit_error")));

    let mut t2 = ResponsesTranslator::new("m", "resp_2");
    let frames = t2.process_line(r#"{"type":"error","error":{"message":"boom"}}"#);
    assert!(t2.has_error);
    assert!(frames.iter().any(|f| f.contains("response.failed") && f.contains("boom")));
    assert!(t2.finalize().is_empty());
}

// ── 单元测试：错误映射 / Key 提取 / 指纹 / slug ────────

/// 验证上游错误映射：429 保留状态码并附 retry_after=30 与上游消息；
/// 402（额度耗尽）映射为 429；500 映射为 502。
#[test]
fn error_mapping() {
    let (s, body) = errors::map_cc_error(429, r#"{"error":{"message":"slow down"}}"#);
    assert_eq!(s, 429);
    assert_eq!(body["retry_after"], 30);
    assert_eq!(body["error"]["message"], "slow down");

    let (s, _) = errors::map_cc_error(402, "");
    assert_eq!(s, 429);
    let (s, _) = errors::map_cc_error(500, "");
    assert_eq!(s, 502);
}

/// 验证本地转发 Key 提取：无 Authorization 头返回 None；Bearer 值中匹配 sk- 前缀片段；
/// 非 sk- 前缀（如 user_）的 Key 不被采信。
#[test]
fn api_key_extraction() {
    let mut headers = HeaderMap::new();
    assert!(server::extract_api_key(&headers).is_none());
    headers.insert(
        axum::http::header::AUTHORIZATION,
        "Bearer token_sk-abc123_def".parse().unwrap(),
    );
    assert_eq!(server::extract_api_key(&headers).unwrap(), "sk-abc123_def");
    headers.insert(
        axum::http::header::AUTHORIZATION,
        "Bearer user_abc123".parse().unwrap(),
    );
    assert!(server::extract_api_key(&headers).is_none());
}

/// 验证本地转发 Key 提取的 x-api-key 回退（Anthropic SDK 风格）：无 Authorization 头时
/// 从 x-api-key 提取 sk- 前缀片段；无效则返回 None。
#[test]
fn api_key_extraction_x_api_key_fallback() {
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", "Bearer token_sk-xyz_789".parse().unwrap());
    assert_eq!(server::extract_api_key(&headers).unwrap(), "sk-xyz_789");
    headers.insert("x-api-key", "user_abc".parse().unwrap());
    assert!(server::extract_api_key(&headers).is_none());
}

/// 验证 Anthropic 翻译器把 reasoning-delta 转成 thinking 块（thinking_delta），
/// 关闭 thinking 块时先发 signature_delta 假签名再 content_block_stop。
#[test]
fn anthropic_translator_thinking_block() {
    let mut t = AnthropicTranslator::new("claude-sonnet-4-6", "msg_think");
    let frames = t.process_line(r#"{"type":"reasoning-delta","text":"我在推理"}"#);
    assert!(frames.iter().any(|f| f.contains("content_block_start") && f.contains("\"type\":\"thinking\"")));
    assert!(frames.iter().any(|f| f.contains("thinking_delta") && f.contains("我在推理")));

    // 关闭 thinking 块：先 signature_delta（base64 假签名）再 content_block_stop
    let frames = t.process_line(r#"{"type":"text-delta","text":"正文"}"#);
    assert!(frames.iter().any(|f| f.contains("signature_delta") && f.contains("signature")));
    assert!(frames.iter().any(|f| f.contains("content_block_stop")));
    assert!(frames.iter().any(|f| f.contains("text_delta") && f.contains("正文")));
}

/// 验证 Responses 翻译器的事件带递增 sequence_number 字段。
#[test]
fn responses_translator_sequence_number() {
    let mut t = ResponsesTranslator::new("gpt-5-codex", "resp_seq");
    let start = t.response_start();
    assert!(start.contains("sequence_number"));
    assert!(start.contains("\"sequence_number\":0"));
    let frames = t.process_line(r#"{"type":"text-delta","text":"Hi"}"#);
    assert!(frames[0].contains("sequence_number"));
    // 首事件后 sequence_number 已递增到 1
    assert!(frames[0].contains("\"sequence_number\":1"));
}

/// 验证生成指纹的结构约束：thumbmark 为 64 位十六进制、平台固定 win32、
/// collector_version=1、MAC 哈希数量在 2-5 之间。
#[test]
fn fingerprint_shape() {
    let profile = fingerprint::default_device_profile("");
    let fp = fingerprint::generate("user_abc", "", &profile);
    assert_eq!(fp.thumbmark.len(), 64);
    assert_eq!(fp.components.platform, "win32");
    assert_eq!(fp.components.collector_version, 1);
    let n = fp.components.mac_hashes.len();
    assert!((2..=5).contains(&n));
}

/// 验证伪造项目 slug 的格式：不含盘符前缀、仅小写字母数字与连字符、不以连字符开头或结尾，
/// 且与 DEVICE_PROFILE.projectDir 同源（slug = slugify(workingDir)）。
#[test]
fn project_slug_format() {
    let profile = fingerprint::default_device_profile("");
    let slug = cc_client::slugify_project_path(&profile.project_dir);
    assert_eq!(slug, "users-dev-projects-app");
    assert!(!slug.starts_with("c:"));
    assert!(slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    assert!(slug.chars().all(|c| !c.is_uppercase()));
    assert!(!slug.starts_with('-') && !slug.ends_with('-'));
}

/// 验证指纹确定性派生：同一 key + salt 恒得同一台设备，不同 key / 不同 salt 换设备；
/// 不再需要持久化（跨重启天然一致）。
#[test]
fn fingerprint_deterministic_derivation() {
    let profile = fingerprint::default_device_profile("");
    let a1 = fingerprint::generate("user_abc123", "", &profile);
    let a2 = fingerprint::generate("user_abc123", "", &profile);
    assert_eq!(a1.thumbmark, a2.thumbmark);
    assert_eq!(a1.components.machine_id_hash, a2.components.machine_id_hash);
    assert_eq!(a1.components.mac_hashes, a2.components.mac_hashes);
    assert_eq!(a1.components.platform, a2.components.platform);
    assert_eq!(a1.components.mem_gib, a2.components.mem_gib);
    assert_eq!(a1.components.timezone, a2.components.timezone);
    assert_eq!(a1.components.collector_version, a2.components.collector_version);

    // 不同 key 换设备；同一 key 换 salt 也换设备
    let b = fingerprint::generate("user_xyz789", "", &profile);
    assert_ne!(a1.thumbmark, b.thumbmark);
    let c = fingerprint::generate("user_abc123", "salt-v2", &profile);
    assert_ne!(a1.thumbmark, c.thumbmark);
}

/// 验证同一 key 的指纹状态跨 AppState 实例稳定（确定性派生，不依赖磁盘）。
#[test]
fn fingerprint_survives_restart() {
    use super::cc_client;
    let key = "user_persist_test";

    let state1 = AppState::new(Config::default());
    let fp1 = cc_client::key_fingerprint_for_test(&state1, key);

    // 新实例：内存为空，仍派生出完全相同的指纹
    let state2 = AppState::new(Config::default());
    let fp2 = cc_client::key_fingerprint_for_test(&state2, key);
    assert_eq!(fp1.thumbmark, fp2.thumbmark);
    assert_eq!(fp1.components.machine_id_hash, fp2.components.machine_id_hash);
}

// ── 集成测试：mock 上游 ───────────────────────────────

/// mock Command Code 上游路由：/alpha/generate 按模型名返回正常 NDJSON、零输出 NDJSON
/// 或 429 错误；model 为 `capture` 时把请求体与 zdr 头记录进 `captured` 供断言。
fn mock_upstream(captured: Option<Arc<Mutex<Value>>>) -> Router {
    /// /alpha/generate 的 mock 处理器：按模型名回放预设响应，可选拦截请求体供断言。
    async fn generate(
        State(captured): State<Option<Arc<Mutex<Value>>>>,
        headers: axum::http::HeaderMap,
        body: String,
    ) -> axum::response::Response {
        let parsed: Value = serde_json::from_str(&body).unwrap();
        if let Some(cap) = &captured {
            let mut c = cap.lock().unwrap();
            c["body"] = parsed.clone();
            c["zdr"] = json!(headers
                .get("x-cmd-zdr")
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""));
        }
        if parsed["params"]["model"] == "zero-output" {
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"text-start\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":50,\"outputTokens\":0,\"inputTokenDetails\":{\"cacheReadTokens\":40}}}\n",
            ));
        }
        if parsed["params"]["model"] == "stream-error" {
            // 上游以 200 开流后立即下发 error 事件（provider 不可用、参数被拒等）：
            // 必须透传真实错误，不能被「零输出」兜底掩盖成误导性的限流 429。
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"error\",\"error\":{\"type\":\"server_error\",\"message\":\"No available providers match the 'only' filter: wafer.\",\"statusCode\":503,\"isRetryable\":false}}\n",
            ));
        }
        if parsed["params"]["model"] == "stream-error-400" {
            // 参数类错误（statusCode 400、不可重试）：下游应看到 400 而非 429
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"error\",\"error\":{\"type\":\"server_error\",\"message\":\"The system field can't be blank.\",\"statusCode\":400,\"isRetryable\":false}}\n",
            ));
        }
        if parsed["params"]["model"] == "no-finish" {
            // 有正文但流里根本没有 finish 事件（上游被截断）：不能谎报成功。
            // 对齐 CLI："Stream ended unexpectedly before completion (no finish event)"。
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"text-start\"}\n{\"type\":\"text-delta\",\"text\":\"半截回答\"}\n",
            ));
        }
        if parsed["params"]["model"] == "max-output-tokens" {
            // 截断类 finishReason 不止 length：max_output_tokens 同样表示被截断
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"text-delta\",\"text\":\"回答\"}\n{\"type\":\"finish\",\"finishReason\":\"max_output_tokens\",\"totalUsage\":{\"inputTokens\":10,\"outputTokens\":5}}\n",
            ));
        }
        if parsed["params"]["model"] == "pause-turn" {
            // Anthropic 原生枚举：这一轮被暂停、后面还有内容，不能折成正常结束
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"text-delta\",\"text\":\"部分\"}\n{\"type\":\"finish\",\"finishReason\":\"pause_turn\",\"totalUsage\":{\"inputTokens\":10,\"outputTokens\":5}}\n",
            ));
        }
        if parsed["params"]["model"] == "usage-only" {
            // 模拟 meta/muse-spark 等模型在 max_tokens 截断时的真实行为：
            // 上游只回 usage 元数据（outputTokens>0，含 reasoning/text 明细），
            // 不下发任何 text-delta/reasoning-delta 内容事件，finishReason=length。
            // 此场景不是零输出：usage 明确有输出 token，不应判空返回 429。
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"start-step\"}\n{\"type\":\"finish-step\",\"finishReason\":\"length\",\"usage\":{\"inputTokens\":27,\"inputTokenDetails\":{\"noCacheTokens\":27,\"cacheReadTokens\":0},\"outputTokens\":32,\"outputTokenDetails\":{\"textTokens\":3,\"reasoningTokens\":29}}}\n{\"type\":\"finish\",\"finishReason\":\"length\",\"totalUsage\":{\"inputTokens\":27,\"inputTokenDetails\":{\"noCacheTokens\":27,\"cacheReadTokens\":0},\"outputTokens\":32,\"outputTokenDetails\":{\"textTokens\":3,\"reasoningTokens\":29}}}\n{\"type\":\"provider-metadata\"}\n",
            ));
        }
        if parsed["params"]["model"] == "slow" {
            // 慢响应：sleep 300ms 模拟上游耗时，用于并发上限测试占用在途额度
            tokio::time::sleep(Duration::from_millis(300)).await;
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"text-start\"}\n{\"type\":\"text-delta\",\"text\":\"Hello\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":10,\"outputTokens\":5,\"inputTokenDetails\":{\"cacheReadTokens\":3}}}\n",
            ));
        }
        // 上游思考阶段完全静默：模型名 `idle-gap-<毫秒>` 指定首字节后的静默时长，
        // 之后才吐正文。meta/muse-spark 系列的思考停顿就是这么产生的（上游不下发
        // reasoning-delta，纯静默数十秒），用于验证代理的空闲超时不会误杀健康请求。
        if let Some(ms) = parsed["params"]["model"]
            .as_str()
            .and_then(|m| m.strip_prefix("idle-gap-"))
            .and_then(|s| s.parse::<u64>().ok())
        {
            let head: Result<Bytes, std::io::Error> =
                Ok(Bytes::from("{\"type\":\"start\"}\n{\"type\":\"start-step\"}\n"));
            let tail: Result<Bytes, std::io::Error> = Ok(Bytes::from(
                "{\"type\":\"text-start\"}\n{\"type\":\"text-delta\",\"text\":\"思考完的正文\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":10,\"outputTokens\":5,\"inputTokenDetails\":{\"cacheReadTokens\":0}}}\n",
            ));
            let first = futures_util::stream::once(async move { head });
            let second = futures_util::stream::once(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                tail
            });
            return axum::response::Response::new(axum::body::Body::from_stream(first.chain(second)));
        }
        if parsed["params"]["model"] == "no-usage" {
            // 有正文但 finish 不回报 usage：验证非流式零输出按实际内容判定而非 usage
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"text-start\"}\n{\"type\":\"text-delta\",\"text\":\"正文内容\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\"}\n",
            ));
        }
        if parsed["params"]["model"] == "tool-caller" {
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"tool-call\",\"toolCallId\":\"call_1\",\"toolName\":\"get_weather\",\"input\":{\"city\":\"BJ\"}}\n{\"type\":\"finish\",\"finishReason\":\"tool-calls\",\"totalUsage\":{\"inputTokens\":10,\"outputTokens\":5,\"inputTokenDetails\":{\"cacheReadTokens\":0}}}\n",
            ));
        }
        if parsed["params"]["model"] == "reasoner" {
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"reasoning-delta\",\"text\":\"deep thought\"}\n{\"type\":\"text-delta\",\"text\":\"answer\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":10,\"outputTokens\":5,\"inputTokenDetails\":{\"cacheReadTokens\":0}}}\n",
            ));
        }
        if parsed["params"]["model"] == "bad-line" {
            return axum::response::Response::new(axum::body::Body::from(
                "not-a-json-line\n{\"type\":\"start\"}\n{\"type\":\"text-delta\",\"text\":\"after bad line\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":1,\"outputTokens\":1,\"inputTokenDetails\":{\"cacheReadTokens\":0}}}\n",
            ));
        }
        if parsed["params"]["model"] == "upstream-error" {
            return axum::response::Response::builder()
                .status(429)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(r#"{"error":{"message":"rate limited"}}"#))
                .unwrap();
        }
        axum::response::Response::new(axum::body::Body::from(
            "{\"type\":\"start\"}\n{\"type\":\"text-start\"}\n{\"type\":\"text-delta\",\"text\":\"Hello\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":10,\"outputTokens\":5,\"inputTokenDetails\":{\"cacheReadTokens\":3}}}\n",
        ))
    }

    Router::new()
        .route("/alpha/generate", post(generate))
        .route(
            "/alpha/fingerprint/record",
            post(|| async { axum::response::Response::new(axum::body::Body::from("{}")) }),
        )
        .route(
            "/alpha/lifecycle-events",
            post(|| async { axum::response::Response::new(axum::body::Body::from("{}")) }),
        )
        .route(
            "/provider/v1/models",
            axum::routing::get(|| async {
                axum::response::Response::builder()
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"data":[{"id":"mock-model-1","object":"model","name":"Mock Model 1","context_length":1000,"owned_by":"command-code"}]}"#,
                    ))
                    .unwrap()
            }),
        )
        .with_state(captured)
}

/// 启动 mock 上游与真实代理服务（api_base 指向 mock，不启用后台任务），
/// 轮询等待服务就绪后返回（代理 base URL, 共享状态）。
/// `captured` 传入时 mock 会把 generate 请求体记录进该容器；
/// `adjust` 可对默认配置做覆盖（如 max_inflight）。
async fn start_proxy_impl(
    captured: Option<Arc<Mutex<Value>>>,
    adjust: impl FnOnce(&mut Config),
) -> (String, Arc<AppState>) {
    let mock = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(mock, mock_upstream(captured)).await;
    });

    let mut cfg = Config {
        api_base: format!("http://{mock_addr}"),
        port: 0, // 下面绑定真实端口
        auto_start_proxy: false,
        ..Config::default()
    };
    adjust(&mut cfg);
    let state = AppState::new(cfg);

    // 注入内存设置库：生成本地转发 Key（sk-）并配置一个 Command Code 账户，使鉴权路径可用
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        super::settings::init_settings_on(&conn).unwrap();
        crate::credentials::save_local_key(&conn, TEST_LOCAL_KEY).unwrap();
        crate::credentials::add_account(
            &conn,
            &crate::proxy::config::Account {
                key: "user_test_account".into(),
                user_id: "test_user_id".into(),
                user_name: "Test".into(),
                source: "manual".into(),
                added_at: 0,
            },
        )
        .unwrap();
        // 账户列表以 AppState.config 为内存真相源，需同步
        state.config.write().unwrap().cc_accounts = vec![crate::proxy::config::Account {
            key: "user_test_account".into(),
            user_id: "test_user_id".into(),
            user_name: "Test".into(),
            source: "manual".into(),
            added_at: 0,
        }];
        *state.usage.lock().unwrap() = Some(conn);
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    *state.shutdown.lock().unwrap() = Some(tx);
    let st = state.clone();
    tokio::spawn(async move {
        let _ = server::serve(listener, st, rx, false).await;
    });
    for _ in 0..100 {
        if state.is_running() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (format!("http://{addr}"), state)
}


/// 并行高负载下 macOS loopback 偶发 ConnectionReset/IncompleteMessage，
/// 对连接层错误做最多 3 次重试（仅重试发送失败，HTTP 错误状态不重试）。
async fn send_retry(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: &str,
    body: Option<&Value>,
) -> reqwest::Response {
    let mut last_err = None;
    for _ in 0..3 {
        let mut req = client.request(method.clone(), url);
        req = req.header("Authorization", format!("Bearer {token}"));
        if let Some(b) = body {
            req = req.json(b);
        }
        match req.send().await {
            Ok(r) => return r,
            Err(e) => last_err = Some(e),
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    panic!("请求重试 3 次仍失败: {last_err:?}");
}

/// 启动 mock 上游与真实代理服务（无捕获）。
async fn start_proxy() -> (String, Arc<AppState>) {
    start_proxy_impl(None, |_| {}).await
}

/// 启动 mock 上游与真实代理服务（带捕获）。
async fn start_proxy_with_capture(captured: Option<Arc<Mutex<Value>>>) -> (String, Arc<AppState>) {
    start_proxy_impl(captured, |_| {}).await
}

/// 端到端：max_inflight=1 时，第一个慢请求占用在途额度期间，第二个业务请求返回 503
/// server_busy + Retry-After；/health 不受限制。
#[tokio::test]
async fn max_inflight_caps_concurrency() {
    let (base, state) = start_proxy_impl(None, |c| c.max_inflight = 1).await;
    let client = reqwest::Client::new();
    // 慢请求在 mock 上游 sleep 300ms，期间占用在途额度
    let slow_base = base.clone();
    let client2 = client.clone();
    let slow_handle = tokio::spawn(async move {
        client2
            .post(format!("{slow_base}/v1/chat/completions"))
            .header("Authorization", "Bearer sk-test-local-key-123")
            .json(&json!({
                "model": "slow",
                "messages": [{ "role": "user", "content": "hi" }],
            }))
            .send()
            .await
            .unwrap()
    });
    // 等待慢请求进入在途（中间件计数 +1、mock 开始 sleep）
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 第二个业务请求超限 → 503
    let second = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "deepseek/deepseek-v4-flash",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 503);
    let body: Value = second.json().await.unwrap();
    assert_eq!(body["error"]["type"], "server_busy");
    assert_eq!(body["retry_after"], 5);

    // 慢请求最终正常完成
    let slow_res = slow_handle.await.unwrap();
    assert_eq!(slow_res.status(), 200);
    state.mark_stopped();
}

/// 端到端：Chat Completions 非流式请求返回 200，聚合文本为 Hello，
/// usage 含 completion_tokens 与缓存命中计数。
#[tokio::test]
async fn chat_completions_nonstream() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "deepseek/deepseek-v4-flash",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hello");
    assert_eq!(body["usage"]["completion_tokens"], 5);
    assert_eq!(body["usage"]["prompt_tokens_details"]["cached_tokens"], 3);
    state.mark_stopped();
}

/// 端到端：Chat Completions 流式请求返回 SSE，包含内容 chunk、[DONE] 终止帧与 usage 统计。
#[tokio::test]
async fn chat_completions_streaming() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "deepseek/deepseek-v4-flash",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    assert!(text.contains("\"content\":\"Hello\""));
    assert!(text.contains("data: [DONE]"));
    assert!(text.contains("\"completion_tokens\":5"));
    state.mark_stopped();
}

/// 端到端：客户端可用不带厂商前缀的短模型名，代理补齐为上游要求的完整 ID。
///
/// 回归：上游只认带厂商前缀的完整 ID，裸名返回 403 `Model/provider not recognized`
/// （经代理表现为 401）。而部分 agent 工具对模型名长度有上限，写不下
/// `deepseek/deepseek-v4.1-flash`、`meta/muse-spark-1.3-contributor` 这类长 ID。
/// 三条协议入口都要支持，且发给上游的必须是补全后的 ID。
#[tokio::test]
async fn short_model_name_is_expanded_to_full_id() {
    let captured = Arc::new(Mutex::new(json!({})));
    let (base, state) = start_proxy_with_capture(Some(captured.clone())).await;
    // 内置表在测试环境即为模型来源，无需依赖 Provider 拉取
    state.models.write().unwrap().models.clear();

    let client = reqwest::Client::new();
    let cases: [(&str, Value, &str); 3] = [
        (
            "/v1/chat/completions",
            json!({
                "model": "deepseek-v4.1-flash",
                "messages": [{ "role": "user", "content": "hi" }],
                "stream": true,
            }),
            "deepseek/deepseek-v4.1-flash",
        ),
        (
            "/v1/messages",
            json!({
                "model": "muse-spark-1.3-contributor",
                "messages": [{ "role": "user", "content": "hi" }],
                "stream": true,
            }),
            "meta/muse-spark-1.3-contributor",
        ),
        (
            "/v1/responses",
            json!({
                "model": "Kimi-K3",
                "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }],
                "stream": true,
            }),
            "moonshotai/Kimi-K3",
        ),
    ];
    for (path, body, expected_upstream) in cases {
        let res = client
            .post(format!("{base}{path}"))
            .header("Authorization", "Bearer sk-test-local-key-123")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "{path} 短名应被解析而非报错");
        let sent = captured.lock().unwrap()["body"]["params"]["model"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            sent, expected_upstream,
            "{path} 发给上游的必须是补齐后的完整 ID"
        );
        // 响应回显同一份已解析名称，便于客户端确认实际调用的模型
        let text = res.text().await.unwrap();
        assert!(
            text.contains(expected_upstream),
            "{path} 响应应回显完整 ID，实际: {text}"
        );
    }
    state.mark_stopped();
}

/// 端到端：未收录的模型名原样透传给上游（不猜测、不模糊匹配邻近模型）。
#[tokio::test]
async fn unknown_model_name_is_passed_through_unchanged() {
    let captured = Arc::new(Mutex::new(json!({})));
    let (base, state) = start_proxy_with_capture(Some(captured.clone())).await;

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            // 与 deepseek-v4.1-flash 仅差一个字符：不得被模糊匹配成它
            "model": "deepseek-v4.1-flashX",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let sent = captured.lock().unwrap()["body"]["params"]["model"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        sent, "deepseek-v4.1-flashX",
        "未收录名称应原样透传，交由上游给出明确错误"
    );
    state.mark_stopped();
}

/// 端到端：上游思考阶段长时间静默（不发任何字节）不应被空闲超时误杀。
///
/// 回归：上游对思考型模型（如 meta/muse-spark-1.3-contributor）在思考阶段完全不下发
/// 事件——实测静默 50~55 秒，长思考可达 395 秒以上，之后才吐正文。此前硬编码的 30s
/// 空闲超时会把这类健康请求判死（HTTP 429「Response timeout」），而官方 CLI 对上游
/// 不设任何 idle timeout。
/// 三条协议入口共用同一读流逻辑，必须逐一覆盖。
#[tokio::test]
async fn upstream_silence_is_tolerated_by_default() {
    // 默认配置（不限空闲超时）下，静默 1.5s 后必须仍能拿到完整正文
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    for (path, body) in [
        (
            "/v1/chat/completions",
            json!({
                "model": "idle-gap-1500",
                "messages": [{ "role": "user", "content": "hi" }],
                "stream": true,
            }),
        ),
        (
            "/v1/messages",
            json!({
                "model": "idle-gap-1500",
                "messages": [{ "role": "user", "content": "hi" }],
                "stream": true,
            }),
        ),
        (
            "/v1/responses",
            json!({
                "model": "idle-gap-1500",
                "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }],
                "stream": true,
            }),
        ),
    ] {
        let res = client
            .post(format!("{base}{path}"))
            .header("Authorization", "Bearer sk-test-local-key-123")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "{path} 静默期间不应被空闲超时判死");
        let text = res.text().await.unwrap();
        assert!(
            text.contains("思考完的正文"),
            "{path} 应拿到静默之后的完整正文，实际: {text}"
        );
        assert!(
            !text.contains("Response timeout"),
            "{path} 不应出现超时错误帧，实际: {text}"
        );
    }
    state.mark_stopped();
}

/// 端到端：显式配置空闲超时后，超过阈值的静默仍会被判为超时（该兜底能力未被移除）。
#[tokio::test]
async fn configured_stream_idle_timeout_still_fires() {
    let (base, state) = start_proxy_impl(None, |c| c.stream_idle_timeout_secs = 1).await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "idle-gap-2500",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    // 静默 2.5s > 配置的 1s：应判超时（首帧前回退为 JSON 429）
    assert_eq!(res.status(), 429);
    let text = res.text().await.unwrap();
    assert!(
        text.contains("Response timeout"),
        "配置了超时后应报超时，实际: {text}"
    );
    state.mark_stopped();
}

/// 端到端：非流式路径的静默同样默认不被误杀，仅显式配置后才判超时。
#[tokio::test]
async fn nonstream_silence_respects_configured_timeout() {
    // 默认不限：静默 1.5s 仍应成功
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "idle-gap-1500",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "非流式静默不应被空闲超时判死");
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "思考完的正文");
    state.mark_stopped();

    // 显式配置 1s：静默 2.5s 应判超时
    let (base, state) = start_proxy_impl(None, |c| c.nonstream_idle_timeout_secs = 1).await;
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "idle-gap-2500",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
    let body: Value = res.json().await.unwrap();
    assert!(
        body["error"]["message"].as_str().unwrap_or("").contains("timeout")
            || body["error"]["message"].as_str().unwrap_or("").contains("Response timeout"),
        "应报超时，实际: {body}"
    );
    state.mark_stopped();
}

/// 端到端：上游零输出（outputTokens=0）时，非流式请求回退为 429 限流响应
/// （rate_limit_error + retry_after=10）。
#[tokio::test]
async fn chat_completions_zero_output_returns_429() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "zero-output",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(body["retry_after"], 10);
    state.mark_stopped();
}

/// 端到端：上游只回 usage 元数据（outputTokens>0）而无任何内容事件时不算零输出。
///
/// 回归：meta/muse-spark-1.3-contributor 在 max_tokens 截断（finishReason=length）时
/// 只回 outputTokenDetails（textTokens/reasoningTokens），内容事件一个都不发；
/// 按「聚合内容为空」判定会误杀成 429，丢失上游已产出 usage 的响应。
#[tokio::test]
async fn usage_only_response_is_not_zero_output() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();

    // 非流式：应返回 200 + finish_reason=length + usage，而非 429 空响应
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "usage-only",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "usage 有输出 token 时不应判为零输出");
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["choices"][0]["finish_reason"], "length");
    assert_eq!(body["usage"]["completion_tokens"], 32);
    assert_eq!(body["usage"]["prompt_tokens"], 27);

    // 流式：应正常收尾（finish 帧 + [DONE]），而非 429 错误帧
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "usage-only",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    assert!(
        !text.contains("Empty response from upstream"),
        "流式不应下发零输出错误帧: {text}"
    );
    assert!(text.contains("\"finish_reason\":\"length\""), "应有 length 结束帧: {text}");
    assert!(text.contains("[DONE]"), "应有 [DONE] 收尾: {text}");

    // Anthropic 非流式：同样不应判空
    let res = client
        .post(format!("{base}/v1/messages"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "usage-only",
            "messages": [{ "role": "user", "content": "hi" }],
            "max_tokens": 32,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "Anthropic 协议下同样不应判为零输出");
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["stop_reason"], "max_tokens");

    // Anthropic 流式：应正常收尾（message_delta + message_stop），而非错误帧。
    // 该路径的零输出判定在 translator.finalize() 内部，与 chat 流式不在同一处。
    let res = client
        .post(format!("{base}/v1/messages"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "usage-only",
            "messages": [{ "role": "user", "content": "hi" }],
            "max_tokens": 32,
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    assert!(
        !text.contains("Empty response from upstream"),
        "Anthropic 流式不应下发零输出错误帧: {text}"
    );
    assert!(text.contains("message_stop"), "Anthropic 流式应正常收尾: {text}");

    // Responses 非流式：同样是独立的判定点
    let res = client
        .post(format!("{base}/v1/responses"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "usage-only",
            "input": "hi",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "Responses 协议下同样不应判为零输出");
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["status"], "incomplete");
    assert_eq!(body["incomplete_details"]["reason"], "max_output_tokens");

    // Responses 流式：零输出判定在 ResponsesTranslator::finalize() 内部
    let res = client
        .post(format!("{base}/v1/responses"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "usage-only",
            "input": "hi",
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    assert!(
        !text.contains("Empty response from upstream"),
        "Responses 流式不应下发零输出错误帧: {text}"
    );
    assert!(
        text.contains("response.incomplete"),
        "Responses 流式应报 incomplete 而非 completed: {text}"
    );

    state.mark_stopped();
}

/// 端到端：上游以 200 开流后下发 error 事件时，透传真实状态码与消息。
///
/// 回归：此前 error 事件只记日志，流结束被「零输出」兜底覆盖成 429 限流错误，
/// 下游看不到真实原因（如 provider 不可用、system 被拒），也无法区分该重试还是换模型。
#[tokio::test]
async fn stream_error_event_is_propagated_not_masked_as_zero_output() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();

    // 503 + 不可重试：下游应看到上游状态码与原始消息，且不暗示重试
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "stream-error",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 503, "应透传上游状态码而非 429");
    let body: Value = res.json().await.unwrap();
    let msg = body["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("No available providers"), "应透传上游原始消息: {msg}");
    assert!(
        !msg.contains("Empty response"),
        "不应被零输出兜底掩盖: {msg}"
    );

    // 400 参数类错误：同样透传（不可重试 → 无 retry_after）
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "stream-error-400",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400, "参数类错误应透传 400");
    let body: Value = res.json().await.unwrap();
    assert!(
        body["error"]["message"].as_str().unwrap_or("").contains("system field"),
        "应透传上游参数错误消息"
    );

    // Anthropic 协议入口同样透传
    let res = client
        .post(format!("{base}/v1/messages"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "stream-error",
            "messages": [{ "role": "user", "content": "hi" }],
            "max_tokens": 16,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 503, "Anthropic 入口同样透传上游状态码");
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert!(
        body["error"]["message"].as_str().unwrap_or("").contains("No available providers"),
        "Anthropic 错误体应含上游消息"
    );

    state.mark_stopped();
}

/// 端到端：上游未正常走完 finish 时不谎报成功（对齐上游 #39）。
///
/// 三种情形：流里没有 finish 事件（被截断）、截断类 finishReason 的别名
/// （max_output_tokens / model_context_window_exceeded）、pause_turn。
#[tokio::test]
async fn incomplete_upstream_is_not_reported_as_success() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();

    // 情形一：有正文但流里没有 finish 事件 —— 非流式应报可重试错误而非 200
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "no-finish",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 502, "没有 finish 事件时不应谎报 200");
    let body: Value = res.json().await.unwrap();
    let msg = body["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("no finish event"), "错误消息应指明根因: {msg}");

    // 流式同场景：应下发错误帧而不是 [DONE]
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "no-finish",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    let text = res.text().await.unwrap();
    assert!(
        text.contains("no finish event") || text.contains("without a completion finish"),
        "流式应告知上游被截断: {text}"
    );

    // 情形二：finishReason=max_output_tokens（length 家族别名）—— 不谎报 completed
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "max-output-tokens",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(
        body["choices"][0]["finish_reason"], "length",
        "max_output_tokens 应归一为 length: {body}"
    );

    // Anthropic 协议下同一 finishReason 应报 max_tokens 而非 end_turn
    let res = client
        .post(format!("{base}/v1/messages"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "max-output-tokens",
            "messages": [{ "role": "user", "content": "hi" }],
            "max_tokens": 32,
        }))
        .send()
        .await
        .unwrap();
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["stop_reason"], "max_tokens", "截断类 finishReason 不折成 end_turn: {body}");

    // 情形三：pause_turn —— Anthropic 原样透出，Responses 报 incomplete
    let res = client
        .post(format!("{base}/v1/messages"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "pause-turn",
            "messages": [{ "role": "user", "content": "hi" }],
            "max_tokens": 32,
        }))
        .send()
        .await
        .unwrap();
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["stop_reason"], "pause_turn", "pause_turn 应原样透出: {body}");

    let res = client
        .post(format!("{base}/v1/responses"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({ "model": "pause-turn", "input": "hi" }))
        .send()
        .await
        .unwrap();
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["status"], "incomplete", "pause_turn 不应报 completed: {body}");
    assert_eq!(body["incomplete_details"]["reason"], "pause_turn");

    // OpenAI 协议下 pause_turn 折成 length（OpenAI 无该枚举，length 表达输出不完整）
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "pause-turn",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["choices"][0]["finish_reason"], "length", "pause_turn 应折成 length: {body}");

    state.mark_stopped();
}

/// 端到端：上游返回 429 错误体时，透传状态码、retry_after=30 与上游错误消息。
#[tokio::test]
async fn upstream_error_mapped() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "upstream-error",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["retry_after"], 30);
    assert_eq!(body["error"]["message"], "rate limited");
    state.mark_stopped();
}

/// 端到端：无 system 请求默认发空格占位、ZDR 模式开启时上游收到 x-cmd-zdr 头。
#[tokio::test]
async fn empty_system_placeholder_and_zdr_header() {
    let captured = Arc::new(Mutex::new(json!({})));
    let (base, state) = start_proxy_with_capture(Some(captured.clone())).await;
    // 开启 ZDR 模式并写入共享状态（模拟 config_save 后运行时配置热更新）
    state.config.write().unwrap().zdr = true;

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "deepseek/deepseek-v4-flash",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let cap = captured.lock().unwrap();
    // 无 system 时 params.system 为句点占位（块数组形态，空格会被部分 provider 拒绝）
    assert_eq!(cap["body"]["params"]["system"][0]["text"], ".");
    // ZDR 模式开启时 generate 请求携带 x-cmd-zdr: 1
    assert_eq!(cap["zdr"], "1");
    drop(cap);
    state.mark_stopped();
}

/// 端到端：/v1/models 返回 Provider 动态模型列表、/health 返回 OK、
/// 无 API Key 的补全请求被拒绝为 401。
#[tokio::test]
async fn models_and_health_and_401() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{base}/v1/models"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert!(body["data"].as_array().unwrap().iter().any(|m| m["id"] == "mock-model-1"));

    let res = send_retry(&client, reqwest::Method::GET, &format!("{base}/health"), "", None).await;
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "OK");

    // 根路径同样映射到健康检查
    let res = send_retry(&client, reqwest::Method::GET, &format!("{base}/"), "", None).await;
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "OK");

    let res = send_retry(
        &client,
        reqwest::Method::POST,
        &format!("{base}/v1/chat/completions"),
        "",
        Some(&json!({ "messages": [] })),
    )
    .await;
    assert_eq!(res.status(), 401);
    state.mark_stopped();
}

/// 端到端：/v1/responses 带 previous_response_id 时显式返回 400（无状态代理不支持会话续接）。
#[tokio::test]
async fn responses_previous_response_id_rejected() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/responses"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "gpt-5-codex",
            "previous_response_id": "resp_prev",
            "input": "hi",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(body["error"]["message"].as_str().unwrap().contains("previous_response_id"));
    state.mark_stopped();
}

/// 端到端：Responses 非流式请求返回 response 对象（resp_ 前缀 id、completed 状态、
/// message 输出条目与 Responses 风格 usage）。
#[tokio::test]
async fn responses_nonstream() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/responses"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "gpt-5-codex",
            "instructions": "你是助手",
            "input": "hi",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert!(body["id"].as_str().unwrap().starts_with("resp_"));
    assert_eq!(body["output"][0]["type"], "message");
    assert_eq!(body["output"][0]["content"][0]["text"], "Hello");
    assert_eq!(body["usage"]["output_tokens"], 5);
    assert_eq!(body["usage"]["input_tokens_details"]["cached_tokens"], 3);
    state.mark_stopped();
}

/// 端到端：Responses 流式请求返回完整事件序列（response.created → output_text.delta
/// → response.completed，含 output_tokens 统计）。
#[tokio::test]
async fn responses_streaming() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/responses"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "gpt-5-codex",
            "input": [{ "type": "message", "role": "user", "content": "hi" }],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    assert!(text.contains("event: response.created"));
    assert!(text.contains("event: response.output_text.delta"));
    assert!(text.contains("\"delta\":\"Hello\""));
    assert!(text.contains("event: response.completed"));
    assert!(text.contains("\"output_tokens\":5"));
    state.mark_stopped();
}

/// 端到端：Responses 流式请求在上游零输出时回退为 429 JSON 限流响应
/// （首帧内容未出现前仍可携带真实状态码）。
#[tokio::test]
async fn responses_zero_output_returns_429() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/responses"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "zero-output",
            "input": "hi",
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["type"], "rate_limit_error");
    state.mark_stopped();
}

/// 端到端：Anthropic Messages 非流式请求返回 text content 块、stop_reason=end_turn
/// 与 output_tokens 统计。
#[tokio::test]
async fn anthropic_messages_nonstream() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/messages"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 1000,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["content"][0]["type"], "text");
    assert_eq!(body["content"][0]["text"], "Hello");
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(body["usage"]["output_tokens"], 5);
    state.mark_stopped();
}

/// 端到端：上游有正文但 finish 不回报 usage 时，Anthropic 非流式按实际内容判定，
/// 返回 200（而非按 usage 误判为 429 零输出）。
#[tokio::test]
async fn anthropic_nonstream_content_without_usage() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/messages"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "no-usage",
            "max_tokens": 1000,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "正文内容");
    // 上游未回报 output_tokens，按内容长度估算得到非 0
    assert!(body["usage"]["output_tokens"].as_u64().unwrap() > 0);
    state.mark_stopped();
}

/// 端到端：注入统计库后，一次非流式成功请求会被采集写入 SQLite，
/// 查询 stats 能聚合出请求数与 token 用量。
#[tokio::test]
async fn usage_recorded_through_proxy() {
    let (base, state) = start_proxy().await;
    // 注入内存统计库（连同设置表：本地 Key + Command Code 账户，保证鉴权路径可用）
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    super::settings::init_settings_on(&conn).unwrap();
    super::usage::init_usage_on(&conn).unwrap();
    crate::credentials::save_local_key(&conn, TEST_LOCAL_KEY).unwrap();
    crate::credentials::add_account(
            &conn,
            &crate::proxy::config::Account {
                key: "user_test_account".into(),
                user_id: "test_user_id".into(),
                user_name: "Test".into(),
                source: "manual".into(),
                added_at: 0,
            },
        )
        .unwrap();
    *state.usage.lock().unwrap() = Some(conn);

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "deepseek/deepseek-v4-flash",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    // 请求已结束，采集应已落库
    let guard = state.usage.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let stats = super::usage::get_stats(conn, super::usage::Period::All).unwrap();
    assert_eq!(stats.total_requests, 1);
    assert_eq!(stats.total_prompt_tokens, 10);
    assert_eq!(stats.total_completion_tokens, 5);
    assert_eq!(stats.total_cached_tokens, 3);
    assert!(stats.total_cost > 0.0);
    assert_eq!(stats.by_model.len(), 1);
    assert_eq!(stats.by_model[0].key, "deepseek/deepseek-v4-flash");
    assert_eq!(stats.recent_requests.len(), 1);
    assert_eq!(stats.recent_requests[0].endpoint, "/v1/chat/completions");
    drop(guard);
    state.mark_stopped();
}

/// 端到端：零输出请求（429）不计入用量统计（token 为 0 不采集）。
#[tokio::test]
async fn zero_output_not_recorded() {
    let (base, state) = start_proxy().await;
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    super::settings::init_settings_on(&conn).unwrap();
    super::usage::init_usage_on(&conn).unwrap();
    crate::credentials::save_local_key(&conn, TEST_LOCAL_KEY).unwrap();
    crate::credentials::add_account(
            &conn,
            &crate::proxy::config::Account {
                key: "user_test_account".into(),
                user_id: "test_user_id".into(),
                user_name: "Test".into(),
                source: "manual".into(),
                added_at: 0,
            },
        )
        .unwrap();
    *state.usage.lock().unwrap() = Some(conn);

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "zero-output",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);

    let guard = state.usage.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let stats = super::usage::get_stats(conn, super::usage::Period::All).unwrap();
    assert_eq!(stats.total_requests, 0);
    drop(guard);
    state.mark_stopped();
}

// ── SSE 翻译器边角分支 ─────────────────────────────────────

/// OpenAI 翻译器：噪声行/坏 JSON/未知事件返回空帧，reasoning 与 tool-call 正确输出。
#[test]
fn openai_translator_edge_events() {
    let mut t = OpenAiTranslator::new("m", "id1");
    // 噪声输入一律空帧
    assert!(t.parse_line("").is_empty());
    assert!(t.parse_line("[DONE]").is_empty());
    assert!(t.parse_line(": comment").is_empty());
    assert!(t.parse_line("{ not json").is_empty());
    assert!(t.parse_line(r#"{"no":"type"}"#).is_empty());
    assert!(t.parse_line(r#"{"type":"mystery-event"}"#).is_empty());
    // 忽略列表事件
    assert!(t
        .parse_line(r#"{"type":"tool-input-start","toolCallId":"t1"}"#)
        .is_empty());

    // text-delta 的 delta 字段别名与空文本早退
    assert!(t.parse_line(r#"{"type":"text-delta"}"#).is_empty());
    let frames = t.parse_line(r#"{"type":"text-delta","delta":"via-delta"}"#);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("via-delta"));

    // reasoning-delta：首个 chunk 带 role，后续不带
    let frames = t.parse_line(r#"{"type":"reasoning-delta","text":"thinking"}"#);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("reasoning_content"));

    // tool-call：显式 id 与对象形态 input
    let frames = t.parse_line(
        r#"{"type":"tool-call","toolCallId":"call_1","toolName":"get_weather","input":{"city":"BJ"}}"#,
    );
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("call_1"));
    assert!(frames[0].contains("get_weather"));

    // finish-step 记录 usage，后续 finish 缺 usage 时保留（不被 0 覆盖）
    assert!(t
        .parse_line(r#"{"type":"finish-step","finishReason":"tool-calls","usage":{"inputTokens":7,"outputTokens":3}}"#)
        .is_empty());
    let frames = t.parse_line(r#"{"type":"finish","finishReason":"stop"}"#);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("tool_calls"));
    assert!(frames[0].contains("\"completion_tokens\":3"));
    // error 事件仅记日志，无帧
    assert!(t
        .parse_line(r#"{"type":"error","error":{"message":"boom"}}"#)
        .is_empty());
}

/// Anthropic 翻译器：tool-call 产出 tool_use 块三连帧，error 产出 error 帧。
#[test]
fn anthropic_translator_tool_and_error() {
    let mut t = AnthropicTranslator::new("claude-x", "msg_1");
    let frames = t.process_line(r#"{"type":"tool-call","toolCallId":"tu_1","toolName":"calc","input":{"x":1}}"#);
    // content_block_start + input_json_delta + content_block_stop
    assert_eq!(frames.len(), 3);
    assert!(frames[0].contains("tool_use"));
    assert!(frames[1].contains("input_json_delta"));
    assert!(frames[2].contains("content_block_stop"));

    let frames = t.process_line(r#"{"type":"error","error":{"message":"upstream broke"}}"#);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].contains("internal_error"));
    assert!(frames[0].contains("upstream broke"));
    // finalize 在已出错时不再发收尾帧
    assert!(t.finalize().is_empty());
}

/// Anthropic 流式收尾的 stop_reason：上游 tool-calls 必须映射为 tool_use，
/// 且尾部 finish 的 "stop" 不得覆盖 finish-step 记录的工具调用结论。
#[test]
fn anthropic_translator_tool_stop_reason() {
    let mut t = AnthropicTranslator::new("claude-x", "msg_1");
    t.process_line(r#"{"type":"tool-call","toolCallId":"tu_1","toolName":"calc","input":"{}"}"#);
    // 上游真实报文：finish-step 携带 tool-calls（带连字符），随后 finish 携带 stop
    t.process_line(r#"{"type":"finish-step","finishReason":"tool-calls","usage":{"inputTokens":7,"outputTokens":3}}"#);
    t.process_line(r#"{"type":"finish","finishReason":"stop"}"#);
    let end = t.finalize().join("");
    assert!(
        end.contains("\"stop_reason\":\"tool_use\""),
        "工具调用流的 stop_reason 应为 tool_use，实际：{end}"
    );
}

/// Anthropic 入站 user 图片块：base64/url 两种 source 都转换为 OpenAI image_url，
/// 与文本块合并为 content 数组，不再被静默丢弃。
#[test]
fn anthropic_to_openai_user_image_blocks() {
    let req = json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 100,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "看图" },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAA" } },
                { "type": "image", "source": { "type": "url", "url": "https://x.test/a.png" } }
            ]
        }]
    });
    let openai = convert::convert_anthropic_to_openai(&req);
    let content = openai["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "看图");
    assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAA");
    assert_eq!(content[2]["image_url"]["url"], "https://x.test/a.png");
}

/// Anthropic 采样/停止参数：top_p 与 stop_sequences 归一后必须真正进入上游 params，
/// 而非止步于中间 openai_req（否则客户端停止序列被静默忽略）。
#[test]
fn anthropic_top_p_and_stop_forwarded_to_params() {
    let req = json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 100,
        "top_p": 0.7,
        "stop_sequences": ["END"],
        "messages": [{ "role": "user", "content": "hi" }]
    });
    let openai = convert::convert_anthropic_to_openai(&req);
    let body = build_cc(&openai, false);
    assert_eq!(body["params"]["top_p"], 0.7);
    assert_eq!(body["params"]["stop"][0], "END");
}

/// Responses 翻译器：tool-call 产出 function_call 四连帧，reasoning 后接正文自动收条目。
#[test]
fn responses_translator_tool_flow() {
    let mut t = ResponsesTranslator::new("gpt-5-codex", "resp_1");
    let mut all = Vec::new();
    all.push(t.response_start());
    all.extend(t.process_line(r#"{"type":"reasoning-delta","text":"think"}"#));
    all.extend(t.process_line(r#"{"type":"text-delta","text":"answer"}"#));
    all.extend(t.process_line(r#"{"type":"tool-call","toolCallId":"c1","toolName":"run","input":"{}"}"#));
    // 完成信号：先 finish-step 给出 tool-calls（权威），再尾部 finish（仅 stop 回退）
    all.extend(t.process_line(r#"{"type":"finish-step","finishReason":"tool-calls","usage":{"inputTokens":7,"outputTokens":3}}"#));
    all.extend(t.process_line(r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":7,"outputTokens":3}}"#));
    all.extend(t.finalize());
    let joined = all.join("");
    assert!(joined.contains("response.reasoning_summary_text.delta"));
    assert!(joined.contains("response.output_text.delta"));
    assert!(joined.contains("response.function_call_arguments.delta"));
    assert!(joined.contains("response.completed"));
}

// ── convert 纯函数边角 ─────────────────────────────────────

/// tool 消息链路：assistant.tool_calls 与 role:tool 结果按 toolCallId 关联成 tool-result。
#[test]
fn build_cc_request_tool_roundtrip() {
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [
            { "role": "user", "content": "天气如何" },
            { "role": "assistant", "content": "", "tool_calls": [
                { "id": "call_9", "type": "function",
                  "function": { "name": "get_weather", "arguments": "{\"city\":\"BJ\"}" } }
            ]},
            { "role": "tool", "tool_call_id": "call_9", "content": "晴 25 度" }
        ]
    });
    let cc = build_cc(&req, true);
    let msgs = cc["params"]["messages"].as_array().unwrap();
    // assistant: tool-call part；tool: tool-result part
    let assistant = &msgs[1];
    let part = &assistant["content"][0];
    assert_eq!(part["type"], "tool-call");
    assert_eq!(part["toolCallId"], "call_9");
    assert_eq!(part["toolName"], "get_weather");
    let tool_msg = &msgs[2];
    assert_eq!(tool_msg["role"], "tool");
    let result = &tool_msg["content"][0];
    assert_eq!(result["toolName"], "get_weather");
    assert_eq!(result["output"]["value"], "晴 25 度");
}

/// tool 消息缺 tool_call_id 关联时回退 name 字段；非字符串 content 序列化为文本。
#[test]
fn build_cc_request_tool_fallback_name_and_object_content() {
    let req = json!({
        "model": "m",
        "messages": [
            { "role": "tool", "tool_call_id": "missing-id", "name": "fallback_tool",
              "content": { "temperature": 25 } }
        ]
    });
    let cc = build_cc(&req, false);
    let msgs = cc["params"]["messages"].as_array().unwrap();
    let result = &msgs[0]["content"][0];
    assert_eq!(result["toolName"], "fallback_tool");
    assert!(result["output"]["value"].as_str().unwrap().contains("temperature"));
}

/// user content parts：image_url 转 image part，其余原样透传。
#[test]
fn build_cc_request_image_parts() {
    let req = json!({
        "model": "m",
        "messages": [
            { "role": "user", "content": [
                { "type": "text", "text": "看图" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAA" } }
            ]}
        ]
    });
    let cc = build_cc(&req, false);
    let parts = cc["params"]["messages"][0]["content"].as_array().unwrap();
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[1]["type"], "image");
    assert_eq!(parts[1]["image"], "data:image/png;base64,AAA");
}

/// tool_choice 语义映射：required→any、指定函数→tool+name、未知字符串→auto。
#[test]
fn build_cc_request_tool_choice_mapping() {
    let base = json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] });
    for (choice, expect_type, expect_name) in [
        (json!("required"), "any", Value::Null),
        (json!("auto"), "auto", Value::Null),
        (json!("none"), "none", Value::Null),
        (json!("weird"), "auto", Value::Null),
        (
            json!({ "type": "function", "function": { "name": "calc" } }),
            "tool",
            json!("calc"),
        ),
    ] {
        let mut req = base.clone();
        req["tool_choice"] = choice;
        let cc = build_cc(&req, false);
        assert_eq!(cc["params"]["tool_choice"]["type"], expect_type);
        if !expect_name.is_null() {
            assert_eq!(cc["params"]["tool_choice"]["name"], expect_name);
        }
    }
}

/// finishReason 映射：tool-calls 归一、空值回 stop、未知透传；Anthropic 侧反向映射。
#[test]
fn finish_reason_mappings() {
    assert_eq!(super::convert::map_finish_reason("tool-calls"), "tool_calls");
    assert_eq!(super::convert::map_finish_reason("length"), "length");
    assert_eq!(super::convert::map_finish_reason("stop"), "stop");
    assert_eq!(super::convert::map_finish_reason(""), "stop");
    assert_eq!(super::convert::map_finish_reason("content-filter"), "content-filter");
    assert_eq!(super::convert::map_anthropic_stop_reason("tool_calls"), "tool_use");
    assert_eq!(super::convert::map_anthropic_stop_reason("length"), "max_tokens");
    assert_eq!(super::convert::map_anthropic_stop_reason("stop"), "end_turn");
    assert_eq!(super::convert::map_anthropic_stop_reason("bogus"), "end_turn");
}

/// 非流式 OpenAI 响应构建：纯工具调用时 content 为 null，reasoning 为扩展字段。
#[test]
fn build_openai_response_variants() {
    let tc = json!({ "id": "c1", "type": "function", "function": { "name": "f", "arguments": "{}" } });
    let resp = super::convert::build_openai_response("id1", "m", "", "thinking...", Some(&[tc]), "tool_calls", 10, 5, 2);
    let msg = &resp["choices"][0]["message"];
    assert!(msg["content"].is_null());
    assert_eq!(msg["reasoning_content"], "thinking...");
    assert_eq!(msg["tool_calls"][0]["function"]["name"], "f");
    assert_eq!(resp["usage"]["prompt_tokens_details"]["cached_tokens"], 2);

    let plain = super::convert::build_openai_response("id2", "m", "text", "", None, "stop", 1, 1, 0);
    assert_eq!(plain["choices"][0]["message"]["content"], "text");
}

// ── 额度 / 套餐 / 模型 / 账户验证的网络链路（mock 上游） ──────────

/// 计数 mock：whoami/subscriptions/credits/summary 四端点 + 命中计数。
async fn spawn_billing_mock(counter: Arc<std::sync::atomic::AtomicUsize>) -> String {
    use std::sync::atomic::Ordering;
    use axum::Json;
    let whoami = Json(json!({
        "success": true,
        "org": { "id": "org_1" },
        "user": { "id": "user_1", "userName": "Tester" },
        "orgLimits": [ { "label": "Monthly", "pct": 0.8, "reached": false } ]
    }));
    let subs = Json(json!({
        "success": true,
        "data": {
            "status": "active",
            "planId": "individual-pro",
            // 上游返回 ISO 8601 字符串（真实响应格式，非毫秒）
            "currentPeriodStart": chrono::DateTime::from_timestamp_millis(
                (super::state::now_millis() - 5 * 86_400_000u64) as i64
            ).unwrap().to_rfc3339(),
            "currentPeriodEnd": chrono::DateTime::from_timestamp_millis(
                (super::state::now_millis() + 25 * 86_400_000u64) as i64
            ).unwrap().to_rfc3339(),
        }
    }));
    let credits = Json(json!({
        "credits": {
            "monthlyCredits": 30, "purchasedCredits": 5, "freeCredits": 2,
            "planId": "individual-pro",
        },
        "windowLimits": {
            "limited": true, "fiveHour": { "used": 10, "cap": 50 }, "weekly": { "used": 100, "cap": 500 }
        }
    }));
    let summary = Json(json!({ "totalCost": 3.5 }));
    let router = Router::new()
        .route("/alpha/whoami", axum::routing::get(move || {
            let c = counter.clone();
            async move { c.fetch_add(1, Ordering::SeqCst); Json(whoami.0.clone()) }
        }))
        .route("/alpha/billing/subscriptions", axum::routing::get(move || { let v = subs.0.clone(); async move { Json(v) } }))
        .route("/alpha/billing/credits", axum::routing::get(move || { let v = credits.0.clone(); async move { Json(v) } }))
        .route("/alpha/usage/summary", axum::routing::get(move || { let v = summary.0.clone(); async move { Json(v) } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { let _ = axum::serve(listener, router).await; });
    format!("http://{addr}")
}

fn plain_state(api_base: &str) -> Arc<AppState> {
    let cfg = Config { api_base: api_base.into(), ..Config::default() };
    AppState::new(cfg)
}

/// 额度拉取全链路：whoami→订阅/额度→summary 的解析、口径计算与窗口限额。
#[tokio::test]
async fn quota_fetch_full_flow() {
    let base = spawn_billing_mock(Arc::new(std::sync::atomic::AtomicUsize::new(0))).await;
    let state = plain_state(&base);
    let q = super::quota::fetch_account_quota(&state, "Tester", "user_…1", "user_k").await;
    assert_eq!(q.error, None);
    assert_eq!(q.plan_id.as_deref(), Some("individual-pro"));
    assert_eq!(q.plan_name, "Pro");
    assert_eq!(q.status.as_deref(), Some("active"));
    assert_eq!(q.monthly_remaining, 30.0);
    assert_eq!(q.purchased_remaining, 5.0);
    assert_eq!(q.free_remaining, 2.0);
    // 总池 = max(套餐月额度 30, 上报 30) + 5 + 2
    assert_eq!(q.total_pool, 37.0);
    assert_eq!(q.total_remaining, 37.0);
    assert_eq!(q.total_spent, 3.5);
    let five = q.five_hour.unwrap();
    assert_eq!((five.used, five.cap), (10.0, 50.0));
    let weekly = q.weekly.unwrap();
    assert_eq!((weekly.used, weekly.cap), (100.0, 500.0));
    assert_eq!(q.org_limits.len(), 1);
    assert_eq!(q.org_limits[0].pct, 80.0, "0-1 比例应换算为百分比");
    let days = q.days_left.unwrap();
    assert!((24..=26).contains(&days), "周期剩余天数应约 25: {days}");
}

/// Go 订阅账户额度解析：订阅正常返回 active + planId，窗口限额在 credits 平级。
#[tokio::test]
async fn quota_fetch_go_subscription() {
    use axum::Json;
    let whoami = Json(json!({
        "success": true,
        "org": { "id": "org_go" },
        "user": { "id": "user_go", "userName": "GoUser" },
        "orgLimits": []
    }));
    let subs = Json(json!({
        "success": true,
        "data": {
            "status": "active",
            "planId": "individual-go",
            "currentPeriodStart": chrono::DateTime::from_timestamp_millis(
                (super::state::now_millis() - 86_400_000u64) as i64
            ).unwrap().to_rfc3339(),
            "currentPeriodEnd": chrono::DateTime::from_timestamp_millis(
                (super::state::now_millis() + 29 * 86_400_000u64) as i64
            ).unwrap().to_rfc3339(),
        }
    }));
    // windowLimits 与 credits 平级（真实结构，CLI 读 e.credits?.windowLimits）
    let credits = Json(json!({
        "credits": { "monthlyCredits": 0, "purchasedCredits": 4.5, "freeCredits": 0, "planId": "individual-go" },
        "windowLimits": { "limited": true, "fiveHour": { "used": 3, "cap": 50 }, "weekly": { "used": 20, "cap": 200 } }
    }));
    let summary = Json(json!({ "totalCost": 1.25 }));
    let router = Router::new()
        .route("/alpha/whoami", axum::routing::get(move || { let v = whoami.0.clone(); async move { Json(v) } }))
        .route("/alpha/billing/subscriptions", axum::routing::get(move || { let v = subs.0.clone(); async move { Json(v) } }))
        .route("/alpha/billing/credits", axum::routing::get(move || { let v = credits.0.clone(); async move { Json(v) } }))
        .route("/alpha/usage/summary", axum::routing::get(move || { let v = summary.0.clone(); async move { Json(v) } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { let _ = axum::serve(listener, router).await; });

    let state = plain_state(&format!("http://{addr}"));
    let q = super::quota::fetch_account_quota(&state, "GoUser", "user_…go", "user_k").await;
    assert_eq!(q.error, None);
    assert_eq!(q.plan_id.as_deref(), Some("individual-go"));
    assert_eq!(q.plan_name, "Go");
    assert_eq!(q.status.as_deref(), Some("active"));
    assert!(q.has_billing);
    assert_eq!(q.monthly_remaining, 0.0);
    assert_eq!(q.purchased_remaining, 4.5);
    assert_eq!(q.total_remaining, 4.5);
    // 总池 = max(套餐月额度 10, 上报 0) + 4.5
    assert_eq!(q.total_pool, 14.5);
    // 窗口限额来自 credits 平级 windowLimits
    let five = q.five_hour.unwrap();
    assert_eq!((five.used, five.cap), (3.0, 50.0));
    let weekly = q.weekly.unwrap();
    assert_eq!((weekly.used, weekly.cap), (20.0, 200.0));
}

/// 个人账户（whoami.org=null）：不应把 userId 当 orgId 拼进 billing 请求，
/// 否则上游 403 导致订阅/额度全空（回归：org_id 只取 org.id，null 时不带参数）。
/// 同时验证 usage/summary 的 since 传 ISO 字符串（上游要求，毫秒会 400）。
#[tokio::test]
async fn quota_fetch_personal_account_no_org() {
    use axum::Json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let whoami = Json(json!({
        "success": true,
        "user": { "id": "5a96ff2d-74e4-4ef9-8108-4f55f3559767", "userName": "zhz8888" },
        "org": null
    }));
    let subs = Json(json!({
        "success": true,
        "data": {
            "status": "active",
            "planId": "individual-go",
            "currentPeriodStart": "2026-09-14T18:52:47.000Z",
            "currentPeriodEnd": "2026-10-14T18:52:47.000Z",
        }
    }));
    let credits = Json(json!({
        "credits": { "monthlyCredits": 10, "purchasedCredits": 0, "freeCredits": 0 },
        "windowLimits": { "limited": true, "fiveHour": { "used": 0, "cap": 3 }, "weekly": { "used": 0, "cap": 6 } }
    }));
    let summary = Json(json!({ "totalCost": 1.25 }));
    // 记录 summary 请求 URL，断言 since 为 ISO 字符串且不带 orgId
    let summary_url = Arc::new(std::sync::Mutex::new(String::new()));
    let sum_url = summary_url.clone();
    let router = Router::new()
        .route("/alpha/whoami", axum::routing::get(move || { let v = whoami.0.clone(); async move { Json(v) } }))
        .route("/alpha/billing/subscriptions", axum::routing::get(move || { let v = subs.0.clone(); async move { Json(v) } }))
        .route("/alpha/billing/credits", axum::routing::get(move || { let v = credits.0.clone(); async move { Json(v) } }))
        .route("/alpha/usage/summary", axum::routing::get(move |req: axum::extract::Request| {
            let v = summary.0.clone();
            let url = sum_url.clone();
            async move {
                *url.lock().unwrap() = req.uri().to_string();
                Json(v)
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { let _ = axum::serve(listener, router).await; });

    let state = plain_state(&format!("http://{addr}"));
    let q = super::quota::fetch_account_quota(&state, "zhz8888", "user_…", "user_k").await;
    assert_eq!(q.error, None);
    assert_eq!(q.plan_id.as_deref(), Some("individual-go"));
    assert_eq!(q.plan_name, "Go");
    assert_eq!(q.status.as_deref(), Some("active"));
    assert!(q.has_billing);
    assert_eq!(q.monthly_remaining, 10.0);
    assert_eq!(q.total_pool, 10.0);
    assert_eq!(q.total_spent, 1.25);
    // period 解析为毫秒且天数可算
    let end = q.period_end.expect("period_end 应解析成功");
    assert!(end > super::state::now_millis());
    assert!(q.days_left.is_some());
    // summary 请求：不带 orgId，since 为百分号编码后的 ISO 字符串
    // （`:` 编码为 %3A，避免 `+00:00` 这类带偏移的时间串被服务端解码成空格）
    let got_url = summary_url.lock().unwrap().clone();
    assert!(!got_url.contains("orgId="), "个人账户不应带 orgId: {got_url}");
    assert!(got_url.contains("since="), "应带 since 参数: {got_url}");
    let since_raw = got_url.split("since=").nth(1).expect("since 参数缺失");
    let since_at = since_raw.find('&').map(|i| &since_raw[..i]).unwrap_or(since_raw);
    let since = urlencoding_decode(since_at);
    assert_eq!(since, "2026-09-14T18:52:47.000Z", "since 应为 ISO 字符串: {got_url}");
}

/// 最小百分号解码：仅用于断言 URL query 中的编码值。
fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 订阅接口异常但 credits 带 planId 时：plan 从 credits 兜底，窗口限额仍可解析。
#[tokio::test]
async fn quota_fetch_plan_fallback_from_credits() {
    use axum::Json;
    let whoami = Json(json!({
        "success": true,
        "org": { "id": "org_f" },
        "user": { "id": "user_f", "userName": "Fallback" }
    }));
    // subscriptions 返回 success=false（模拟 Go 等账户无订阅记录/接口异常）
    let subs = Json(json!({ "success": false, "data": null }));
    let credits = Json(json!({
        "credits": { "monthlyCredits": 0, "purchasedCredits": 2, "freeCredits": 0, "planId": "individual-go" },
        "windowLimits": { "limited": true, "fiveHour": { "used": 1, "cap": 50 } }
    }));
    let summary = Json(json!({ "totalCost": 0.5 }));
    let router = Router::new()
        .route("/alpha/whoami", axum::routing::get(move || { let v = whoami.0.clone(); async move { Json(v) } }))
        .route("/alpha/billing/subscriptions", axum::routing::get(move || { let v = subs.0.clone(); async move { Json(v) } }))
        .route("/alpha/billing/credits", axum::routing::get(move || { let v = credits.0.clone(); async move { Json(v) } }))
        .route("/alpha/usage/summary", axum::routing::get(move || { let v = summary.0.clone(); async move { Json(v) } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { let _ = axum::serve(listener, router).await; });

    let state = plain_state(&format!("http://{addr}"));
    let q = super::quota::fetch_account_quota(&state, "Fallback", "user_…f", "user_k").await;
    assert_eq!(q.error, None);
    // plan 从 credits.planId 兜底
    assert_eq!(q.plan_id.as_deref(), Some("individual-go"));
    assert_eq!(q.plan_name, "Go");
    assert!(q.has_billing);
    assert_eq!(q.total_remaining, 2.0);
    assert!(q.five_hour.is_some(), "windowLimits 平级时 5h 窗口应可解析");
}

/// whoami 不可达时额度快照降级为失败占位（不 panic、error 码正确）。
#[tokio::test]
async fn quota_fetch_whoami_failed_degrades() {
    // 指向未监听端口
    let state = plain_state("http://127.0.0.1:1");
    let q = super::quota::fetch_account_quota(&state, "N", "user_…x", "user_k").await;
    assert_eq!(q.error.as_deref(), Some("whoami_failed"));
    assert!(!q.has_billing);
    assert_eq!(q.total_pool, 0.0);
}

/// 后台刷新：force 拉取后 TTL 内不再重拉；账户删除后缓存与在途标记被清理。
#[tokio::test]
async fn quota_refresh_caches_ttl_and_cleanup() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let counter = Arc::new(AtomicUsize::new(0));
    let base = spawn_billing_mock(counter.clone()).await;
    let state = plain_state(&base);
    let acct = |id: &str| crate::proxy::config::Account {
        key: format!("user_{id}"), user_id: id.into(),
        user_name: id.into(), source: "manual".into(), added_at: 0,
    };
    state.config.write().unwrap().cc_accounts = vec![acct("a"), acct("b")];

    super::quota::refresh_all_caches(&state, true).await;
    assert_eq!(state.quota_cache.lock().unwrap().len(), 2);
    let first = counter.load(Ordering::SeqCst);

    // TTL 内非强制刷新：不发新请求
    super::quota::refresh_all_caches(&state, false).await;
    assert_eq!(counter.load(Ordering::SeqCst), first);

    // 删除账户 b 后刷新：缓存收缩为 a
    state.config.write().unwrap().cc_accounts = vec![acct("a")];
    super::quota::refresh_all_caches(&state, true).await;
    let cache = state.quota_cache.lock().unwrap();
    assert_eq!(cache.len(), 1);
    assert!(cache.contains_key("a"));
}

/// snapshot_all 命中未过期缓存时不发起上游请求，顺序与账户列表一致。
#[tokio::test]
async fn quota_snapshot_uses_cache() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let counter = Arc::new(AtomicUsize::new(0));
    let base = spawn_billing_mock(counter.clone()).await;
    let state = plain_state(&base);
    state.config.write().unwrap().cc_accounts = vec![
        crate::proxy::config::Account { key: "user_a".into(), user_id: "a".into(), user_name: "A".into(), source: "manual".into(), added_at: 0 },
        crate::proxy::config::Account { key: "user_b".into(), user_id: "b".into(), user_name: "B".into(), source: "manual".into(), added_at: 0 },
    ];
    super::quota::refresh_all_caches(&state, true).await;
    let first = counter.load(Ordering::SeqCst);
    let snap = super::quota::snapshot_all(&state).await;
    assert_eq!(snap.len(), 2);
    assert_eq!(snap[0].user_name, "A");
    assert_eq!(snap[1].user_name, "B");
    assert_eq!(counter.load(Ordering::SeqCst), first, "命中缓存不应重拉");
}

/// 标记耗尽：把最窄可用窗口推满并清除该账户全部会话绑定。
#[tokio::test]
async fn quota_mark_exhausted_updates_cache_and_bindings() {
    let state = plain_state("http://127.0.0.1:1");
    let q = super::quota::AccountQuota {
        user_name: "a".into(), masked_key: "user_…a".into(),
        plan_id: None, plan_name: String::new(), status: None,
        monthly_remaining: 0.0, purchased_remaining: 0.0, free_remaining: 0.0,
        total_remaining: 9.0, total_pool: 10.0, total_spent: 0.0, usage_percent: 10.0,
        has_billing: true, days_left: None, period_start: None, period_end: None,
        five_hour: Some(super::quota::LimitWindow { used: 1.0, cap: 50.0, reset_at: None }),
        weekly: None, org_limits: Vec::new(), exhausted: false, error: None,
    };
    state.quota_cache.lock().unwrap().insert("a".into(), (q.clone(), super::state::now_millis()));
    state.account_bindings.lock().unwrap().insert(
        "sess-1".into(),
        super::state::AccountBinding { user_id: "a".into(), bound_at: super::state::now_millis() },
    );
    super::quota::mark_exhausted(&state, "a");
    let cached = state.quota_cache.lock().unwrap().get("a").unwrap().0.clone();
    assert!(cached.exhausted, "应置显式耗尽标记");
    assert!(super::quota::is_exhausted(&cached), "5h 窗口应被推满");
    assert!(state.account_bindings.lock().unwrap().is_empty(), "绑定应被清除");
    // 缓存无该账户时不 panic
    super::quota::mark_exhausted(&state, "ghost");
}

/// 标记耗尽对「无窗口限额且额度池为 0」的账户同样生效。
///
/// 旧实现靠改写额度值间接表达耗尽，而月配额判定要求 total_pool > 0，
/// 这类账户（credits 三项皆 0 且无可识别套餐）的耗尽判定恒为假。
#[tokio::test]
async fn quota_mark_exhausted_without_windows_or_pool() {
    use crate::proxy::quota::{remaining_score, AccountQuota};

    let state = plain_state("http://127.0.0.1:1");
    let q = AccountQuota {
        user_name: "a".into(), masked_key: "user_…a".into(),
        plan_id: None, plan_name: String::new(), status: None,
        monthly_remaining: 0.0, purchased_remaining: 0.0, free_remaining: 0.0,
        total_remaining: 0.0, total_pool: 0.0, total_spent: 0.0, usage_percent: 0.0,
        has_billing: true, days_left: None, period_start: None, period_end: None,
        five_hour: None, weekly: None, org_limits: Vec::new(), exhausted: false, error: None,
    };
    state.quota_cache.lock().unwrap().insert("a".into(), (q, super::state::now_millis()));
    super::quota::mark_exhausted(&state, "a");
    let cached = state.quota_cache.lock().unwrap().get("a").unwrap().0.clone();
    assert!(super::quota::is_exhausted(&cached), "无窗口无池也应判为耗尽");
    assert_eq!(remaining_score(&cached), 0.0, "耗尽账户余量评分应为 0");
}

/// 套餐上下文拉取：完整链路得到 plan/credits；上游不可达时降级为放行。
#[tokio::test]
async fn plan_context_fetch_flow_and_failure() {
    let base = spawn_billing_mock(Arc::new(std::sync::atomic::AtomicUsize::new(0))).await;
    let state = plain_state(&base);
    let ctx = super::plans::fetch_plan_context(&state, "user_k").await;
    assert!(!ctx.fetch_failed);
    assert_eq!(ctx.plan_id.as_deref(), Some("individual-pro"));
    assert_eq!(ctx.plan_name, "Pro");
    assert_eq!(ctx.purchased_credits, 5.0);
    assert_eq!(ctx.free_credits, 2.0);

    // 上游不可达 → fetch_failed 放行
    let bad = plain_state("http://127.0.0.1:1");
    let failed = super::plans::fetch_plan_context(&bad, "user_k").await;
    assert!(failed.fetch_failed);
    assert!(failed.plan_id.is_none());
    // 放行语义：任意模型可用
    let acc = super::plans::evaluate_access("claude-opus-4-8", &failed);
    assert!(acc.allowed);
}

/// Provider 端点拉取成功路径与拉取失败回退。
#[tokio::test]
async fn fetch_models_provider_and_fallback() {
    let captured: Arc<Mutex<Value>> = Arc::new(Mutex::new(Value::Null));
    let mock = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = mock.local_addr().unwrap().to_string();
    tokio::spawn(async move { let _ = axum::serve(mock, mock_upstream(Some(captured))).await; });
    let state = plain_state(&format!("http://{addr}"));

    // 走 Provider 端点（公开接口，无需 key）
    let (models, is_fallback) = super::cc_client::fetch_models(&state).await;
    assert!(!is_fallback);
    assert!(models.iter().any(|m| m.id == "mock-model-1"));

    // 端点不可达时回退（用全新 state，避免命中上一步的模型缓存）
    let fresh = plain_state("http://127.0.0.1:9");
    let (fallback, is_fallback2) = super::cc_client::fetch_models(&fresh).await;
    assert!(is_fallback2);
    assert!(fallback.len() >= 60);
}

/// 预请求（fingerprint/record + lifecycle-events）8h 节流：首次发送，二次跳过。
#[tokio::test]
async fn ensure_initialized_throttles_pre_requests() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let h2 = hits.clone();
    let router = Router::new()
        .route("/alpha/fingerprint/record", post(move || {
            let h = h2.clone();
            async move { h.fetch_add(1, Ordering::SeqCst); axum::response::Response::new(axum::body::Body::from("{}")) }
        }))
        .route("/alpha/lifecycle-events", post(|| async {
            axum::response::Response::new(axum::body::Body::from("{}"))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { let _ = axum::serve(listener, router).await; });
    let state = plain_state(&format!("http://{addr}"));

    super::cc_client::ensure_initialized(&state, "user_k", "user_1").await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // 8h 节流窗口内：不再发送
    super::cc_client::ensure_initialized(&state, "user_k", "user_1").await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // 清空键控状态后重新初始化
    state.key_states.lock().unwrap().clear();
    super::cc_client::ensure_initialized(&state, "user_k", "user_1").await;
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

/// 账户 key 验证（whoami）：成功 / 缺 id / 401 / 非 JSON 各分支。
#[tokio::test]
async fn verify_account_key_variants() {
    use axum::response::Response;
    async fn spawn_with(status: u16, body: &'static str) -> Arc<AppState> {
        let router = Router::new().route("/alpha/whoami", axum::routing::get(move || {
            let s = status;
            async move {
                Response::builder().status(s).header("content-type", "application/json")
                    .body(axum::body::Body::from(body)).unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { let _ = axum::serve(listener, router).await; });
        plain_state(&format!("http://{addr}"))
    }

    let ok = spawn_with(200, r#"{"user":{"id":"id_1","userName":"N"}}"#).await;
    let (uid, name) =
        crate::credentials::verify_account_key(&ok.client(), &ok.config.read().unwrap().api_base, "user_k").await.unwrap();
    assert_eq!((uid.as_str(), name.as_str()), ("id_1", "N"));

    let no_id = spawn_with(200, r#"{"user":{}}"#).await;
    assert!(crate::credentials::verify_account_key(&no_id.client(), &no_id.config.read().unwrap().api_base, "user_k").await.is_err());

    let unauth = spawn_with(401, r#"{"message":"bad"}"#).await;
    assert!(crate::credentials::verify_account_key(&unauth.client(), &unauth.config.read().unwrap().api_base, "user_k").await.is_err());

    let bad_json = spawn_with(200, "not json").await;
    assert!(crate::credentials::verify_account_key(&bad_json.client(), &bad_json.config.read().unwrap().api_base, "user_k").await.is_err());

    // 上游 500 → 通用验证失败分支
    let e500 = spawn_with(500, r#"{"message":"oops"}"#).await;
    assert!(crate::credentials::verify_account_key(&e500.client(), &e500.config.read().unwrap().api_base, "user_k").await.is_err());
}

// ── server 各协议错误分支与流式边角 ─────────────────────────

/// Anthropic 流式：reasoning → thinking 块，正文 → text 块，收尾 message_delta/stop。
#[tokio::test]
async fn anthropic_streaming_flow() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/messages"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "reasoner",
            "max_tokens": 1000,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    assert!(text.contains("event: message_start"));
    assert!(text.contains("thinking_delta"));
    assert!(text.contains("deep thought"));
    assert!(text.contains("text_delta"));
    assert!(text.contains("message_stop"));
    state.mark_stopped();
}

/// Anthropic 上游错误：429 映射为 Anthropic 风格错误体（type: error + retry_after）。
#[tokio::test]
async fn anthropic_upstream_error_mapped() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/messages"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "upstream-error",
            "max_tokens": 100,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(body["error"]["message"], "rate limited");
    state.mark_stopped();
}

/// Responses 上游错误：映射为 OpenAI 风格错误体。
#[tokio::test]
async fn responses_upstream_error_mapped() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/responses"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "upstream-error",
            "input": [{ "type": "message", "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(body["error"]["message"], "rate limited");
    state.mark_stopped();
}

/// OpenAI 流式 tool-call：上游 tool-call 事件翻译为 chat.completion.chunk 的 tool_calls。
#[tokio::test]
async fn chat_completions_streaming_tool_calls() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "tool-caller",
            "messages": [{ "role": "user", "content": "天气" }],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    assert!(text.contains("tool_calls"));
    assert!(text.contains("get_weather"));
    assert!(text.contains("finish_reason"));
    assert!(text.contains("[DONE]"));
    state.mark_stopped();
}

/// 上游 NDJSON 中混入坏行：跳过该行继续翻译，不影响后续内容与收尾。
#[tokio::test]
async fn bad_ndjson_line_is_skipped() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer sk-test-local-key-123")
        .json(&json!({
            "model": "bad-line",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    assert!(text.contains("after bad line"));
    assert!(text.contains("[DONE]"));
    state.mark_stopped();
}

/// 请求体超过 max_body_mb 上限时返回 413。
#[tokio::test]
async fn oversized_body_returns_413() {
    let (base, state) = start_proxy_impl(None, |c| c.max_body_mb = 1).await;
    let client = reqwest::Client::new();
    let big = "x".repeat(2 * 1024 * 1024);
    // 并行高负载下偶发收到非 413 的瞬时响应（如空 503），非 413 时重试
    let mut status = 0u16;
    let mut body = String::new();
    for _ in 0..4 {
        let res = send_retry(
            &client,
            reqwest::Method::POST,
            &format!("{base}/v1/chat/completions"),
            "sk-test-local-key-123",
            Some(&json!({
                "model": "m",
                "messages": [{ "role": "user", "content": big }],
            })),
        )
        .await;
        status = res.status().as_u16();
        body = res.text().await.unwrap_or_default();
        if status == 413 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    assert_eq!(status, 413, "实际返回: {body}");
    state.mark_stopped();
}

// ── cc_client 补充：预请求失败降级、指纹持久化失败降级、模型拉取坏响应 ──

/// 预请求返回 500：仅记 warn 不 panic，节流时间仍被安排。
#[tokio::test]
async fn ensure_initialized_tolerates_upstream_error() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let h2 = hits.clone();
    let router = Router::new()
        .route("/alpha/fingerprint/record", post(move || {
            let h = h2.clone();
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                axum::response::Response::builder().status(500)
                    .body(axum::body::Body::from("boom")).unwrap()
            }
        }))
        .route("/alpha/lifecycle-events", post(|| async {
            axum::response::Response::builder().status(500)
                .body(axum::body::Body::from("boom")).unwrap()
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { let _ = axum::serve(listener, router).await; });
    let state = plain_state(&format!("http://{addr}"));

    super::cc_client::ensure_initialized(&state, "user_k", "user_1").await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    state.key_states.lock().unwrap().clear();
    super::cc_client::ensure_initialized(&state, "user_k", "user_1").await;
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

/// 指纹确定性派生：无需任何持久化设施，直接生成有效指纹（thumbmark 非空），不 panic。
#[tokio::test]
async fn fingerprint_derivation_without_persistence() {
    let state = plain_state("http://127.0.0.1:1");
    let fp = super::cc_client::key_fingerprint_for_test(&state, "user_1");
    assert!(!fp.thumbmark.is_empty());
}

/// Provider 模型接口返回坏 JSON / 空 data：回退内置表而非 panic。
#[tokio::test]
async fn fetch_models_bad_provider_response_falls_back() {
    async fn spawn_with(body: &'static str) -> Arc<AppState> {
        let router = Router::new().route("/provider/v1/models", axum::routing::get(move || {
            async move { axum::response::Response::new(axum::body::Body::from(body)) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { let _ = axum::serve(listener, router).await; });
        plain_state(&format!("http://{addr}"))
    }
    let bad = spawn_with("not json").await;
    let (models, fallback) = super::cc_client::fetch_models(&bad).await;
    assert!(fallback);
    assert!(models.len() >= 60);
    let empty = spawn_with(r#"{"data":[]}"#).await;
    let (models2, fallback2) = super::cc_client::fetch_models(&empty).await;
    assert!(fallback2);
    assert!(models2.len() >= 60);
}

// ── convert 协议转换的更多分支 ─────────────────────────────

/// Anthropic → OpenAI：system 数组合并、thinking 转 reasoning_content、tool_use/tool_result 关联。
#[test]
fn convert_anthropic_system_thinking_and_tools() {
    let req = json!({
        "model": "claude-sonnet-4-6",
        "system": [
            { "type": "text", "text": "sys-a" },
            { "type": "text", "text": "sys-b" },
            { "type": "image", "source": {} }
        ],
        "messages": [
            { "role": "user", "content": "hi" },
            { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "ponder" },
                { "type": "tool_use", "id": "tu_1", "name": "calc", "input": { "x": 1 } }
            ]},
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "tu_1", "content": [
                    { "type": "text", "text": "result-42" }
                ]}
            ]}
        ]
    });
    let openai = super::convert::convert_anthropic_to_openai(&req);
    let msgs = openai["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    // system 数组块保留为块数组（build_cc_request 据此把断点原样下发）
    let sys_blocks = msgs[0]["content"].as_array().unwrap();
    assert_eq!(sys_blocks.len(), 2, "image 块被过滤，仅 text 块保留");
    assert_eq!(sys_blocks[0]["text"], "sys-a");
    assert_eq!(sys_blocks[1]["text"], "sys-b");
    // assistant：thinking → reasoning_content，tool_use → tool_calls
    let assistant = &msgs[2];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["reasoning_content"], "ponder");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "calc");
    // tool_result：按 tool_use_id 关联出 name，数组 content 拼接
    let tool = &msgs[3];
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], "tu_1");
    assert_eq!(tool["name"], "calc");
    assert_eq!(tool["content"], "result-42");
}

/// Responses → OpenAI：reasoning/function_call 并入 assistant，function_call_output 转 tool 消息。
#[test]
fn convert_responses_reasoning_and_function_calls() {
    let resp = json!({
        "model": "gpt-5-codex",
        "instructions": "be brief",
        "input": [
            { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "step1" }] },
            { "type": "function_call", "call_id": "call_r1", "name": "run", "arguments": "{\"q\":1}" },
            { "type": "function_call_output", "call_id": "call_r1", "output": "done" }
        ]
    });
    let openai = super::convert::convert_responses_to_openai(&resp);
    let msgs = openai["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "be brief");
    // assistant 消息同时含 reasoning_content 与 tool_calls
    let assistant = &msgs[1];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["reasoning_content"], "step1");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "run");
    let tool = &msgs[2];
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], "call_r1");
    assert_eq!(tool["content"], "done");
}

/// Responses 字符串形态 input 直接成为 user 消息；assistant 文本块拼接。
#[test]
fn convert_responses_string_input_and_assistant_blocks() {
    let resp = json!({
        "input": [
            { "role": "assistant", "content": [{ "type": "output_text", "text": "a" }, { "type": "output_text", "text": "b" }] },
            "plain user text"
        ]
    });
    let openai = super::convert::convert_responses_to_openai(&resp);
    let msgs = openai["messages"].as_array().unwrap();
    // assistant 消息累积进 pending、函数尾部统一 flush，因此排在字符串 user 之后
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"], "plain user text");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["content"], "ab");
}

/// OpenAI 请求中 system content 为 parts 数组时抽取 text；已有 cache_control 时不重复注入。
#[test]
fn build_cc_request_array_system_and_cache_marker() {
    let req = json!({
        "model": "m",
        "prompt_cache_key": "ck-12345678",
        "messages": [
            { "role": "system", "content": [ { "type": "text", "text": "sys1" }, { "type": "image_url" } ] },
            { "role": "user", "content": [ { "type": "text", "text": "u1", "cache_control": { "type": "ephemeral" } } ] }
        ]
    });
    let cc = build_cc(&req, false);
    // system 数组抽取 text 成块数组；非 text 块（image_url）被过滤
    let system = cc["params"]["system"].as_array().unwrap();
    assert_eq!(system.len(), 1);
    assert_eq!(system[0]["text"], "sys1");
    let user_parts = cc["params"]["messages"][0]["content"].as_array().unwrap();
    // 已有 cache_control：system 不再注入第二处标记
    assert!(user_parts[0].get("cache_control").is_some());
    assert!(system.iter().all(|b| b.get("cache_control").is_none()));
}

// ── 版本刷新（注入 URL）与坏数据库文件 ─────────────────────

/// registry 返回新版本号时写回状态；坏 JSON / 非成功状态保持原值。
#[tokio::test]
async fn refresh_cc_version_parses_and_persists() {
    use axum::response::Response;
    let state = plain_state("http://127.0.0.1:1");
    let before = super::cc_client::cc_version(&state);

    async fn spawn_registry(body: &'static str, status: u16) -> String {
        let router = Router::new().route("/command-code/latest", axum::routing::get(move || {
            async move {
                Response::builder().status(status).header("content-type", "application/json")
                    .body(axum::body::Body::from(body)).unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { let _ = axum::serve(listener, router).await; });
        format!("http://{addr}")
    }

    // 正常版本号
    let ok = spawn_registry(r#"{"version":"9.9.9"}"#, 200).await;
    super::cc_client::refresh_cc_version_from(&state, &format!("{ok}/command-code/latest")).await;
    assert_eq!(super::cc_client::cc_version(&state), "9.9.9");

    // 坏 JSON：保持原值
    let bad = spawn_registry("not json", 200).await;
    super::cc_client::refresh_cc_version_from(&state, &format!("{bad}/command-code/latest")).await;
    assert_eq!(super::cc_client::cc_version(&state), "9.9.9");

    // 非 2xx：保持原值
    let err = spawn_registry(r#"{"version":"0.0.1"}"#, 500).await;
    super::cc_client::refresh_cc_version_from(&state, &format!("{err}/command-code/latest")).await;
    assert_eq!(super::cc_client::cc_version(&state), "9.9.9");
    let _ = before;
}

/// 上游版本号必须缓存进配置并跨启动复用，而不是每次启动都回到内置占位版本。
///
/// 回归：版本号原先只存在于内存（`AppState::new` 恒以硬编码 `0.32.3` 起步），
/// 每次启动首页都先显示占位版本，要等 npm 拉取成功才变成真实值；拉取失败
/// （离线、registry 不可达）则整个进程生命周期都显示占位版本。
#[tokio::test]
async fn cc_version_cache_survives_restart() {
    use axum::response::Response;
    let router = Router::new().route(
        "/command-code/latest",
        axum::routing::get(|| async {
            Response::builder()
                .header("content-type", "application/json")
                .body(axum::body::Body::from(r#"{"version":"1.56.1"}"#))
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    // 第一次运行：从空缓存起步 → 拉取成功 → 写回配置缓存并落库
    let mut cfg = Config {
        api_base: "http://127.0.0.1:1".into(),
        ..Config::default()
    };
    assert_eq!(cfg.cc_version_cache, "", "新配置的版本缓存应为空");
    let state = AppState::new(cfg.clone());
    // 挂上真实设置库，验证刷新确实把版本号持久化了（而不只是改内存）
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    super::settings::init_settings_on(&conn).unwrap();
    *state.usage.lock().unwrap() = Some(conn);
    assert_eq!(
        super::cc_client::cc_version(&state),
        super::state::DEFAULT_CC_VERSION,
        "空缓存时应回落到内置占位版本"
    );
    super::cc_client::refresh_cc_version_from(&state, &format!("http://{addr}/command-code/latest")).await;
    assert_eq!(super::cc_client::cc_version(&state), "1.56.1");
    cfg.cc_version_cache = state.config.read().unwrap().cc_version_cache.clone();
    assert_eq!(cfg.cc_version_cache, "1.56.1", "拉取成功后应写回配置缓存");
    // 落库校验：从设置库读回的配置应带同一版本号
    {
        let guard = state.usage.lock().unwrap();
        let loaded = super::settings::load_config(guard.as_ref().unwrap());
        assert_eq!(
            loaded.cc_version_cache, "1.56.1",
            "版本号应写入本地设置库缓存，供下次启动复用"
        );
    }

    // 模拟重启：用带缓存的配置新建状态，即使网络不可达也应显示上次的版本号
    let offline = AppState::new(Config {
        api_base: "http://127.0.0.1:1".into(),
        ..cfg.clone()
    });
    assert_eq!(
        super::cc_client::cc_version(&offline),
        "1.56.1",
        "重启后应直接复用缓存的版本号，而非回到占位版本"
    );
    // 拉取失败不覆盖缓存
    super::cc_client::refresh_cc_version_from(&offline, "http://127.0.0.1:1/command-code/latest").await;
    assert_eq!(super::cc_client::cc_version(&offline), "1.56.1");
}

/// 数据库文件损坏（非 SQLite 格式）时：初始化报错而非 panic（models/settings/usage 各表）。
#[test]
fn init_tables_on_garbage_db_file_is_error() {
    let path = std::env::temp_dir().join(format!("garbage-db-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, b"this is definitely not a sqlite database file").unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert!(super::models::init_models_on(&conn).is_err());
    assert!(super::settings::init_settings_on(&conn).is_err());
    drop(conn);
    assert!(super::usage::init_usage(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

/// Anthropic 非流式响应构建：thinking 首块 + tool_use 输入解析 + stop_reason 折算。
#[test]
fn build_anthropic_response_with_thinking_and_tools() {
    let tc = json!({ "id": "tu_9", "type": "function", "function": { "name": "calc", "arguments": "{\"x\":2}" } });
    let body = super::convert::build_anthropic_response(
        "msg_1", "claude-x", "", "deep think", Some(&[tc]), "tool_calls", 11, 7, 3, None,
    );
    assert_eq!(body["role"], "assistant");
    let content = body["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], "deep think");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["name"], "calc");
    assert_eq!(content[1]["input"]["x"], 2, "arguments 应解析为 JSON 对象");
    assert_eq!(body["stop_reason"], "tool_use");
    let usage = &body["usage"];
    assert_eq!(usage["input_tokens"], 11);

    // 纯文本响应
    let plain = super::convert::build_anthropic_response("msg_2", "claude-x", "hi", "", None, "stop", 1, 1, 0, None);
    let content = plain["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "hi");
}

// ── 只读连接触发 SQLite 写入错误分支 ────────────────────────

/// 只读连接上写入：models 的 upsert 与 usage 的记录/清理返回错误而非 panic。
#[test]
fn readonly_connection_write_errors_are_mapped() {
    let path = std::env::temp_dir().join(format!("ro-db-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    // 先以可写连接建好表
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        super::models::init_models_on(&conn).unwrap();
        super::usage::init_usage_on(&conn).unwrap();
    }
    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();

    // models：只读 upsert → 写入失败映射为错误码
    let m = super::pricing::ModelPricing {
        id: "ro-model".into(),
        deal: None,
        time_of_day: None,
        tiers: vec![],
    };
    assert!(super::models::upsert_pricing(&conn, &[m], "manual").is_err());

    // usage：只读记录 → 错误映射
    let entry = super::usage::UsageEntry {
        ts: super::state::now_millis(),
        model: "m".into(),
        endpoint: "/v1/chat/completions".into(),
        status: "ok".into(),
        prompt_tokens: 1,
        completion_tokens: 1,
        cached_tokens: 0,
        cache_write_tokens: 0,
        stream: true,
    };
    assert!(super::usage::record_usage(&conn, &entry).is_err());
    // 只读清理 → 错误映射
    assert!(super::usage::clear_before(&conn, 5).is_err());
    drop(conn);
    let _ = std::fs::remove_file(&path);
}
