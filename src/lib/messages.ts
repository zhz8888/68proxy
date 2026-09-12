// 后端消息的翻译层。
//
// 后端不直接返回中文/英文文案，而是返回带前缀的消息码（见 src-tauri/src/i18n.rs）：
// - `err:<code>` / `err:<code>:[参数…]`：错误（展示为 toast.error 或错误条）
// - `msg:<code>` / `msg:<code>:[参数…]`：成功类提示（展示为 toast.success）
// 参数为 JSON 字符串数组，在此映射为插值变量 p0/p1… 后交给 i18next。
//
// 无前缀的字符串（上游英文报错、OS 错误等）原样返回，保证外部文案不受影响。

import { translate } from "@/i18n";

/** 错误消息前缀（与后端 i18n::err 一致）。 */
const ERR_PREFIX = "err:";
/** 成功提示前缀（与后端 i18n::msg 一致）。 */
const MSG_PREFIX = "msg:";

/** 去掉 `Error: ` 前缀：开发调试桥接会把后端错误包进 Error 对象。 */
function unwrap(raw: string): string {
  return raw.startsWith("Error: ") ? raw.slice(7) : raw;
}

/** 把带前缀的后端消息码翻译为当前语言文案；非消息码原样返回。 */
export function msgText(raw: string): string {
  const text = unwrap(raw);
  const isErr = text.startsWith(ERR_PREFIX);
  const isMsg = text.startsWith(MSG_PREFIX);
  if (!isErr && !isMsg) return text;
  const body = text.slice((isErr ? ERR_PREFIX : MSG_PREFIX).length);
  const sep = body.indexOf(":");
  const code = sep === -1 ? body : body.slice(0, sep);
  const opts: Record<string, unknown> = {};
  if (sep !== -1) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(body.slice(sep + 1));
    } catch {
      // 参数区不是合法 JSON：不是约定的消息码，原样返回避免误译
      return text;
    }
    if (Array.isArray(parsed)) parsed.forEach((v, i) => (opts[`p${i}`] = String(v)));
  }
  const key = `${isErr ? "errors" : "messages"}.${code}`;
  const out = translate(key, opts);
  // 未登记的码：回退展示原始码，便于排查而不是显示空串
  return out === key ? code : out;
}

/** 把任意错误值翻译为当前语言文案（供 toast.error / 错误条统一调用）。 */
export function errText(e: unknown): string {
  return msgText(String(e));
}
