//! 模型单价表与成本估算，数据抓取自 commandcode.ai 的 pricing-limits 文档
//! （单位均为 $/1M tokens）。
//!
//! 计费口径要点：
//! - **分档计费**：按请求输入 token 落在哪一档上下文区间选取费率；
//! - **闲时/忙时**：DeepSeek 系列按 UTC 工作日窗口在 off-peak/peak 两套费率间切换，
//!   `timeOfDay` 中的 `tiers` 即闲时价，命中午夜窗口时改用 `peak`；
//! - **免费模型**：`deal.free` 为真或费率为 0 时不产生成本；
//! - **折扣**：`deal.discountPercent` 为促销标记，抓取到的 `rates` 已是折后有效价，
//!   成本直接按 `rates` 计算、不再二次打折，折扣仅作展示信息；
//! - **模型能力**：`caps` 记录 text/vision/reasoning，供模型列表展示。
//!
//! 成本为估算值而非真实账单；未收录的模型回退默认单价。
//!
//! 数据落库与兜底：运行时以 SQLite `model_pricing` 表为准（首次启动由内嵌表播种、
//! 数据更新按 ID 覆盖，见 models 模块）；内嵌常量 `MODELS_JSON` 仅作兜底，
//! 数据库无数据/不可用时才使用，由维护者不定时更新。

use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

/// 单档费率（输入 / 输出 / 缓存命中 / 缓存写入，$/1M tokens）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Rates {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default, rename = "cacheRead")]
    pub cached: f64,
    #[serde(default, rename = "cacheWrite")]
    pub cache_write: f64,
}

/// 模型能力标记（文本输入 / 视觉 / 思考）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ModelCaps {
    #[serde(default)]
    pub text: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub reasoning: bool,
}

/// 一个价格档位：`max_context` 为该档覆盖的最大输入 token（None 表示最高档、无上限）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tier {
    #[serde(default, rename = "maxContext")]
    pub max_context: Option<u64>,
    pub rates: Rates,
    /// 该档的标牌价（未打折；仅部分促销模型提供），仅供展示。
    #[serde(default, rename = "listRates", skip_serializing_if = "Option::is_none")]
    pub list_rates: Option<Rates>,
}

/// 闲时/忙时费率：`peak` 为忙时价，其余时间用档位价（即闲时价）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeOfDay {
    pub peak: Rates,
    /// 忙时窗口，UTC 小时区间 [start, end)。
    #[serde(default, rename = "peakRanges")]
    pub peak_ranges: Vec<(u8, u8)>,
    /// 忙时窗口是否仅限周一至周五。
    #[serde(default, rename = "weekdaysOnly")]
    pub weekdays_only: bool,
    /// 人类可读窗口描述（仅作展示）。
    #[serde(default)]
    pub windows: String,
}

/// 促销信息：`discount_percent` 为折扣百分比，`free` 表示限时免费。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deal {
    #[serde(default, rename = "discountPercent")]
    pub discount_percent: u8,
    #[serde(default)]
    pub free: bool,
    /// 促销结束日期（YYYY-MM-DD），存在时仅作展示。
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(default, rename = "endsWhen")]
    pub ends_when: Option<String>,
}

/// 单个模型的完整计费与能力信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default, rename = "contextWindow")]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub caps: ModelCaps,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub deal: Option<Deal>,
    #[serde(default, rename = "timeOfDay")]
    pub time_of_day: Option<TimeOfDay>,
    pub tiers: Vec<Tier>,
}

/// 未收录模型时的兜底单价。
pub const FALLBACK_PRICE: Rates = Rates {
    input: 1.00,
    output: 3.00,
    cached: 0.10,
    cache_write: 1.00,
};

/// 内嵌的模型计费表（抓取自官方 pricing-limits 文档，字段含义见上文类型注释）。
///
/// 仅作兜底：首次启动播种进 SQLite 后，运行时读取均以数据库为准。
const MODELS_JSON: &str = r#"[{"id":"laguna-s-2.1-free","name":"Laguna S 2.1","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}],"contextWindow":256000,"deal":{"discountPercent":100,"free":true,"endsWhen":"while capacity lasts"}},{"id":"ling-3.0-flash-free","name":"Ling 3.0 Flash","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}],"contextWindow":256000,"deprecated":true,"deal":{"discountPercent":100,"free":true,"expires":"2026-08-02"}},{"id":"longcat-2.0:free","name":"LongCat 2.0","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}],"contextWindow":1048576,"deal":{"discountPercent":100,"free":true,"endsWhen":"while it lasts"}},{"id":"ling-3.0-flash-sante:free","name":"Ling 3.0 Flash Sante","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}],"contextWindow":262144,"deal":{"discountPercent":100,"free":true,"endsWhen":"while it lasts"}},{"id":"tencent/hy4-preview","name":"Tencent Hy4 Preview","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.834,"output":2.501,"cacheRead":0.042,"cacheWrite":0}}],"contextWindow":1048576,"provider":"Tencent"},{"id":"tencent/hy3-paid","name":"Tencent Hy3","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.14,"output":0.58,"cacheRead":0.035,"cacheWrite":0}}],"contextWindow":262144,"provider":"Tencent"},{"id":"kimi-k3","name":"Kimi K3","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":3,"output":15,"cacheRead":0.3,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Moonshot AI"},{"id":"kimi-k2.7-code","name":"Kimi K2.7 Code","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.95,"output":4,"cacheRead":0.19,"cacheWrite":0}}],"contextWindow":256000,"provider":"Moonshot AI"},{"id":"kimi-k2.7-code-highspeed","name":"Kimi K2.7 Code HighSpeed","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.9,"output":8,"cacheRead":0.38,"cacheWrite":0}}],"contextWindow":262000,"provider":"Moonshot AI"},{"id":"kimi-k2.6","name":"Kimi K2.6","category":"opensource","caps":{"text":true,"vision":true,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":0.95,"output":4,"cacheRead":0.16,"cacheWrite":0}}],"contextWindow":256000,"provider":"Moonshot AI"},{"id":"kimi-k2.5","name":"Kimi K2.5","category":"opensource","caps":{"text":true,"vision":true,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":0.6,"output":3,"cacheRead":0.1,"cacheWrite":0}}],"contextWindow":256000,"provider":"Moonshot AI"},{"id":"glm-5.3-flash","name":"GLM-5.3 Flash","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.15,"output":0.5,"cacheRead":0.03,"cacheWrite":0}}],"contextWindow":1048576,"provider":"Z.ai"},{"id":"glm-5.3","name":"GLM-5.3","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.4,"output":4.4,"cacheRead":0.26,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Z.ai"},{"id":"glm-5.2","name":"GLM-5.2","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.4,"output":4.4,"cacheRead":0.26,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Z.ai"},{"id":"glm-5.2-fast","name":"GLM-5.2 Fast","category":"opensource","caps":{"text":true,"vision":false,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":3,"output":10.25,"cacheRead":0.5,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Z.ai"},{"id":"glm-5.1","name":"GLM-5.1","category":"opensource","caps":{"text":true,"vision":false,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":1.4,"output":4.4,"cacheRead":0.26,"cacheWrite":0}}],"provider":"Z.ai"},{"id":"glm-5","name":"GLM-5","category":"opensource","caps":{"text":true,"vision":false,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":1,"output":3.2,"cacheRead":0.2,"cacheWrite":0}}],"contextWindow":200000,"provider":"Z.ai"},{"id":"minimax-m3","name":"MiniMax M3","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":512000,"rates":{"input":0.3,"output":1.2,"cacheRead":0.06,"cacheWrite":0},"listRates":{"input":0.6,"output":2.4,"cacheRead":0.12,"cacheWrite":0}},{"maxContext":null,"rates":{"input":0.3,"output":1.2,"cacheRead":0.06,"cacheWrite":0}}],"contextWindow":1000000,"provider":"MiniMax","deal":{"discountPercent":50,"free":false}},{"id":"minimax-m2.7","name":"MiniMax M2.7","category":"opensource","caps":{"text":true,"vision":false,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":0.3,"output":1.2,"cacheRead":0.06,"cacheWrite":0}}],"provider":"MiniMax"},{"id":"minimax-m2.5","name":"MiniMax M2.5","category":"opensource","caps":{"text":true,"vision":false,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":0.3,"output":1.2,"cacheRead":0.03,"cacheWrite":0}}],"contextWindow":200000,"provider":"MiniMax"},{"id":"deepseek-v4-pro","name":"DeepSeek V4 Pro (latest)","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.66,"output":1.98,"cacheRead":0.022,"cacheWrite":0}}],"contextWindow":1000000,"provider":"DeepSeek","timeOfDay":{"peak":{"input":1.32,"output":3.96,"cacheRead":0.044,"cacheWrite":0},"peakRanges":[[1,4],[6,10]],"weekdaysOnly":true,"windows":"01–04 & 06–10 UTC, Mon–Fri"}},{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash (latest)","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.15,"output":0.6,"cacheRead":0.003,"cacheWrite":0}}],"contextWindow":1000000,"provider":"DeepSeek","timeOfDay":{"peak":{"input":0.3,"output":1.2,"cacheRead":0.006,"cacheWrite":0},"peakRanges":[[1,4],[6,10]],"weekdaysOnly":true,"windows":"01–04 & 06–10 UTC, Mon–Fri"}},{"id":"deepseek-v4-flash-vision-exp","name":"DeepSeek V4 Flash Vision (exp)","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.22,"output":0.66,"cacheRead":0.007,"cacheWrite":0}}],"contextWindow":1000000,"provider":"DeepSeek","timeOfDay":{"peak":{"input":0.44,"output":1.32,"cacheRead":0.014,"cacheWrite":0},"peakRanges":[[1,4],[6,10]],"weekdaysOnly":true,"windows":"01–04 & 06–10 UTC, Mon–Fri"}},{"id":"deepseek-v4-flash-fast","name":"DeepSeek V4 Flash Fast","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.28,"output":0.56,"cacheRead":0.07,"cacheWrite":0}}],"contextWindow":1000000,"provider":"DeepSeek"},{"id":"deepseek-v4.1-flash","name":"DeepSeek V4.1 Flash","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.15,"output":0.6,"cacheRead":0.003,"cacheWrite":0}}],"contextWindow":1000000,"provider":"DeepSeek","timeOfDay":{"peak":{"input":0.3,"output":1.2,"cacheRead":0.006,"cacheWrite":0},"peakRanges":[[1,4],[6,10]],"weekdaysOnly":true,"windows":"01–04 & 06–10 UTC, Mon–Fri"}},{"id":"qwen-3.8-max-0902","name":"Qwen 3.8 Max 0902","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":2,"output":6,"cacheRead":0.25,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Alibaba"},{"id":"qwen-3.8-max","name":"Qwen 3.8 Max","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":2,"output":6,"cacheRead":0.25,"cacheWrite":2.5}}],"contextWindow":1000000,"provider":"Alibaba"},{"id":"qwen-3.8-27b","name":"Qwen 3.8 27B","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.4,"output":3,"cacheRead":0.04,"cacheWrite":0}}],"contextWindow":262144,"provider":"Alibaba"},{"id":"qwen-3.6-max","name":"Qwen 3.6 Max Preview","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.3,"output":7.8,"cacheRead":0.26,"cacheWrite":1.63}}],"provider":"Alibaba"},{"id":"qwen-3.6-plus","name":"Qwen 3.6 Plus","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":256000,"rates":{"input":0.5,"output":3,"cacheRead":0.1,"cacheWrite":0}},{"maxContext":null,"rates":{"input":2,"output":6,"cacheRead":0.2,"cacheWrite":0}}],"provider":"Alibaba"},{"id":"qwen-3.7-max","name":"Qwen 3.7 Max","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":2.5,"output":7.5,"cacheRead":0.5,"cacheWrite":3.13},"listRates":{"input":5,"output":15,"cacheRead":1,"cacheWrite":6.26}}],"contextWindow":1000000,"provider":"Alibaba","deal":{"discountPercent":50,"free":false,"expires":"2026-06-22"}},{"id":"qwen-3.7-plus","name":"Qwen 3.7 Plus","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":256000,"rates":{"input":0.4,"output":1.6,"cacheRead":0.08,"cacheWrite":0.5}},{"maxContext":null,"rates":{"input":1.2,"output":4.8,"cacheRead":0.24,"cacheWrite":1.5}}],"contextWindow":1000000,"provider":"Alibaba"},{"id":"qwen-3.8-flash","name":"Qwen 3.8 Flash","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.16,"output":0.47,"cacheRead":0.016,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Alibaba"},{"id":"qwen-3.7-flash","name":"Qwen 3.7 Flash","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":32000,"rates":{"input":0.03,"output":0.13,"cacheRead":0.006,"cacheWrite":0.038}},{"maxContext":256000,"rates":{"input":0.1,"output":0.4,"cacheRead":0.02,"cacheWrite":0.125}},{"maxContext":null,"rates":{"input":0.2,"output":0.8,"cacheRead":0.04,"cacheWrite":0.25}}],"contextWindow":1000000,"provider":"Alibaba"},{"id":"step-3.7-flash","name":"Step 3.7 Flash","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.2,"output":1.15,"cacheRead":0.04,"cacheWrite":0}}],"contextWindow":256000,"provider":"StepFun"},{"id":"step-3.5-flash","name":"Step 3.5 Flash","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.1,"output":0.3,"cacheRead":0.02,"cacheWrite":0}}],"contextWindow":1000000,"provider":"StepFun"},{"id":"mimo-v2.5-pro","name":"MiMo V2.5 Pro","category":"opensource","caps":{"text":true,"vision":false,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":0.435,"output":0.87,"cacheRead":0.0036,"cacheWrite":0},"listRates":{"input":2,"output":6,"cacheRead":0.4,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Xiaomi","deal":{"discountPercent":99,"free":false}},{"id":"mimo-v2.5","name":"MiMo V2.5","category":"opensource","caps":{"text":true,"vision":true,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":0.14,"output":0.28,"cacheRead":0.0028,"cacheWrite":0},"listRates":{"input":0.8,"output":4,"cacheRead":0.16,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Xiaomi","deal":{"discountPercent":98,"free":false}},{"id":"nemotron-3-ultra","name":"Nemotron 3 Ultra","category":"opensource","caps":{"text":true,"vision":false,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.6,"output":2.4,"cacheRead":0.12,"cacheWrite":0}}],"contextWindow":1000000,"provider":"NVIDIA"},{"id":"claude-fable-5-1","name":"Claude Fable 5.1","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":10,"output":50,"cacheRead":0.25,"cacheWrite":12.5}}],"contextWindow":1000000,"provider":"Anthropic"},{"id":"claude-fable-5","name":"Claude Fable 5","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":10,"output":50,"cacheRead":1,"cacheWrite":12.5}}],"contextWindow":1000000,"provider":"Anthropic"},{"id":"claude-opus-5","name":"Claude Opus 5","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":5,"output":25,"cacheRead":0.5,"cacheWrite":6.25}}],"contextWindow":1000000,"provider":"Anthropic"},{"id":"claude-opus-4-8","name":"Claude Opus 4.8","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":5,"output":25,"cacheRead":0.5,"cacheWrite":6.25}}],"contextWindow":1000000,"provider":"Anthropic"},{"id":"claude-opus-4-7","name":"Claude Opus 4.7","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":5,"output":25,"cacheRead":0.5,"cacheWrite":6.25}}],"contextWindow":1000000,"provider":"Anthropic"},{"id":"claude-opus-4-6","name":"Claude Opus 4.6","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":5,"output":25,"cacheRead":0.5,"cacheWrite":6.25}}],"contextWindow":1000000,"provider":"Anthropic"},{"id":"claude-sonnet-5","name":"Claude Sonnet 5","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":2,"output":10,"cacheRead":0.2,"cacheWrite":2.5}}],"contextWindow":1000000,"provider":"Anthropic"},{"id":"claude-sonnet-4-6","name":"Claude Sonnet 4.6","category":"premium","caps":{"text":true,"vision":true,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":3,"output":15,"cacheRead":0.3,"cacheWrite":3.75}}],"contextWindow":1000000,"provider":"Anthropic"},{"id":"claude-haiku-4-5","name":"Claude Haiku 4.5","category":"premium","caps":{"text":true,"vision":true,"reasoning":false},"tiers":[{"maxContext":null,"rates":{"input":1,"output":5,"cacheRead":0.1,"cacheWrite":1.25}}],"contextWindow":200000,"provider":"Anthropic"},{"id":"gpt-6-astra","name":"GPT-6 Astra","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":272000,"rates":{"input":10,"output":50,"cacheRead":1,"cacheWrite":12.5}},{"maxContext":null,"rates":{"input":20,"output":75,"cacheRead":2,"cacheWrite":25}}],"contextWindow":1050000,"provider":"OpenAI"},{"id":"gpt-5.6-sol","name":"GPT-5.6 Sol","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":272000,"rates":{"input":5,"output":30,"cacheRead":0.5,"cacheWrite":6.25}},{"maxContext":null,"rates":{"input":10,"output":45,"cacheRead":1,"cacheWrite":12.5}}],"contextWindow":1050000,"provider":"OpenAI"},{"id":"gpt-5.6-terra","name":"GPT-5.6 Terra","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":272000,"rates":{"input":2,"output":12,"cacheRead":0.2,"cacheWrite":2.5}},{"maxContext":null,"rates":{"input":4,"output":18,"cacheRead":0.4,"cacheWrite":5}}],"contextWindow":1050000,"provider":"OpenAI"},{"id":"gpt-5.6-luna","name":"GPT-5.6 Luna","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":272000,"rates":{"input":0.2,"output":1.2,"cacheRead":0.02,"cacheWrite":0.25}},{"maxContext":null,"rates":{"input":0.4,"output":1.8,"cacheRead":0.04,"cacheWrite":0.5}}],"contextWindow":1050000,"provider":"OpenAI"},{"id":"gpt-5.5","name":"GPT-5.5","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":5,"output":30,"cacheRead":0.5,"cacheWrite":0}}],"contextWindow":400000,"provider":"OpenAI"},{"id":"gpt-5.4","name":"GPT-5.4","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":2.5,"output":15,"cacheRead":0.25,"cacheWrite":0}}],"contextWindow":400000,"provider":"OpenAI"},{"id":"gpt-5.4-mini","name":"GPT-5.4 Mini","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.75,"output":4.5,"cacheRead":0.075,"cacheWrite":0}}],"contextWindow":400000,"provider":"OpenAI"},{"id":"gpt-5.3-codex","name":"GPT-5.3 Codex","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":2,"output":8,"cacheRead":0.5,"cacheWrite":0}}],"contextWindow":400000,"provider":"OpenAI"},{"id":"gemini-3.8-flash","name":"Gemini 3.8 Flash","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.5,"output":7.5,"cacheRead":0.15,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Google"},{"id":"gemini-3.7-flash","name":"Gemini 3.7 Flash","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.5,"output":7.5,"cacheRead":0.15,"cacheWrite":0.08334}}],"contextWindow":1048576,"provider":"Google"},{"id":"gemini-3.6-flash","name":"Gemini 3.6 Flash","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.5,"output":7.5,"cacheRead":0.15,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Google"},{"id":"gemini-3.5-flash","name":"Gemini 3.5 Flash","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.5,"output":9,"cacheRead":0.15,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Google"},{"id":"gemini-3.5-flash-lite","name":"Gemini 3.5 Flash Lite","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.3,"output":2.5,"cacheRead":0.03,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Google"},{"id":"gemini-3.1-flash-lite","name":"Gemini 3.1 Flash Lite","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.25,"output":1.5,"cacheRead":0.03,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Google"},{"id":"fugu-ultra","name":"Fugu Ultra","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":5,"output":30,"cacheRead":0.5,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Sakana"},{"id":"muse-spark-1.3","name":"Muse Spark 1.3","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.25,"output":4.25,"cacheRead":0.15,"cacheWrite":0}}],"contextWindow":1048576,"provider":"Meta"},{"id":"muse-spark-1.3-contributor","name":"Muse Spark 1.3 Contributor","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.1,"output":0.2,"cacheRead":0.002,"cacheWrite":0}}],"contextWindow":1048576,"provider":"Meta"},{"id":"muse-spark-1.2","name":"Muse Spark 1.2","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.25,"output":4.25,"cacheRead":0.15,"cacheWrite":0}}],"contextWindow":1048576,"provider":"Meta"},{"id":"muse-spark-1.2-contributor","name":"Muse Spark 1.2 Contributor","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.1,"output":0.2,"cacheRead":0.002,"cacheWrite":0}}],"contextWindow":1048576,"provider":"Meta"},{"id":"muse-spark-1.1","name":"Muse Spark 1.1","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1.25,"output":4.25,"cacheRead":0.15,"cacheWrite":0}}],"contextWindow":1048576,"provider":"Meta"},{"id":"grok-4.6","name":"Grok 4.6","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":200000,"rates":{"input":2,"output":6,"cacheRead":0.5,"cacheWrite":0}},{"maxContext":null,"rates":{"input":4,"output":12,"cacheRead":1,"cacheWrite":0}}],"contextWindow":500000,"provider":"xAI"},{"id":"grok-4.5","name":"Grok 4.5","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":2,"output":6,"cacheRead":0.5,"cacheWrite":0}}],"contextWindow":500000,"provider":"xAI"},{"id":"inkling","name":"Inkling","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":1,"output":4.05,"cacheRead":0.17,"cacheWrite":0}}],"contextWindow":256000,"provider":"Thinking Machines"},{"id":"inkling-small","name":"Inkling Small","category":"opensource","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":0.5,"output":1.2,"cacheRead":0.1,"cacheWrite":0}}],"contextWindow":1000000,"provider":"Thinking Machines"},{"id":"claude-sonnet-4-5","name":"Claude Sonnet 4.5","category":"premium","caps":{"text":true,"vision":true,"reasoning":true},"tiers":[{"maxContext":null,"rates":{"input":3,"output":15,"cacheRead":0.3,"cacheWrite":3.75}}],"contextWindow":1000000,"deprecated":true}]"#;

/// 运行时模型表：启动时由 SQLite 载入，数据更新时整体替换；`None` 表示尚未载入。
static REGISTRY: RwLock<Option<Arc<Vec<ModelPricing>>>> = RwLock::new(None);

/// 解析内嵌兜底表（进程内只解析一次）。
pub fn builtin_models() -> &'static [ModelPricing] {
    static CACHE: OnceLock<Vec<ModelPricing>> = OnceLock::new();
    CACHE.get_or_init(|| {
        serde_json::from_str::<Vec<ModelPricing>>(MODELS_JSON)
            .expect("内置计费表 JSON 解析失败")
    })
}

/// 用给定模型表整体替换运行时注册表（启动时从 SQLite 载入、数据更新落库后刷新）。
pub fn set_models(models: Vec<ModelPricing>) {
    *REGISTRY.write().unwrap() = Some(Arc::new(models));
}

/// 当前生效的模型表：优先注册表（SQLite 载入），未载入时回退内嵌兜底表。
///
/// 返回 `Arc` 便于调用方低成本持有快照，避免在统计重算等高频路径上反复克隆整表。
pub fn all_models() -> Arc<Vec<ModelPricing>> {
    if let Some(models) = REGISTRY.read().unwrap().as_ref() {
        return models.clone();
    }
    static FALLBACK: OnceLock<Arc<Vec<ModelPricing>>> = OnceLock::new();
    FALLBACK
        .get_or_init(|| Arc::new(builtin_models().to_vec()))
        .clone()
}

/// 查找模型计费信息：精确 → 去 provider 前缀的短名 → 最长前缀。
///
/// 上游模型 ID 可能带 provider 前缀（`deepseek/deepseek-v4-flash`）或日期后缀
/// （`claude-haiku-4-5-20251001`），故匹配不区分大小写并取最长前缀。
/// 返回借用，调用方持有 `all_models()` 快照即可复用，避免整条克隆。
pub fn find_pricing<'a>(models: &'a [ModelPricing], model: &str) -> Option<&'a ModelPricing> {
    let target = model.to_ascii_lowercase();

    // 1) 全名精确匹配（忽略大小写）
    if let Some(m) = models.iter().find(|m| m.id.to_ascii_lowercase() == target) {
        return Some(m);
    }
    // 2) 去掉 provider 前缀后的短名精确匹配
    let short = target.rsplit('/').next().unwrap_or(&target);
    if let Some(m) = models.iter().find(|m| m.id.to_ascii_lowercase() == short) {
        return Some(m);
    }
    // 3) 最长前缀匹配，避免短前缀（如 gpt-5）抢走更具体的档位
    models
        .iter()
        .filter(|m| short.starts_with(&m.id.to_ascii_lowercase()))
        .max_by_key(|m| m.id.len())
}

/// 判断给定时刻是否落在忙时窗口内（UTC，窗口定义来自模型自身的 `timeOfDay`）。
///
/// 所有闲忙时模型共用同一窗口（UTC 周一至周五 01–04 与 06–10），故此处用统一规则；
/// 若某模型 `weekdaysOnly` 为假则忽略星期限制。
pub fn is_peak_at(ts_millis: u64, tod: &TimeOfDay) -> bool {
    let secs = (ts_millis / 1000) as i64;
    let Some(dt) = chrono::DateTime::from_timestamp(secs, 0) else {
        return false;
    };
    let dt = dt.naive_utc();
    if tod.weekdays_only {
        use chrono::Datelike;
        // Mon=0 .. Sun=6
        if dt.weekday().num_days_from_monday() >= 5 {
            return false;
        }
    }
    use chrono::Timelike;
    let hour = dt.hour() as u8;
    tod.peak_ranges.iter().any(|(s, e)| hour >= *s && hour < *e)
}

/// 解析指定模型在给定输入规模与时刻下的有效费率。
///
/// 先按输入 token 选档，命中午夜窗口时用忙时价覆盖输入/输出/缓存命中
/// （缓存写入忙时未单独给出，沿用档位价）。
pub fn price_for_at(model: &str, prompt_tokens: u64, ts_millis: u64) -> Rates {
    let models = all_models();
    let Some(m) = find_pricing(&models, model) else {
        return FALLBACK_PRICE;
    };
    let tier_rates = select_tier(m, prompt_tokens);
    match &m.time_of_day {
        Some(tod) if is_peak_at(ts_millis, tod) => Rates {
            cache_write: tier_rates.cache_write,
            ..tod.peak
        },
        _ => tier_rates,
    }
}

/// 选取输入 token 对应的价格档位费率（档位按 max_context 升序，最后一个通常无上限）。
fn select_tier(m: &ModelPricing, prompt_tokens: u64) -> Rates {
    for t in &m.tiers {
        if t.max_context.map(|c| prompt_tokens <= c).unwrap_or(true) {
            return t.rates;
        }
    }
    m.tiers.last().map(|t| t.rates).unwrap_or(FALLBACK_PRICE)
}

/// 把当前生效的模型表序列化为 JSON，供前端展示模型能力/折扣/免费等信息。
pub fn catalog_json() -> serde_json::Value {
    serde_json::to_value(&*all_models()).unwrap_or_else(|_| serde_json::json!([]))
}

/// 根据 token 用量、模型档位与请求时刻估算成本（美元）。
///
/// 输入 token 包含缓存命中与缓存写入两部分（均为输入的子集），计算时先扣除，
/// 避免按全价重复计费；缓存命中按缓存单价、缓存写入按写入单价分别计费。
pub fn calculate_cost(
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
    ts_millis: u64,
) -> f64 {
    let p = price_for_at(model, prompt_tokens, ts_millis);
    let cached = cached_tokens.min(prompt_tokens) as f64;
    let cache_write = cache_write_tokens.min(prompt_tokens) as f64;
    let non_cached = (prompt_tokens as f64 - cached - cache_write).max(0.0);
    let mut cost = non_cached * p.input / 1e6;
    if cached > 0.0 {
        cost += cached * p.cached / 1e6;
    }
    if cache_write > 0.0 {
        cost += cache_write * p.cache_write / 1e6;
    }
    cost += completion_tokens as f64 * p.output / 1e6;
    cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_loaded_and_covers_common_models() {
        let models = all_models();
        assert!(models.len() >= 60, "计费表条目过少: {}", models.len());
        // 主推模型都应能查到
        for id in [
            "deepseek/deepseek-v4-flash",
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "gpt-5.5",
            "gpt-5.4",
            "kimi-k3",
        ] {
            let p = find_pricing(&models, id);
            assert!(p.is_some(), "未收录模型 {id}");
        }
    }

    #[test]
    fn provider_prefix_and_case_insensitive() {
        // provider 前缀与大小写不影响匹配
        let models = all_models();
        let a = find_pricing(&models, "deepseek/deepseek-v4-flash").unwrap();
        let b = find_pricing(&models, "deepseek-v4-flash").unwrap();
        assert_eq!(a.id, b.id);
        let c = find_pricing(&models, "moonshotai/Kimi-K2.6").unwrap();
        assert_eq!(c.id, "kimi-k2.6");
    }

    #[test]
    fn longest_prefix_wins() {
        // gpt-5.6-luna 不应被错误前缀抢走；带日期后缀也能命中
        let p = price_for_at("gpt-5.6-luna-20260101", 1000, 0);
        assert!((p.input - 0.2).abs() < 1e-9);
        let p2 = price_for_at("claude-haiku-4-5-20251001", 1000, 0);
        assert!((p2.input - 1.0).abs() < 1e-9);
    }

    #[test]
    fn tier_selection_by_context() {
        // qwen-3.7-flash：≤32K 为 0.03，≤256K 为 0.1，>256K 为 0.2
        assert!((price_for_at("qwen-3.7-flash", 10_000, 0).input - 0.03).abs() < 1e-9);
        assert!((price_for_at("qwen-3.7-flash", 100_000, 0).input - 0.1).abs() < 1e-9);
        assert!((price_for_at("qwen-3.7-flash", 500_000, 0).input - 0.2).abs() < 1e-9);
        // grok-4.6：≤200K 与 >200K 两档
        assert!((price_for_at("grok-4.6", 100_000, 0).input - 2.0).abs() < 1e-9);
        assert!((price_for_at("grok-4.6", 300_000, 0).input - 4.0).abs() < 1e-9);
    }

    #[test]
    fn off_peak_and_peak_pricing() {
        let models = all_models();
        let m = find_pricing(&models, "deepseek-v4-pro").unwrap();
        let tod = m.time_of_day.as_ref().unwrap();
        // 2026-09-14 是周一：02:00 UTC 为忙时，12:00 UTC 为闲时
        let mon_peak = chrono::DateTime::parse_from_rfc3339("2026-09-14T02:00:00Z")
            .unwrap()
            .timestamp_millis() as u64;
        let mon_off = chrono::DateTime::parse_from_rfc3339("2026-09-14T12:00:00Z")
            .unwrap()
            .timestamp_millis() as u64;
        let sat = chrono::DateTime::parse_from_rfc3339("2026-09-19T02:00:00Z")
            .unwrap()
            .timestamp_millis() as u64;
        assert!(is_peak_at(mon_peak, tod));
        assert!(!is_peak_at(mon_off, tod));
        // 周六同一时刻不算忙时
        assert!(!is_peak_at(sat, tod));
        assert!((price_for_at("deepseek-v4-pro", 1000, mon_peak).input - 1.32).abs() < 1e-9);
        assert!((price_for_at("deepseek-v4-pro", 1000, mon_off).input - 0.66).abs() < 1e-9);
    }

    #[test]
    fn free_models_cost_zero() {
        // 免费模型（deal.free 且费率为 0）计费应为 0
        for id in ["laguna-s-2.1-free", "longcat-2.0:free"] {
            let c = calculate_cost(id, 1_000_000, 1_000_000, 0, 0, 0);
            assert_eq!(c, 0.0, "{id} 应为免费");
            let models = all_models();
            assert!(find_pricing(&models, id).unwrap().deal.as_ref().unwrap().free);
        }
    }

    #[test]
    fn caps_exposed() {
        // 能力标记：kimi-k3 具备视觉；minimax-m2.5 不支持思考
        let models = all_models();
        let k = find_pricing(&models, "kimi-k3").unwrap();
        assert!(k.caps.text && k.caps.vision && k.caps.reasoning);
        let m = find_pricing(&models, "minimax-m2.5").unwrap();
        assert!(!m.caps.reasoning);
    }

    #[test]
    fn cost_basic_and_cache_dedup() {
        // 取无闲忙时模型的固定档位验证成本公式
        let c = calculate_cost("claude-opus-4-8", 1_000_000, 0, 0, 0, 0);
        assert!((c - 5.0).abs() < 1e-9);
        let c = calculate_cost("claude-opus-4-8", 0, 1_000_000, 0, 0, 0);
        assert!((c - 25.0).abs() < 1e-9);
        // 输入全命中缓存：按缓存单价计费，不重复计输入
        let c = calculate_cost("claude-opus-4-8", 1_000_000, 0, 1_000_000, 0, 0);
        assert!((c - 0.5).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_falls_back() {
        let c = calculate_cost("unknown/model-x", 1_000_000, 1_000_000, 0, 0, 0);
        assert!((c - 4.0).abs() < 1e-9);
    }
}
