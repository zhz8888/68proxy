/** 格式化毫秒耗时：<1s 显示 ms，<1min 显示 s，否则显示 m s。 */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return `${m}m${s % 60}s`;
}

/** 格式化运行时长（秒）为中文可读文本，如「3分20秒」「2小时5分」。 */
export function formatUptime(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}分${secs % 60}秒`;
  const h = Math.floor(m / 60);
  return `${h}小时${m % 60}分`;
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
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
      return true;
    } catch {
      return false;
    }
  }
}

/** 将较大的整数格式化为紧凑文本（1.2K / 1.3M / 987），用于图表坐标与摘要卡。 */
export function formatCompactNumber(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n % 1000 === 0 ? 0 : 1)}K`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/** 将整数 token 数格式化为带千分位分隔符的文本（12,345）。 */
export function formatTokens(n: number): string {
  return n.toLocaleString("en-US");
}

/** 将美元成本格式化为 $ 前缀的紧凑文本（$0.1234 / $12.34 / $1.2K）。 */
export function formatCost(cost: number): string {
  if (cost === 0) return "$0";
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
  if (n >= 1000) return `${Number((n / 1000).toFixed(0))}K`;
  return String(n);
}
