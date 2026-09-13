#!/usr/bin/env node
/**
 * 从 commandcode.ai 官方 pricing-limits 文档爬取「按成本计价」（model-pricing-at-cost）
 * 的模型价格数据，格式化为价格域 JSON（促销 / 分档费率 / 闲忙时），写入
 * `src-tauri/src/proxy/pricing.json`（随程序打包，首次启动播种进 SQLite）。
 *
 * 用法：node tools/fetch-pricing.mjs
 * 数据更新：随时重跑本脚本即可热更新 pricing.json，重新编译（或走
 * models_catalog_update 运行时更新通道）后生效。
 *
 * 解析要点（Next.js RSC 载荷）：
 * - 页面数据在 `self.__next_f.push([1,"..."])` 的字符串流里；
 * - 价格行是 `{"rows":[...]}` 一个 JSON 岛；
 * - 重复对象会被序列化为 `"$<hex>:props:rows:..."` 引用串，需按路径回溯原值；
 * - `tiers[].context` 是人类可读文本（"≤ 32K"/"> 256K"），转成数值 maxContext
 *   （"> X" 为最高档、无上限 → null）；
 * - 闲忙时窗口只有文本（"01–04 & 06–10 UTC, Mon–Fri"），反解为
 *   peakRanges 数组 + weekdaysOnly 标记。
 */

import { writeFileSync, readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const PAGE_URL = "https://commandcode.ai/docs/resources/pricing-limits";
const OUT_FILE = join(dirname(fileURLToPath(import.meta.url)), "../src-tauri/src/proxy/pricing.json");

/** 把 RSC 文本流里的 `{"rows":...}` JSON 岛按括号配对完整截出。 */
function extractBalanced(s, start) {
  let depth = 0, inStr = false, esc = false;
  for (let i = start; i < s.length; i++) {
    const ch = s[i];
    if (inStr) {
      if (esc) esc = false;
      else if (ch === "\\") esc = true;
      else if (ch === '"') inStr = false;
    } else if (ch === '"') inStr = true;
    else if (ch === "{") depth++;
    else if (ch === "}" && --depth === 0) return s.slice(start, i + 1);
  }
  throw new Error("JSON 岛未闭合");
}

/** 解析 "≤ 32K"/"≤ 1M" 这类上下文文本为 token 数；"> X"（最高档无上限）或无法解析时返回 null。 */
function parseContextCap(text) {
  if (!text || /^\s*>/.test(text)) return null;
  const m = /([0-9.]+)\s*(K|M)/i.exec(text);
  if (!m) return null;
  const n = Number.parseFloat(m[1]);
  return Math.round(n * (m[2].toUpperCase() === "M" ? 1e6 : 1e3));
}

/** 从窗口文本（"01–04 & 06–10 UTC, Mon–Fri"）反解忙时区间与是否仅工作日。 */
function parseWindows(windows) {
  const ranges = [...windows.matchAll(/(\d+)\s*[–—-]\s*(\d+)/g)].map((m) => [
    Number(m[1]),
    Number(m[2]),
  ]);
  return { peakRanges: ranges, weekdaysOnly: /mon/i.test(windows) };
}

/** 四项费率补全：上游缺省的缓存写入价按 0 处理。 */
function normalizeRates(r) {
  return {
    input: r.input ?? 0,
    output: r.output ?? 0,
    cacheRead: r.cacheRead ?? 0,
    cacheWrite: r.cacheWrite ?? 0,
  };
}

const html = await (await fetch(PAGE_URL)).text();

// 1) 拼接 RSC 文本流：每段 push 的第二个元素是 JS 字符串字面量，用 JSON.parse 正确解码
const chunks = [...html.matchAll(/self\.__next_f\.push\(\[1,\s*"((?:[^"\\]|\\.)*)"\]\)/g)].map(
  (m) => JSON.parse(`"${m[1]}"`),
);
const stream = chunks.join("");

// 2) 截出价格行 JSON 岛
const anchor = stream.indexOf('{"rows":');
if (anchor < 0) throw new Error("页面中未找到价格数据（rows），页面结构可能已变化");
const rows = JSON.parse(extractBalanced(stream, anchor)).rows;

// 3) 解析 RSC 引用："$30:props:rows:17:tiers:0:listRates" → 引用组件 props 根下的
//    rows[17].tiers[0].listRates（路径含 "rows" 前缀，故从 props 根开始导航）
function resolve(value) {
  if (typeof value !== "string") return value;
  const m = /^\$[0-9a-f]+:props:(.+)$/.exec(value);
  if (!m) return value;
  let cur = { rows };
  for (const seg of m[1].split(":")) {
    cur = cur?.[Number.isNaN(Number(seg)) ? seg : Number(seg)];
  }
  return cur ?? value;
}

// 4) 格式化为价格域条目
const entries = rows.map((r) => ({
  id: r.id,
  ...(r.deal
    ? {
        deal: {
          discountPercent: r.deal.discountPercent ?? 0,
          free: r.deal.free ?? false,
          ...(r.deal.expires ? { expires: r.deal.expires } : {}),
          ...(r.deal.endsWhen ? { endsWhen: r.deal.endsWhen } : {}),
        },
      }
    : {}),
  ...(r.timeOfDay
    ? {
        timeOfDay: {
          peak: normalizeRates(r.timeOfDay.peak ?? {}),
          ...parseWindows(r.timeOfDay.windows ?? ""),
          ...(r.timeOfDay.windows ? { windows: r.timeOfDay.windows } : {}),
        },
      }
    : {}),
  tiers: (r.tiers ?? []).map((t) => {
    const listRates = t.listRates ? resolve(t.listRates) : undefined;
    return {
      maxContext: parseContextCap(t.context),
      rates: normalizeRates(t.rates ?? {}),
      ...(listRates && typeof listRates === "object" ? { listRates: normalizeRates(listRates) } : {}),
    };
  }),
}));
entries.sort((a, b) => a.id.localeCompare(b.id));

// 5) 写文件，并输出与现有文件的差异摘要
const before = existsSync(OUT_FILE) ? JSON.parse(readFileSync(OUT_FILE, "utf-8")) : [];
const beforeById = new Map(before.map((e) => [e.id, e]));
const changed = entries.filter((e) => JSON.stringify(e) !== JSON.stringify(beforeById.get(e.id))).length;
const added = entries.filter((e) => !beforeById.has(e.id)).map((e) => e.id);
const removed = before.filter((e) => !entries.some((n) => n.id === e.id)).map((e) => e.id);

writeFileSync(OUT_FILE, JSON.stringify(entries, null, 2) + "\n", "utf-8");

console.log(`已写入 ${OUT_FILE}`);
console.log(
  `共 ${entries.length} 条（促销 ${entries.filter((e) => e.deal).length}、闲忙时 ${entries.filter((e) => e.timeOfDay).length}、多档 ${entries.filter((e) => e.tiers.length > 1).length}）`,
);
if (before.length) {
  console.log(`与上次相比：变更 ${changed}、新增 [${added.join(", ") || "无"}]、移除 [${removed.join(", ") || "无"}]`);
}
