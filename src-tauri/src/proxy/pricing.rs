//! 模型单价表与成本估算（对齐 9router 的 pricing.js，单位均为 $/1M tokens）。
//!
//! 成本为估算值而非真实账单：未收录的模型回退到默认单价，缓存命中与
//! 缓存写入按子集从输入 token 中扣除，避免重复计费。

use std::collections::HashMap;

/// 单个模型的四档单价：输入 / 输出 / 缓存命中 / 缓存写入（$/1M tokens）。
#[derive(Debug, Clone, Copy)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cached: f64,
    pub cache_write: f64,
}

/// 未收录模型时的兜底单价。
const FALLBACK_PRICE: Price = Price {
    input: 1.00,
    output: 3.00,
    cached: 0.10,
    cache_write: 1.00,
};

/// 内置单价表：覆盖 deepseek / claude / gpt / qwen 等常见模型。
///
/// 价格参照各提供商公开价目（近似值），仅用于本地用量估算。
fn pricing_table() -> HashMap<&'static str, Price> {
    let mut m = HashMap::new();
    // DeepSeek
    m.insert("deepseek/deepseek-v4-flash", Price { input: 0.28, output: 0.42, cached: 0.028, cache_write: 0.28 });
    m.insert("deepseek/deepseek-v3", Price { input: 0.27, output: 1.10, cached: 0.07, cache_write: 0.27 });
    m.insert("deepseek/deepseek-reasoner", Price { input: 0.55, output: 2.19, cached: 0.14, cache_write: 0.55 });
    // Anthropic / Claude
    m.insert("claude-opus-4-6", Price { input: 5.00, output: 25.00, cached: 0.50, cache_write: 6.25 });
    m.insert("claude-sonnet-4-6", Price { input: 3.00, output: 15.00, cached: 0.30, cache_write: 3.75 });
    m.insert("claude-sonnet-4-5", Price { input: 3.00, output: 15.00, cached: 0.30, cache_write: 3.75 });
    m.insert("claude-sonnet-4", Price { input: 3.00, output: 15.00, cached: 0.30, cache_write: 3.00 });
    m.insert("claude-haiku-4-5", Price { input: 1.00, output: 5.00, cached: 0.10, cache_write: 1.25 });
    m.insert("claude-3-5-sonnet", Price { input: 3.00, output: 15.00, cached: 1.50, cache_write: 3.00 });
    m.insert("claude-3-7-sonnet", Price { input: 3.00, output: 15.00, cached: 1.50, cache_write: 3.75 });
    // OpenAI / GPT
    m.insert("gpt-4o", Price { input: 2.50, output: 10.00, cached: 1.25, cache_write: 2.50 });
    m.insert("gpt-4o-mini", Price { input: 0.15, output: 0.60, cached: 0.075, cache_write: 0.15 });
    m.insert("gpt-4.1", Price { input: 2.50, output: 10.00, cached: 1.25, cache_write: 2.50 });
    m.insert("gpt-5", Price { input: 1.25, output: 10.00, cached: 0.625, cache_write: 1.25 });
    m.insert("gpt-5-mini", Price { input: 0.25, output: 2.00, cached: 0.125, cache_write: 0.25 });
    m.insert("gpt-5-codex", Price { input: 1.25, output: 10.00, cached: 0.625, cache_write: 1.25 });
    m.insert("gpt-5.1-codex", Price { input: 1.75, output: 14.00, cached: 0.175, cache_write: 1.75 });
    m.insert("gpt-5.1-codex-mini", Price { input: 1.50, output: 6.00, cached: 0.75, cache_write: 1.50 });
    m.insert("gpt-5.1-codex-max", Price { input: 8.00, output: 32.00, cached: 4.00, cache_write: 8.00 });
    m.insert("gpt-5.3-codex", Price { input: 1.75, output: 14.00, cached: 0.175, cache_write: 1.75 });
    m
}

/// 查找模型的单价，未收录时回退默认价。
pub fn price_for(model: &str) -> Price {
    let table = pricing_table();
    // 优先精确匹配完整模型 ID，再尝试匹配 provider 前缀下的短名
    if let Some(p) = table.get(model) {
        return *p;
    }
    if let Some(short) = model.split('/').last() {
        if let Some(p) = table.get(short) {
            return *p;
        }
        // 短名前缀匹配，如 "claude-sonnet-4-6-xxxx" 命中 "claude-sonnet-4-6"。
        // 一个短名可能同时匹配多个 key（如 gpt-5 与 gpt-5.1-codex-mini），必须取
        // 「最长前缀」而非遍历 HashMap 的首个命中，否则结果随哈希种子随机、不可复现。
        let mut best: Option<(&str, Price)> = None;
        for (key, p) in &table {
            if short.starts_with(key) {
                let better = best.map(|(bk, _)| key.len() > bk.len()).unwrap_or(true);
                if better {
                    best = Some((key, *p));
                }
            }
        }
        if let Some((_, p)) = best {
            return p;
        }
    }
    FALLBACK_PRICE
}

/// 根据 token 用量与模型单价估算成本（美元）。
///
/// 输入 token 包含缓存命中与缓存写入两部分（均为输入的子集），计算时先扣除，
/// 避免按全价重复计费；缓存命中按缓存单价、缓存写入按写入单价分别计费。
pub fn calculate_cost(
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
) -> f64 {
    let p = price_for(model);
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
    fn cost_basic() {
        // deepseek-v4-flash: input 0.28, output 0.42
        let c = calculate_cost("deepseek/deepseek-v4-flash", 1_000_000, 0, 0, 0);
        assert!((c - 0.28).abs() < 1e-9);
        let c = calculate_cost("deepseek/deepseek-v4-flash", 0, 1_000_000, 0, 0);
        assert!((c - 0.42).abs() < 1e-9);
    }

    #[test]
    fn cost_cache_dedup() {
        // 输入 1M 全部命中缓存：只按缓存单价计费，不重复计输入
        let c = calculate_cost("deepseek/deepseek-v4-flash", 1_000_000, 0, 1_000_000, 0);
        assert!((c - 0.028).abs() < 1e-9);
        // 输入 1M 全部为缓存写入：按写入单价计费
        let c = calculate_cost("deepseek/deepseek-v4-flash", 1_000_000, 0, 0, 1_000_000);
        assert!((c - 0.28).abs() < 1e-9);
    }

    #[test]
    fn cost_fallback() {
        // 未收录模型走兜底价（input 1.00 / output 3.00）
        let c = calculate_cost("unknown/model-x", 1_000_000, 1_000_000, 0, 0);
        assert!((c - 4.00).abs() < 1e-9);
    }

    #[test]
    fn price_prefix_match() {
        // 短名前缀匹配：claude-sonnet-4-6-xxx 命中 claude-sonnet-4-6
        let p = price_for("claude-sonnet-4-6-20250929");
        assert!((p.input - 3.00).abs() < 1e-9);
    }

    #[test]
    fn price_longest_prefix_wins() {
        // gpt-5.1-codex-mini-xxx 同时是 gpt-5 与 gpt-5.1-codex-mini 的前缀，
        // 必须取最长者（mini 价 1.50），不能随 HashMap 迭代顺序随机命中 gpt-5
        for _ in 0..32 {
            let p = price_for("gpt-5.1-codex-mini-20250101");
            assert!((p.input - 1.50).abs() < 1e-9, "应命中 gpt-5.1-codex-mini");
        }
        let p = price_for("gpt-5.1-codex-max-preview");
        assert!((p.input - 8.00).abs() < 1e-9);
    }
}
