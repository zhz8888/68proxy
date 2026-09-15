import { useEffect, useState } from "react";
import {
  BarChart3,
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
  UserRound,
  X,
  type LucideIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { applyLanguage, translate } from "@/i18n";
import { StatusLamp } from "@/components/StatusLamp";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { UrlRow } from "@/components/UrlRow";
import { Button } from "@/components/ui/button";
import { api, onStatus, type ProxyStatus } from "@/lib/api";
import { DEFAULT_PORT } from "@/lib/constants";
import { errText } from "@/lib/messages";
import { appVersion, appWindow } from "@/lib/platform";
import { applyTheme, watchSystemTheme } from "@/lib/theme";
import { useVisiblePolling } from "@/lib/useVisiblePolling";
import { cn } from "@/lib/utils";
import { AboutView } from "@/views/AboutView";
import { AccountsView } from "@/views/AccountsView";
import { ConfigView } from "@/views/ConfigView";
import { ConsoleView } from "@/views/ConsoleView";
import { LogsView } from "@/views/LogsView";
import { ModelsView } from "@/views/ModelsView";
import { RelayView } from "@/views/RelayView";
import { StatsView } from "@/views/StatsView";
import { ToolsView } from "@/views/ToolsView";

/** 应用主视图标识，与左侧导航项一一对应。 */
type View =
  | "console"
  | "logs"
  | "relay"
  | "stats"
  | "models"
  | "tools"
  | "accounts"
  | "config"
  | "about";

/** 左侧导航栏的菜单项配置：视图 id、文案 i18n key 与图标。 */
const NAV: Array<{ id: View; labelKey: string; icon: LucideIcon }> = [
  { id: "console", labelKey: "nav.console", icon: LayoutDashboard },
  { id: "logs", labelKey: "nav.logs", icon: Terminal },
  { id: "relay", labelKey: "nav.relay", icon: ListTree },
  { id: "stats", labelKey: "nav.stats", icon: BarChart3 },
  { id: "models", labelKey: "nav.models", icon: Boxes },
  { id: "tools", labelKey: "nav.tools", icon: Plug },
  { id: "accounts", labelKey: "nav.accounts", icon: UserRound },
  { id: "config", labelKey: "nav.config", icon: Settings2 },
  { id: "about", labelKey: "nav.about", icon: Info },
];

/** 侧栏版本号展示：dev 模式与 CI 短哈希构建显示纯标识（无 v 前缀），正式版保留 v 前缀。 */
function displayVersion(version: string): string {
  if (version === "dev") return version;
  // CI 构建版本为 0.0.0-<短哈希>（Tauri 要求 semver），显示层只取短哈希
  const ciPrefix = "0.0.0-";
  if (version.startsWith(ciPrefix)) {
    return version.slice(ciPrefix.length) || version;
  }
  return `v${version}`;
}

/** 应用主框架：左侧导航栏 + 顶部状态栏 + 按当前视图切换的内容区。 */
function App() {
  const { t } = useTranslation();
  const [view, setView] = useState<View>("console");
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [busy, setBusy] = useState<"start" | "stop" | null>(null);
  const [maximized, setMaximized] = useState(false);
  const [version, setVersion] = useState("");

  useEffect(() => {
    // 读取应用版本（来自 tauri.conf.json ← package.json，随发版 tag 自动联动）；
    // 浏览器调试环境下无版本信息，返回空串（界面已按空串省略显示）
    appVersion().then(setVersion).catch(() => {});
    // 主题与语言以后端配置为准：加载后各应用一次；index.html 内联脚本已按缓存值预设过，
    // 无缓存（首次运行）时此处补上，避免默认落在浅色/中文。
    let mounted = true;
    api.configGet()
      .then((c) => {
        if (!mounted) return;
        applyTheme(c.theme);
        applyLanguage(c.language);
      })
      .catch(() => {});
    // 系统明暗变化：仅在「跟随系统」模式下重新应用
    // （当前模式由 theme.ts 模块级记录，配置页改动主题后此处无需同步）
    const unwatchTheme = watchSystemTheme();
    // 挂载时拉取一次代理状态，并订阅后端推送；3 秒兜底轮询在窗口隐藏时自动暂停
    api.proxyStatus().then(setStatus).catch(() => {});
    const off = onStatus(setStatus);
    appWindow.isMaximized().then(setMaximized).catch(() => {});
    // 卸载时取消事件订阅（轮询由 useVisiblePolling 自行清理），避免泄漏
    return () => {
      mounted = false;
      off.then((f) => f());
      unwatchTheme();
    };
  }, []);

  // 状态兜底轮询：窗口隐藏时暂停，重新可见时立即刷一次
  useVisiblePolling(() => {
    api.proxyStatus().then(setStatus).catch(() => {});
  }, 3000);

  /** 启动或停止本地代理：运行中则停止，否则启动，期间禁用按钮防重复点击。 */
  async function toggle() {
    const running = status?.running ?? false;
    setBusy(running ? "stop" : "start");
    try {
      const s = running ? await api.proxyStop() : await api.proxyStart();
      setStatus(s);
      toast.success(running ? t("topbar.stoppedToast") : t("topbar.started"));
    } catch (e) {
      toast.error(errText(e));
    } finally {
      setBusy(null);
    }
  }

  /** 切换窗口最大化/还原状态，并同步本地 maximized 标记以更新按钮提示。 */
  async function toggleMaximize() {
    const isMax = await appWindow.toggleMaximize();
    setMaximized(isMax);
  }

  // 代理是否运行中，控制状态灯、文案与启停按钮样式
  const running = status?.running ?? false;

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex h-screen overflow-hidden rounded-[10px] bg-background text-foreground">
        {/* 左侧导航栏 */}
        <aside className="flex w-52 shrink-0 flex-col border-r border-border bg-card/50">
          <div
            data-tauri-drag-region
            onDoubleClick={toggleMaximize}
            className="flex h-11 shrink-0 select-none items-center gap-2 border-b border-border px-4"
          >
            <span className="font-display text-sm font-semibold tracking-[0.22em] text-primary">
              68Proxy
            </span>
          </div>
          {/* min-h-0 + overflow-y-auto：窗口高度不足时让导航自身滚动，
              避免导航把底部作者信息挤出可视区（侧栏外层是 overflow-hidden，溢出即被裁切） */}
          <nav className="flex min-h-0 flex-col gap-1 overflow-y-auto p-3">
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
                  {translate(item.labelKey)}
                </button>
              );
            })}
          </nav>
          {/* shrink-0：底部信息区固定高度，任何窗口高度下都完整可见 */}
          <div className="mt-auto shrink-0 p-4">
            <p className="font-mono text-2xs leading-relaxed text-muted-foreground">
              Develop by 6ix8ight
              <br />
              Fork by zhz8888
              {/* 版本号独占一行；浏览器调试环境无版本信息时整行省略，避免留下空行。
                  dev 模式显示 dev、CI 短哈希构建显示纯短哈希（均无 v 前缀），正式版保留 v 前缀。 */}
              {version && (
                <>
                  <br />
                  {displayVersion(version)}
                </>
              )}
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
              {running ? t("topbar.running") : t("topbar.stopped")}
            </span>
            <UrlRow url={status?.url ?? `http://127.0.0.1:${DEFAULT_PORT}/v1`} className="w-64 min-w-0" />
            <div data-tauri-drag-region onDoubleClick={toggleMaximize} className="min-w-0 flex-1" />
            <Button
              variant={running ? "destructive" : "default"}
              size="sm"
              className="h-7 px-2.5"
              onClick={toggle}
              disabled={busy !== null}
            >
              {running ? <Square /> : <Play />}
              {running ? t("topbar.stop") : t("topbar.start")}
            </Button>

            <div className="flex items-center">
              <button
                onClick={() => appWindow.minimize()}
                className="flex h-8 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
                title={t("topbar.minimize")}
              >
                <Minus className="h-4 w-4" />
              </button>
              <button
                onClick={toggleMaximize}
                className="flex h-8 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
                title={maximized ? t("topbar.restore") : t("topbar.maximize")}
              >
                <Square className="h-3.5 w-3.5" />
              </button>
              <button
                onClick={() => appWindow.close()}
                className="flex h-8 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-foreground hover:text-background"
                title={t("topbar.close")}
              >
                <X className="h-4 w-4" />
              </button>
            </div>
          </header>

          {/* 内容区：日志/模型/中继/统计视图占满高度，其余视图可纵向滚动 */}
          <div className="min-h-0 flex-1 overflow-hidden p-4">
            {view === "logs" || view === "models" || view === "relay" || view === "stats" ? (
              <div className="h-full">
                {view === "logs" ? (
                  <LogsView />
                ) : view === "models" ? (
                  <ModelsView />
                ) : view === "relay" ? (
                  <RelayView />
                ) : (
                  <StatsView />
                )}
              </div>
            ) : (
            <div className="h-full overflow-y-auto pr-2">
              {view === "console" ? (
                <ConsoleView />
              ) : view === "tools" ? (
                <ToolsView />
              ) : view === "accounts" ? (
                <AccountsView />
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
