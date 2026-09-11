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

#[test]
fn openai_translator_zero_output_normalization() {
    let mut t = OpenAiTranslator::new("m", "c");
    t.parse_line(r#"{"type":"finish","totalUsage":{"inputTokens":100,"outputTokens":0,"cachedInputTokens":90}}"#);
    assert_eq!(t.input_tokens, 0);
    assert_eq!(t.cached_tokens, 0);
    assert!(t.zero_output_error_frame().contains("rate_limit_error"));
}

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

#[test]
fn fingerprint_shape() {
    let fp = fingerprint::generate();
    assert_eq!(fp.thumbmark.len(), 64);
    assert_eq!(fp.components.platform, "win32");
    assert_eq!(fp.components.collector_version, 1);
    let n = fp.components.mac_hashes.len();
    assert!((2..=5).contains(&n));
}

#[test]
fn project_slug_format() {
    let slug = cc_client::fake_project_slug("a3f2c001-0000-0000-0000-000000000000");
    assert!(!slug.starts_with("c:"));
    assert!(slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    assert!(slug.chars().all(|c| !c.is_uppercase()));
    assert!(!slug.starts_with('-') && !slug.ends_with('-'));
}

// ── 集成测试：mock 上游 ───────────────────────────────

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
