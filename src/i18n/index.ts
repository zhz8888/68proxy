// 国际化（i18n）实例与语言应用逻辑。
//
// - 语言包为单一命名空间（translation）的嵌套字典，见 ./locales/zh.json 与 en.json；
// - 首屏语言取自 localStorage 缓存（cachedLanguage），避免首屏闪回默认语言；
// - 后端 Config.language 是持久化真源，App 启动加载配置后再同步一次；
// - 切换语言无需 Provider：react-i18next 在初始化时已注册到全局 i18next 实例。

import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import en from "@/i18n/locales/en.json";
import zh from "@/i18n/locales/zh.json";
import {
  cachedLanguage,
  htmlLang,
  setCachedLanguage,
  type Language,
} from "@/lib/language";

export type { Language };

/** i18n 初始化：资源、初始语言、回退语言与插值设置。 */
void i18n.use(initReactI18next).init({
  resources: {
    zh: { translation: zh },
    en: { translation: en },
  },
  lng: cachedLanguage(),
  fallbackLng: "zh",
  // React 已完成转义，此处关闭 i18next 的 HTML 转义，允许译文包含普通标点与引号
  interpolation: { escapeValue: false },
  returnNull: false,
});

/** 同步 <html lang>，便于无障碍与浏览器断词等按语言处理。 */
function syncHtmlLang(lang: Language): void {
  document.documentElement.lang = htmlLang(lang);
}

syncHtmlLang(cachedLanguage());

/**
 * 应用语言：切换 i18next 语言、写入 localStorage 缓存并同步 <html lang>。
 *
 * 立即生效、无需重启；调用方在配置页改动时同步更新后端配置以持久化。
 * 传入非法值（如旧后端未返回 language 字段）时忽略，保留当前语言。
 */
export function applyLanguage(lang: string | undefined): void {
  if (lang !== "zh" && lang !== "en") return;
  void i18n.changeLanguage(lang);
  setCachedLanguage(lang);
  syncHtmlLang(lang);
}

/**
 * 动态 key 的翻译：`t()` 的静态 key 类型来自 zh.json，此处用于运行时拼接的 key
 * （如后端错误码 `errors.<code>`），绕过编译期 key 校验。
 */
export function translate(key: string, opts?: Record<string, unknown>): string {
  return (i18n.t as unknown as (k: string, o?: Record<string, unknown>) => string)(key, opts);
}

export default i18n;
