import { cn } from "@/lib/utils";

export type LampState = "running" | "stopped" | "streaming" | "ok" | "error" | "timeout";

const LAMP_CLASS: Record<LampState, string> = {
  running: "bg-signal-success shadow-[0_0_8px_rgba(63,182,139,0.8)]",
  streaming: "bg-signal-warn shadow-[0_0_10px_rgba(245,165,36,0.9)]",
  stopped: "bg-muted-foreground/50",
  ok: "bg-signal-success",
  error: "bg-signal-error shadow-[0_0_8px_rgba(229,83,75,0.7)]",
  timeout: "bg-signal-warn",
};

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
