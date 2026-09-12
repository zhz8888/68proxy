// IPC 适配层：统一「Tauri 窗口」与「浏览器直连开发页面」两种运行方式。
//
// 在 Tauri 窗口内运行时（`window.__TAURI_INTERNALS__` 存在）直接走官方 IPC；
// 否则（用浏览器打开 Vite 开发页面，如 http://localhost:1420）改走 Rust 侧的
// 开发调试桥接：命令经 HTTP `POST /rpc`，事件经 SSE `GET /events`。
//
// 桥接仅在 debug 构建启用（见 src-tauri/src/dev_bridge.rs），生产构建下浏览器
// 直连会调用失败——这是预期行为，生产包只应在 Tauri 窗口内使用。

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn, type Event } from "@tauri-apps/api/event";

import { translate } from "@/i18n";

/** 桥接服务基址（与后端默认端口一致；可用 VITE_BRIDGE_PORT 覆写）。 */
const BRIDGE_BASE = `http://127.0.0.1:${import.meta.env.VITE_BRIDGE_PORT ?? "1431"}`;

/** 是否运行在 Tauri 窗口中（false 表示浏览器直连开发页面）。 */
export const IN_TAURI = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** 调用后端命令：Tauri 内走 IPC，浏览器内走调试桥接。 */
export async function invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (IN_TAURI) return tauriInvoke<T>(cmd, args);
  let res: Response;
  try {
    res = await fetch(`${BRIDGE_BASE}/rpc`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ cmd, args }),
    });
  } catch {
    // 网络层失败通常是后端未运行或桥接被生产构建排除
    throw new Error(translate("errors.bridge_unreachable", { p0: BRIDGE_BASE }));
  }
  const body = (await res.json()) as { ok: boolean; data?: T; error?: string };
  if (!body.ok) throw new Error(body.error ?? translate("errors.command_failed"));
  return body.data as T;
}

/** 订阅后端事件：Tauri 内走事件系统，浏览器内走共享 SSE 连接。 */
export async function listen<T>(
  event: string,
  handler: (e: { payload: T }) => void,
): Promise<UnlistenFn> {
  if (IN_TAURI) return tauriListen<T>(event, handler as (e: Event<T>) => void);
  // 浏览器对同一源的 HTTP/1.1 连接数有上限（Chrome 为 6），而每条 SSE 都长期占用一条；
  // 若每个订阅各开一条连接，多路订阅会瞬间占满连接池、把后续 /rpc 请求全部阻塞。
  // 故全局只维持一条 SSE 连接，在此按事件名分发。
  const bag = bridgeHandlers.get(event) ?? new Set<(e: { payload: unknown }) => void>();
  bridgeHandlers.set(event, bag);
  const fn = handler as (e: { payload: unknown }) => void;
  bag.add(fn);
  ensureBridgeStream();
  return () => {
    bag.delete(fn);
    if (bag.size === 0) bridgeHandlers.delete(event);
  };
}

/** 事件名 → 已注册的处理器集合（共享一条 SSE 连接）。 */
const bridgeHandlers = new Map<string, Set<(e: { payload: unknown }) => void>>();

/** 共享 SSE 连接的当前状态（用于重连与避免重复建立）。 */
let bridgeStream: { close: () => void } | null = null;

/** 确保共享 SSE 连接已建立（幂等；断线后自动重连）。 */
function ensureBridgeStream() {
  if (bridgeStream) return;
  const controller = new AbortController();
  bridgeStream = {
    close: () => {
      controller.abort();
      bridgeStream = null;
    },
  };
  void (async () => {
    try {
      const res = await fetch(`${BRIDGE_BASE}/events`, { signal: controller.signal });
      if (!res.body) throw new Error(translate("errors.event_stream_unavailable"));
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      // 逐块解析 SSE 帧（块间以空行分隔，同一事件可能被拆到多个 chunk）
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let sep: number;
        while ((sep = buffer.indexOf("\n\n")) !== -1) {
          const frame = buffer.slice(0, sep);
          buffer = buffer.slice(sep + 2);
          let name = "";
          const dataLines: string[] = [];
          for (const line of frame.split("\n")) {
            if (line.startsWith("event:")) name = line.slice(6).trim();
            else if (line.startsWith("data:")) dataLines.push(line.slice(5).trimStart());
          }
          const handlers = bridgeHandlers.get(name);
          if (handlers && dataLines.length > 0) {
            try {
              // 与 Tauri 事件同形：回调收到 { payload }，故 api.ts 的包装无需分支处理
              const payload = JSON.parse(dataLines.join("\n"));
              for (const h of handlers) h({ payload });
            } catch {
              /* 非 JSON 载荷忽略 */
            }
          }
        }
      }
    } catch {
      /* 连接失败/被中断：仅忽略，下一次订阅会重新建立 */
    } finally {
      bridgeStream = null;
    }
  })();
}
