//! proxy 模块的单元与集成测试。
//!
//! 单元测试覆盖：三种协议的请求转换、SSE/流事件翻译、错误映射、Key 提取、
//! 设备指纹与项目 slug 生成；集成测试用 axum 搭建 mock CC 上游，端到端验证
//! 代理服务的流式/非流式转发、零输出限流、上游错误映射与鉴权行为。

use axum::http::HeaderMap;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

use super::cc_client;
use super::config::Config;
use super::convert;
use super::errors;
use super::fingerprint;
use super::server;
use super::sse::{AnthropicTranslator, OpenAiTranslator, ResponsesTranslator};
use super::state::AppState;

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
    let cc = convert::build_cc_request(&req);
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
    let cc = convert::build_cc_request(&req);
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
    let cc = convert::build_cc_request(&req);
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

/// 验证 Responses 非流式响应体结构：text 与 function_call 两类 output 条目、
/// usage 字段命名，以及 finish_reason=length 时 status=incomplete 并附
/// incomplete_details.reason=max_output_tokens。
#[test]
fn build_responses_response_shape() {
    let tool_calls = vec![json!({
        "id": "call_1", "type": "function",
        "function": { "name": "shell", "arguments": "{\"cmd\":\"ls\"}" },
    })];
    let body = convert::build_responses_response("resp_1", "gpt-5-codex", "Hi", Some(&tool_calls), "tool_calls", 10, 5, 3);
    assert_eq!(body["id"], "resp_1");
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["output"][0]["type"], "message");
    assert_eq!(body["output"][0]["content"][0]["text"], "Hi");
    assert_eq!(body["output"][1]["type"], "function_call");
    assert_eq!(body["output"][1]["call_id"], "call_1");
    assert_eq!(body["output"][1]["name"], "shell");
    assert_eq!(body["usage"]["output_tokens"], 5);
    assert_eq!(body["usage"]["input_tokens_details"]["cached_tokens"], 3);

    let truncated = convert::build_responses_response("resp_2", "m", "abc", None, "length", 1, 2, 0);
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
        r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":10,"outputTokens":5,"cachedInputTokens":3}}"#,
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
    t.parse_line(r#"{"type":"finish","totalUsage":{"inputTokens":100,"outputTokens":0,"cachedInputTokens":90}}"#);
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
        r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":7,"outputTokens":2,"cachedInputTokens":1}}"#,
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
        r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":10,"outputTokens":5,"cachedInputTokens":3}}"#,
    );
    assert_eq!(t.output_tokens, 5);
    let end = t.finalize();
    assert!(end.iter().any(|f| f.contains("response.completed")));
    assert!(end.iter().any(|f| f.contains("\"output_tokens\":5")));
}

/// 验证 Responses 翻译器异常路径：零输出时 finalize 发 response.failed（限流）；
/// 上游 error 事件即时转 response.failed 并置 has_error，此后 finalize 不再补发事件。
#[test]
fn responses_translator_zero_output_and_error() {
    let mut t = ResponsesTranslator::new("m", "resp_1");
    t.process_line(
        r#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":50,"outputTokens":0,"cachedInputTokens":40}}"#,
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

/// 验证 API Key 提取：无 Authorization 头返回 None；Bearer 值中匹配 user_ 前缀片段；
/// 非 user_ 前缀（如 sk-）的 Key 不被采信。
#[test]
fn api_key_extraction() {
    let mut headers = HeaderMap::new();
    assert!(server::extract_api_key(&headers).is_none());
    headers.insert(
        axum::http::header::AUTHORIZATION,
        "Bearer token_user_abc123_def".parse().unwrap(),
    );
    assert_eq!(server::extract_api_key(&headers).unwrap(), "user_abc123_def");
    headers.insert(
        axum::http::header::AUTHORIZATION,
        "Bearer sk-abc123".parse().unwrap(),
    );
    assert!(server::extract_api_key(&headers).is_none());
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

// ── 集成测试：mock 上游 ───────────────────────────────

/// mock CC 上游路由：/alpha/generate 按模型名返回正常 NDJSON、零输出 NDJSON
/// 或 429 错误；另提供指纹/生命周期/模型列表端点。
fn mock_upstream() -> Router {
    Router::new()
        .route(
            "/alpha/generate",
            post(|body: String| async move {
                let parsed: Value = serde_json::from_str(&body).unwrap();
                if parsed["params"]["model"] == "zero-output" {
                    return axum::response::Response::new(
                        axum::body::Body::from(
                            "{\"type\":\"start\"}\n{\"type\":\"text-start\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":50,\"outputTokens\":0,\"cachedInputTokens\":40}}\n",
                        ),
                    );
                }
                if parsed["params"]["model"] == "upstream-error" {
                    return axum::response::Response::builder()
                        .status(429)
                        .header("Content-Type", "application/json")
                        .body(axum::body::Body::from(r#"{"error":{"message":"rate limited"}}"#))
                        .unwrap();
                }
                axum::response::Response::new(axum::body::Body::from(
                    "{\"type\":\"start\"}\n{\"type\":\"text-start\"}\n{\"type\":\"text-delta\",\"text\":\"Hello\"}\n{\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":10,\"outputTokens\":5,\"cachedInputTokens\":3}}\n",
                ))
            }),
        )
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
}

/// 启动 mock 上游与真实代理服务（api_base 指向 mock，不启用后台任务），
/// 轮询等待服务就绪后返回（代理 base URL, 共享状态）。
async fn start_proxy() -> (String, Arc<AppState>) {
    let mock = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(mock, mock_upstream()).await;
    });

    let cfg = Config {
        api_base: format!("http://{mock_addr}"),
        port: 0, // 下面绑定真实端口
        auto_start_proxy: false,
        ..Config::default()
    };
    let state = AppState::new(cfg);
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

/// 端到端：Chat Completions 非流式请求返回 200，聚合文本为 Hello，
/// usage 含 completion_tokens 与缓存命中计数。
#[tokio::test]
async fn chat_completions_nonstream() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer user_test_key")
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
        .header("Authorization", "Bearer user_test_key")
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
        .header("Authorization", "Bearer user_test_key")
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
        .header("Authorization", "Bearer user_test_key")
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

/// 端到端：/v1/models 返回 Provider 动态模型列表、/health 返回 OK、
/// 无 API Key 的补全请求被拒绝为 401。
#[tokio::test]
async fn models_and_health_and_401() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{base}/v1/models"))
        .header("Authorization", "Bearer user_test_key")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert!(body["data"].as_array().unwrap().iter().any(|m| m["id"] == "mock-model-1"));

    let res = client.get(format!("{base}/health")).send().await.unwrap();
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

/// 端到端：Responses 非流式请求返回 response 对象（resp_ 前缀 id、completed 状态、
/// message 输出条目与 Responses 风格 usage）。
#[tokio::test]
async fn responses_nonstream() {
    let (base, state) = start_proxy().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/responses"))
        .header("Authorization", "Bearer user_test_key")
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
        .header("Authorization", "Bearer user_test_key")
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
        .header("Authorization", "Bearer user_test_key")
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
        .header("Authorization", "Bearer user_test_key")
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

/// 端到端：注入统计库后，一次非流式成功请求会被采集写入 SQLite，
/// 查询 stats 能聚合出请求数与 token 用量。
#[tokio::test]
async fn usage_recorded_through_proxy() {
    let (base, state) = start_proxy().await;
    // 注入内存统计库
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    super::usage::init_usage_on(&conn).unwrap();
    *state.usage.lock().unwrap() = Some(conn);

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer user_test_key")
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
    super::usage::init_usage_on(&conn).unwrap();
    *state.usage.lock().unwrap() = Some(conn);

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", "Bearer user_test_key")
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
