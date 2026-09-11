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
