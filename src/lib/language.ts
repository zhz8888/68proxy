// 界面语言（简体中文 / English）的缓存读取。
//
// 与主题（theme.ts）同构：选择缓存到 localStorage，供 index.html 的内联脚本在首屏
// 同步设置 <html lang>；后端 Config.language 才是持久化真源，启动加载配置后再同步一次。
// i18next 实例与应用逻辑在 @/i18n，此处只保留不依赖 i18next 的纯存储工具，避免循环引用。

/** 界面语言：简体中文 / 英文。 */
export type Language = "zh" | "en";

/** localStorage 中缓存语言选择的键（与 index.html 内联脚本保持一致）。 */
export const LANGUAGE_STORAGE_KEY = "68proxy-language";

/** 语言对应的 <html lang> 值。 */
export function htmlLang(lang: Language): string {
  return lang === "en" ? "en" : "zh-CN";
}

/** 写入缓存的语言选择（隐私模式等场景下写入失败不影响本次应用）。 */
export function setCachedLanguage(lang: Language): void {
  try {
    localStorage.setItem(LANGUAGE_STORAGE_KEY, lang);
  } catch {
    /* 忽略写入失败 */
  }
}

/** 读取缓存的语言（无缓存或非法值时回退简体中文）。 */
export function cachedLanguage(): Language {
  try {
    const v = localStorage.getItem(LANGUAGE_STORAGE_KEY);
    if (v === "zh" || v === "en") return v;
  } catch {
    /* 读取失败按默认处理 */
  }
  return "zh";
}
