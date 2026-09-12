import { cn } from "@/lib/utils";

// 指示灯状态：运行、已停止、流式转发、成功、错误、超时
export type LampState = "running" | "stopped" | "streaming" | "ok" | "error" | "timeout";

// 各状态对应的灯体颜色与发光样式
const LAMP_CLASS: Record<LampState, string> = {
  running: "bg-signal-success shadow-[0_0_8px_rgba(63,182,139,0.8)]",
  streaming: "bg-signal-warn shadow-[0_0_10px_rgba(245,165,36,0.9)]",
  stopped: "bg-muted-foreground",
  ok: "bg-signal-success",
  error: "bg-signal-error shadow-[0_0_8px_rgba(229,83,75,0.7)]",
  timeout: "bg-signal-warn",
};

/** 代理状态指示灯，根据运行状态显示不同颜色，pulse 为 true 时带脉冲动画。 */
export function StatusLamp({
  state,
  pulse = false,
  className,
}: {
  state: LampState;
  pulse?: boolean;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "inline-block h-2 w-2 rounded-full transition-colors duration-200",
        LAMP_CLASS[state],
        pulse && "animate-pulse",
        className,
      )}
      aria-hidden
    />
  );
}
