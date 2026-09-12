//! proxy 模块的单元与集成测试。
//!
//! 单元测试覆盖：三种协议的请求转换、SSE/流事件翻译、错误映射、Key 提取、
//! 设备指纹与项目 slug 生成；集成测试用 axum 搭建 mock CC 上游，端到端验证
//! 代理服务的流式/非流式转发、零输出限流、上游错误映射与鉴权行为。

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::Router;
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

// ── 单元测试：请求转换 ────────────────────────────────

/// 验证基础 OpenAI 请求转换出的 CC 信封：model/system 提取、user 消息转 text parts、
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
    let cc = convert::build_cc_request(&req, true);
    assert_eq!(cc["params"]["model"], "deepseek/deepseek-v4-flash");
    assert_eq!(cc["params"]["system"], "你是助手");
    assert_eq!(cc["params"]["messages"][0]["role"], "user");
    assert_eq!(cc["params"]["messages"][0]["content"][0]["type"], "text");
    assert_eq!(cc["params"]["max_tokens"], 1000);
    assert_eq!(cc["permissionMode"], "standard");
    assert!(cc["config"]["date"].is_string());
}

/// 验证 developer 与 system 两种角色的消息按序合并为顶层 system 字段，
/// 且均不会作为聊天消息原样转发（CC API 会拒绝未知角色，issue #1）。
#[test]
fn build_cc_request_developer_role_merged_into_system() {
    // issue #1: OpenAI 新客户端以 role: "developer" 发送 system prompt，
    // 需与 system 一并提取为顶层 system，不能原样转发（CC API 会报 400）
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [
            { "role": "developer", "content": "系统指令" },
            { "role": "system", "content": "补充说明" },
            { "role": "user", "content": "你好" },
        ],
    });
    let cc = convert::build_cc_request(&req, true);
    assert_eq!(cc["params"]["system"], "系统指令\n补充说明");
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
    let cc = convert::build_cc_request(&req, true);
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
}

/// 验证无 system 时 params.system 发空格占位（开关开启），关闭时缺省字段。
#[test]
fn build_cc_request_empty_system_placeholder() {
    let req = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [{ "role": "user", "content": "你好" }],
    });
    // 开关开启：无 system 时发空格占位，阻止上游注入默认提示词
    let cc = convert::build_cc_request(&req, true);
    assert_eq!(cc["params"]["system"], " ");
    // 开关关闭：不写 system 字段
    let cc2 = convert::build_cc_request(&req, false);
    assert!(cc2["params"].get("system").is_none());
}

/// 验证 assistant 消息的 reasoning 回传：reasoning_content 字段与 content 数组内的
/// reasoning part 均转为 CC 的 `{type:"reasoning"}`，且顺序为 [reasoning, text, tool-call]。
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
    let cc = convert::build_cc_request(&req, true);
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
    let cc2 = convert::build_cc_request(&req2, true);
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
    let cc = convert::build_cc_request(&req, true);
    let content = cc["params"]["messages"][0]["content"].as_array().unwrap();
    // 最后一个 text 块获得 cache_control
    assert!(content[0].get("cache_control").is_none());
    assert_eq!(content[1]["cache_control"]["type"], "ephemeral");

    // 已有 cache_control 时不重复注入
    let req2 = json!({
        "model": "m",
        "prompt_cache_key": "cache-abc-123456",
        "messages": [
            { "role": "user", "content": [
                { "type": "text", "text": "x", "cache_control": { "type": "ephemeral" } },
            ] },
        ],
    });
    let cc2 = convert::build_cc_request(&req2, true);
    let parts2 = cc2["params"]["messages"][0]["content"].as_array().unwrap();
    assert_eq!(parts2.iter().filter(|p| p.get("cache_control").is_some()).count(), 1);
}

/// 验证 fingerprint 序列化为 camelCase（字段名与上游期望的格式一致）。
#[test]
fn fingerprint_camelcase_serialization() {
    let fp = fingerprint::generate();
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
    assert_eq!(openai["messages"][3]["role"], "user");
    assert_eq!(openai["messages"][4]["role"], "tool");
    assert_eq!(openai["messages"][4]["tool_call_id"], "tu_1");
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
    // issue #2: Codex CLI 等 Responses 客户端 → Chat 格式（供 build_cc_request 复用）
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
    let fp = fingerprint::generate();
    assert_eq!(fp.thumbmark.len(), 64);
    assert_eq!(fp.components.platform, "win32");
    assert_eq!(fp.components.collector_version, 1);
    let n = fp.components.mac_hashes.len();
    assert!((2..=5).contains(&n));
}

/// 验证伪造项目 slug 的格式：不含盘符前缀、仅小写字母数字与连字符、不以连字符开头或结尾。
#[test]
fn project_slug_format() {
    let slug = cc_client::fake_project_slug("a3f2c001-0000-0000-0000-000000000000");
    assert!(!slug.starts_with("c:"));
    assert!(slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    assert!(slug.chars().all(|c| !c.is_uppercase()));
    assert!(!slug.starts_with('-') && !slug.ends_with('-'));
}

/// 验证指纹持久化：key_id 稳定且不泄露明文 Key；remember 写入后 load_store 能原样读回
/// 同一份指纹（thumbmark 与 components 全部字段），保证同一 Key 跨重启设备身份不变。
#[test]
fn fingerprint_persist_roundtrip() {
    // 同一 Key 的 id 稳定，不同 Key 的 id 不同；id 为 64 位 hex 且不含明文 Key
    let id1 = fingerprint::key_id("user_abc123");
    assert_eq!(id1, fingerprint::key_id("user_abc123"));
    assert_ne!(id1, fingerprint::key_id("user_xyz789"));
    assert_eq!(id1.len(), 64);
    assert!(!id1.contains("user_abc123"));

    let path = std::env::temp_dir().join(format!("cc-fp-test-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    assert!(fingerprint::load_store(&path).is_empty());

    let fp = fingerprint::generate();
    fingerprint::remember(&path, &id1, &fp).unwrap();
    let store = fingerprint::load_store(&path);
    let got = store.get(&id1).expect("指纹应已持久化");
    assert_eq!(got.thumbmark, fp.thumbmark);
    assert_eq!(got.components.machine_id_hash, fp.components.machine_id_hash);
    assert_eq!(got.components.mac_hashes, fp.components.mac_hashes);
    assert_eq!(got.components.platform, fp.components.platform);
    assert_eq!(got.components.mem_gib, fp.components.mem_gib);
    assert_eq!(got.components.timezone, fp.components.timezone);
    assert_eq!(got.components.collector_version, fp.components.collector_version);

    // 再次 remember 另一 Key 时不应覆盖已存在的条目
    let id2 = fingerprint::key_id("user_other");
    fingerprint::remember(&path, &id2, &fingerprint::generate()).unwrap();
    let store = fingerprint::load_store(&path);
    assert_eq!(store.len(), 2);
    assert_eq!(store.get(&id1).unwrap().thumbmark, fp.thumbmark);

    let _ = std::fs::remove_file(&path);
}

/// 验证同一 Key 的指纹状态跨 AppState 实例稳定：首个实例生成并写盘，第二个实例
/// （模拟重启，内存已清空）从磁盘恢复出完全相同的指纹。
#[test]
fn fingerprint_survives_restart() {
    use super::cc_client;
    let path = std::env::temp_dir().join(format!("cc-fp-restart-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let key = "user_persist_test";

    let state1 = AppState::new(Config::default());
    state1.set_fingerprint_path(path.clone());
    let fp1 = cc_client::key_fingerprint_for_test(&state1, key);
    assert!(path.exists(), "首次生成后应写盘");

    // 新实例：内存为空，应从磁盘恢复同一份指纹
    let state2 = AppState::new(Config::default());
    state2.set_fingerprint_path(path.clone());
    let fp2 = cc_client::key_fingerprint_for_test(&state2, key);
    assert_eq!(fp1.thumbmark, fp2.thumbmark);
    assert_eq!(fp1.components.machine_id_hash, fp2.components.machine_id_hash);

    let _ = std::fs::remove_file(&path);
}

// ── 集成测试：mock 上游 ───────────────────────────────

/// mock CC 上游路由：/alpha/generate 按模型名返回正常 NDJSON、零输出 NDJSON
/// 或 429 错误；model 为 `capture` 时把请求体与 zdr 头记录进 `captured` 供断言。
fn mock_upstream(captured: Option<Arc<Mutex<Value>>>) -> Router {
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
        if parsed["params"]["model"] == "slow" {
            // 慢响应：sleep 300ms 模拟上游耗时，用于并发上限测试占用在途额度
            tokio::time::sleep(Duration::from_millis(300)).await;
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"text-start\"}\n{\"type\":\"text-delta\",\"text\":\"Hello\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":10,\"outputTokens\":5,\"inputTokenDetails\":{\"cacheReadTokens\":3}}}\n",
            ));
        }
        if parsed["params"]["model"] == "no-usage" {
            // 有正文但 finish 不回报 usage：验证非流式零输出按实际内容判定而非 usage
            return axum::response::Response::new(axum::body::Body::from(
                "{\"type\":\"start\"}\n{\"type\":\"text-start\"}\n{\"type\":\"text-delta\",\"text\":\"正文内容\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\"}\n",
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
                        r#"{"data":[{"id":"mock-model-1","object":"model"}]}"#,
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

    // 注入内存设置库：生成本地转发 Key（sk-）并配置一个 CC 账户，使鉴权路径可用
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
    // 无 system 时 params.system 为空格占位
    assert_eq!(cap["body"]["params"]["system"], " ");
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

    let res = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "OK");

    // 根路径同样映射到健康检查
    let res = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "OK");

    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({ "messages": [] }))
        .send()
        .await
        .unwrap();
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
    // 注入内存统计库（连同设置表：本地 Key + CC 账户，保证鉴权路径可用）
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
