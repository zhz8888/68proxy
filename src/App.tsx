import { useEffect, useState } from "react";
import {
  Boxes,
  Info,
  LayoutDashboard,
  ListTree,
  Minus,
  Play,
  Plug,
  Settings2,
  Square,
  Terminal,
  X,
  type LucideIcon,
} from "lucide-react";
import { toast } from "sonner";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { StatusLamp } from "@/components/StatusLamp";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { UrlRow } from "@/components/UrlRow";
import { Button } from "@/components/ui/button";
import { api, onStatus, type ProxyStatus } from "@/lib/api";
import { cn } from "@/lib/utils";
import { AboutView } from "@/views/AboutView";
import { ConfigView } from "@/views/ConfigView";
import { ConsoleView } from "@/views/ConsoleView";
import { LogsView } from "@/views/LogsView";
import { ModelsView } from "@/views/ModelsView";
import { RelayView } from "@/views/RelayView";
import { ToolsView } from "@/views/ToolsView";

type View = "console" | "logs" | "relay" | "config" | "models" | "tools" | "about";

const NAV: Array<{ id: View; label: string; icon: LucideIcon }> = [
  { id: "console", label: "控制台", icon: LayoutDashboard },
  { id: "logs", label: "调试日志", icon: Terminal },
  { id: "relay", label: "中继记录", icon: ListTree },
  { id: "models", label: "模型列表", icon: Boxes },
  { id: "tools", label: "工具接入", icon: Plug },
  { id: "config", label: "配置", icon: Settings2 },
  { id: "about", label: "关于", icon: Info },
];

const win = getCurrentWindow();

function App() {
  const [view, setView] = useState<View>("console");
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [busy, setBusy] = useState<"start" | "stop" | null>(null);
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    api.proxyStatus().then(setStatus).catch(() => {});
    const off = onStatus(setStatus);
    const timer = setInterval(() => {
      api.proxyStatus().then(setStatus).catch(() => {});
    }, 3000);
    win.isMaximized().then(setMaximized).catch(() => {});
    return () => {
      clearInterval(timer);
      off.then((f) => f());
    };
  }, []);

  async function toggle() {
    const running = status?.running ?? false;
    setBusy(running ? "stop" : "start");
    try {
      const s = running ? await api.proxyStop() : await api.proxyStart();
      setStatus(s);
      toast.success(running ? "代理已停止" : "代理已启动");
    } catch (e) {
      toast.error(String(e));
    } finally {
      setBusy(null);
    }
  }

  async function toggleMaximize() {
    const isMax = await win.isMaximized();
    if (isMax) {
      await win.unmaximize();
    } else {
      await win.maximize();
    }
    setMaximized(!isMax);
  }

  const running = status?.running ?? false;

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex h-screen overflow-hidden bg-background text-foreground">
        {/* 左侧导航栏 */}
        <aside className="flex w-52 shrink-0 flex-col border-r border-border bg-card/50">
          <div
            data-tauri-drag-region
            onDoubleClick={toggleMaximize}
            className="flex h-11 shrink-0 select-none items-center gap-2 border-b border-border px-4"
          >
            <span className="font-display text-[14px] font-semibold tracking-[0.22em] text-primary">
              68PROXY
            </span>
          </div>
          <nav className="flex flex-col gap-1 p-3">
            {NAV.map((item) => {
              const Icon = item.icon;
              return (
                <button
                  key={item.id}
                  onClick={() => setView(item.id)}
                  className={cn(
                    "flex items-center gap-2.5 rounded-md px-3 py-2 text-sm transition-colors",
                    view === item.id
                      ? "bg-foreground text-background shadow"
                      : "text-muted-foreground hover:bg-secondary hover:text-foreground",
                  )}
                >
                  <Icon className="h-4 w-4" />
                  {item.label}
                </button>
              );
            })}
          </nav>
          <div className="mt-auto p-4">
            <p className="font-mono text-[10px] leading-relaxed text-muted-foreground/70">
              Developed by 6ix8ight
              <br />
              V1.0
            </p>
          </div>
        </aside>

        <main className="flex min-w-0 flex-1 flex-col">
          {/* 顶部状态栏（兼作窗口拖动区） */}
          <header
            data-tauri-drag-region
            className="flex h-11 shrink-0 select-none items-center gap-3 border-b border-border px-3"
          >
            <StatusLamp state={running ? "running" : "stopped"} pulse={running} />
            <span
              className={cn(
                "whitespace-nowrap text-xs",
                running ? "text-signal-success" : "text-muted-foreground",
              )}
            >
              {running ? "运行中" : "已停止"}
            </span>
            <UrlRow url={status?.url ?? "http://127.0.0.1:3050/v1"} className="w-64 min-w-0" />
            <div data-tauri-drag-region onDoubleClick={toggleMaximize} className="min-w-0 flex-1" />
            <Button
              variant={running ? "destructive" : "default"}
              size="sm"
              className="h-7 px-2.5"
              onClick={toggle}
              disabled={busy !== null}
            >
              {running ? <Square /> : <Play />}
              {running ? "停止" : "启动"}
            </Button>

            <div className="flex items-center">
              <button
                onClick={() => win.minimize()}
                className="flex h-8 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
                title="最小化"
              >
                <Minus className="h-4 w-4" />
              </button>
              <button
                onClick={toggleMaximize}
                className="flex h-8 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
                title={maximized ? "还原" : "最大化"}
              >
                <Square className="h-3.5 w-3.5" />
              </button>
              <button
                onClick={() => win.close()}
                className="flex h-8 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-foreground hover:text-background"
                title="关闭"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
          </header>

          <div className="min-h-0 flex-1 overflow-hidden p-4">
            {view === "logs" || view === "models" || view === "relay" ? (
              <div className="h-full">
                {view === "logs" ? (
                  <LogsView />
                ) : view === "models" ? (
                  <ModelsView />
                ) : (
                  <RelayView />
                )}
              </div>
            ) : (
            <div className="h-full overflow-y-auto pr-2">
              {view === "console" ? (
                <ConsoleView />
              ) : view === "tools" ? (
                <ToolsView />
              ) : view === "about" ? (
                <AboutView />
              ) : (
                <ConfigView />
              )}
            </div>
            )}
          </div>
        </main>
        <Toaster position="bottom-right" />
      </div>
    </TooltipProvider>
  );
}

export default App;
