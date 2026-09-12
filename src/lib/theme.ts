// 主题（深色 / 浅色 / 跟随系统）的应用与持久化。
//
// 颜色本身由 index.css 的 :root（浅色）与 .dark（深色）两个变量块定义，
// 此处只负责在 <html> 上增删 `dark` 类并缓存选择：
// - 缓存写入 localStorage，供 index.html 的内联脚本在首屏同步应用（避免闪白/闪黑）；
// - 后端 Config.theme 才是持久化真源，启动加载配置后再同步一次。

/** 主题模式：跟随系统 / 深色 / 浅色。 */
export type ThemeMode = "system" | "dark" | "light";

/** localStorage 中缓存主题选择的键（与 index.html 内联脚本保持一致）。 */
const STORAGE_KEY = "68proxy-theme";

/** 系统深色偏好的媒体查询。 */
const darkQuery = () => window.matchMedia("(prefers-color-scheme: dark)");

/** 把主题模式解析为实际生效的明暗（system 时取系统偏好）。 */
export function resolveDark(mode: ThemeMode): boolean {
  if (mode === "dark") return true;
  if (mode === "light") return false;
  return darkQuery().matches;
}

/**
 * 应用主题：切换 `<html>` 的 dark 类，并把选择写入 localStorage。
 *
 * 立即生效、无需重启；调用方在配置页改动时同步更新后端配置以持久化。
 */
export function applyTheme(mode: ThemeMode): void {
  document.documentElement.classList.toggle("dark", resolveDark(mode));
  try {
    localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    /* 隐私模式等场景下写入失败不影响本次应用 */
  }
}

/** 读取缓存的主题模式（无缓存或非法值时回退「跟随系统」）。 */
export function cachedTheme(): ThemeMode {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v === "dark" || v === "light" || v === "system") return v;
  } catch {
    /* 读取失败按默认处理 */
  }
  return "system";
}

/** 监听系统明暗变化：仅当模式为「跟随系统」时重新应用（返回取消订阅函数）。 */
export function watchSystemTheme(getMode: () => ThemeMode): () => void {
  const mq = darkQuery();
  const handler = () => {
    if (getMode() === "system") applyTheme("system");
  };
  mq.addEventListener("change", handler);
  return () => mq.removeEventListener("change", handler);
}
