//! 本地代理服务核心模块。
//!
//! 将下游客户端的 OpenAI Chat Completions / OpenAI Responses / Anthropic Messages
//! 三种协议请求，转换为 command-code 上游的 CLI 信封协议并转发，
//! 再把上游返回的 NDJSON 事件流翻译回对应协议的 SSE 或 JSON 响应。

pub mod auth_login;
pub mod cc_client;
pub mod config;
pub mod convert;
pub mod errors;
pub mod fingerprint;
pub mod log;
pub mod models;
pub mod plans;
pub mod pricing;
pub mod quota;
pub mod server;
pub mod settings;
pub mod sse;
pub mod state;
pub mod usage;

#[cfg(test)]
mod tests;
