import { translate } from "@/i18n";

/** 格式化毫秒耗时：<1s 显示 ms，<1min 显示 s，否则显示 m s。 */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return `${m}m${s % 60}s`;
}

/** 格式化运行时长（秒）为当前语言的可读文本，如「3分20秒」/「2h 5m」。 */
export function formatUptime(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return translate("time.uptimeMinutes", { p0: m, p1: secs % 60 });
  const h = Math.floor(m / 60);
  return translate("time.uptimeHours", { p0: h, p1: m % 60 });
}

/** 将毫秒时间戳格式化为当日时钟 HH:MM:SS。 */
export function formatClock(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** 将毫秒时间戳格式化为日志时间 MM-DD HH:MM:SS。 */
export function formatLogTime(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** 复制文本到剪贴板：优先 Clipboard API，失败时回退到临时 textarea + execCommand。@returns 是否复制成功 */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    try {
      ta.select();
      // execCommand 失败时返回 false 而不抛异常，必须透传其返回值，
      // 否则会亮出「已复制」而剪贴板实际为空
      return document.execCommand("copy");
    } catch {
      return false;
    } finally {
      // 保证异常路径也不残留不可见的 textarea 节点
      ta.remove();
    }
  }
}

/** 将较大的整数格式化为紧凑文本（1.2K / 1.3M / 987），用于图表坐标与摘要卡。 */
export function formatCompactNumber(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) {
    // 先四舍五入再判断是否进位：否则 999_999 会得到 "1000.0K" 而非 "1.0M"
    const k = Number((n / 1000).toFixed(n % 1000 === 0 ? 0 : 1));
    return k >= 1000 ? `${(n / 1_000_000).toFixed(1)}M` : `${k}K`;
  }
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/** 将整数 token 数格式化为带千分位分隔符的文本（12,345）。 */
export function formatTokens(n: number): string {
  return n.toLocaleString("en-US");
}

/** 将美元成本格式化为 $ 前缀的紧凑文本（$0.1234 / $12.34 / $1.2K）。 */
export function formatCost(cost: number): string {
  if (cost === 0) return "$0";
  // 极小但非零的成本不能显示成 $0.0000（与真正的 $0 无法区分）
  if (cost < 0.0001) return "<$0.0001";
  if (cost < 0.01) return `$${cost.toFixed(4)}`;
  if (cost < 1000) return `$${cost.toFixed(2)}`;
  return `$${formatCompactNumber(cost)}`;
}

/** 将单价（$/1M tokens）格式化为紧凑文本：整数省略小数，小数最多保留 3 位。 */
export function formatPrice(v: number): string {
  if (v === 0) return "$0";
  if (Number.isInteger(v)) return `$${v}`;
  if (v < 0.01) return `$${v.toFixed(4)}`;
  return `$${Number(v.toFixed(3))}`;
}

/** 将 token 规模格式化为紧凑的 K/M 后缀文本（272K / 1M）。 */
export function formatContextTokens(n: number): string {
  if (n >= 1_000_000) return `${Number((n / 1_000_000).toFixed(1))}M`;
  if (n >= 1000) {
    // 与 formatCompactNumber 同理：先取整再判进位，避免 999_999 显示成 "1000K"
    const k = Number((n / 1000).toFixed(0));
    return k >= 1000 ? `${Number((n / 1_000_000).toFixed(1))}M` : `${k}K`;
  }
  return String(n);
}

/** 本地时区相对 UTC 的偏移分钟数（东八区为 +480）。 */
function localOffsetMinutes(): number {
  return -new Date().getTimezoneOffset();
}

/** 将窗口内的绝对分钟数取模到一天内，格式化为 HH:MM（整点省略分钟）。 */
function formatWindowMinutes(mins: number): string {
  const wrapped = ((mins % 1440) + 1440) % 1440;
  const pad = (n: number) => String(n).padStart(2, "0");
  const m = wrapped % 60;
  return m === 0 ? `${pad(Math.floor(wrapped / 60))}` : `${pad(Math.floor(wrapped / 60))}:${pad(m)}`;
}

/** 时区偏移标签：UTC / UTC+8 / UTC+5:30 / UTC-4。 */
function formatUtcOffset(offMin: number): string {
  if (offMin === 0) return "UTC";
  const abs = Math.abs(offMin);
  const m = abs % 60;
  return `UTC${offMin > 0 ? "+" : "-"}${Math.floor(abs / 60)}${m ? `:${String(m).padStart(2, "0")}` : ""}`;
}

/**
 * 将闲/忙时窗口（UTC 小时区间）换算为系统本地时区的可读描述，
 * 星期文本随界面语言本地化，并附上 UTC 偏移标签，
 * 如「周一至周五 09–12 & 14–18 UTC+8」。缺少结构化区间时回退到后端自带的英文描述。
 */
export function formatPeakWindows(tod: {
  peakRanges?: Array<[number, number]>;
  weekdaysOnly?: boolean;
  windows?: string;
}): string {
  const ranges = tod.peakRanges ?? [];
  if (ranges.length === 0) return tod.windows ?? "";
  const offMin = localOffsetMinutes();
  const segs: string[] = [];
  for (const [s, e] of ranges) {
    const start = s * 60 + offMin;
    const end = e * 60 + offMin;
    const startW = ((start % 1440) + 1440) % 1440;
    const endW = ((end % 1440) + 1440) % 1440;
    if (start !== end && startW >= endW) {
      // 窗口跨过本地午夜：拆成两段，保持 [start, end) 语义清晰。
      // end 恰落在本地 0 点时第二段为零长度，需省略，否则输出无意义的「00–00」。
      segs.push(`${formatWindowMinutes(start)}–24`);
      if (endW !== 0) segs.push(`00–${formatWindowMinutes(end)}`);
    } else {
      segs.push(`${formatWindowMinutes(start)}–${formatWindowMinutes(end)}`);
    }
  }
  // 星期范围是 UTC 语义（见 pricing.json 的 weekdaysOnly）。换算到本地时区后，
  // 西半球偏移（offMin < 0）会把整段窗口前移一天，例如 UTC Mon 01:00 在当地是
  // 周日 21:00，此时「周一至周五」与本地日历冲突，改用平移一天的说法。
  let weekdaysKey = "models.timeWeekdays";
  if (tod.weekdaysOnly && offMin < 0) weekdaysKey = "models.timeWeekdaysPrev";
  const weekdays = tod.weekdaysOnly ? `${translate(weekdaysKey)} ` : "";
  return `${weekdays}${segs.join(" & ")} ${formatUtcOffset(offMin)}`;
}
