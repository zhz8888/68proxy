// 平台适配层：把「仅桌面端可用」的能力（窗口控制、文件对话框、打开外链、应用版本）
// 统一包装出浏览器可用的降级实现。
//
// 浏览器直连开发页面（Vite devUrl）调试时，这些能力并非核心——窗口操作无意义、
// 文件对话框不可用——但调用它们不能导致白屏或未捕获异常，故此处逐个降级。

import { getCurrentWindow } from "@tauri-apps/api/window";
import { getVersion } from "@tauri-apps/api/app";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";

import { IN_TAURI } from "@/lib/ipc";

/** 窗口控制句柄：浏览器下所有操作降级为无操作，避免调用 Tauri 内部结构报错。 */
export const appWindow = {
  /** 最小化窗口（浏览器下无效）。 */
  minimize: () => {
    if (IN_TAURI) void getCurrentWindow().minimize();
  },
  /** 最大化/还原切换（浏览器下无效）。 */
  toggleMaximize: async () => {
    if (!IN_TAURI) return false;
    const w = getCurrentWindow();
    const isMax = await w.isMaximized();
    if (isMax) await w.unmaximize();
    else await w.maximize();
    return !isMax;
  },
  /** 查询是否已最大化（浏览器下恒为 false）。 */
  isMaximized: async () => (IN_TAURI ? getCurrentWindow().isMaximized() : false),
  /** 关闭窗口（浏览器下无效）。 */
  close: () => {
    if (IN_TAURI) void getCurrentWindow().close();
  },
};

/** 读取应用版本；浏览器调试环境下无此信息，返回空串由界面自行省略。 */
export async function appVersion(): Promise<string> {
  if (!IN_TAURI) return "";
  return getVersion().catch(() => "");
}

/**
 * 弹出保存路径选择框（桌面端）并返回所选路径；用户取消返回 null。
 *
 * 浏览器调试环境下无原生对话框，抛出明确提示而非静默失败——
 * 导出日志等依赖路径的操作请在桌面端使用。
 */
export async function pickSavePath(defaultPath: string, extensions: string[]): Promise<string | null> {
  if (!IN_TAURI) {
    throw new Error("保存文件对话框仅在桌面应用内可用，请在 68proxy 窗口中使用该功能");
  }
  return saveDialog({
    defaultPath,
    filters: [{ name: "文件", extensions }],
  });
}

/** 用系统浏览器打开链接；浏览器环境下改为新标签页打开。 */
export async function openExternal(url: string): Promise<void> {
  if (IN_TAURI) return openUrl(url);
  window.open(url, "_blank", "noopener,noreferrer");
}
