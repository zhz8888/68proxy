// i18next 类型声明：把 zh.json 作为翻译 key 的类型来源。
//
// 声明后 `t("nav.console")` 等静态 key 会在编译期校验，zh.json 中缺失的 key 会直接
// 报错（en.json 结构与 zh.json 必须一致，见 scripts 约定的键结构对齐）。
// 运行时拼接的动态 key 请改用 @/i18n 的 translate()。

import "i18next";

import type zh from "@/i18n/locales/zh.json";

declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "translation";
    resources: {
      translation: typeof zh;
    };
  }
}
