import { useEffect, useRef } from "react";

/**
 * 可见性感知的轮询：窗口隐藏（最小化 / 切到别的应用）时暂停，重新可见时立即执行一次。
 *
 * 桌面应用长时间挂后台是常态，无条件轮询会持续发 IPC、并在统计页叠加重量级 SQL 聚合。
 * 隐藏期间只依赖后端事件推送（onStatus/onStats 等）保持实时性。
 *
 * @param fn 每次触发执行的函数（内部自行处理错误）；用 ref 持有，无需调用方 memo 化。
 * @param intervalMs 可见时的轮询间隔（毫秒）。
 */
export function useVisiblePolling(fn: () => void, intervalMs: number) {
  const fnRef = useRef(fn);
  fnRef.current = fn;

  useEffect(() => {
    let timer: ReturnType<typeof setInterval> | null = null;

    const start = () => {
      if (timer !== null) return;
      fnRef.current();
      timer = setInterval(() => {
        if (document.hidden) return;
        fnRef.current();
      }, intervalMs);
    };
    const stop = () => {
      if (timer !== null) {
        clearInterval(timer);
        timer = null;
      }
    };

    // 窗口重新可见时立刻刷新一次并恢复轮询；隐藏时停掉定时器
    const onVisibility = () => {
      if (document.hidden) stop();
      else start();
    };

    if (!document.hidden) start();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      stop();
    };
  }, [intervalMs]);
}
