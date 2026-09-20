import { useCallback, useEffect, useRef, useState } from "react";
import { Activity, Play, RefreshCw, RotateCw, Square } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { RecentRequestsCard } from "@/components/RecentRequestsCard";
import { StatusLamp } from "@/components/StatusLamp";
import { UrlRow } from "@/components/UrlRow";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { api, onRequest, onStatus, type ProxyStatus, type RequestInfo } from "@/lib/api";
import { DEFAULT_PORT } from "@/lib/constants";
import { formatUptime } from "@/lib/format";
import { errText } from "@/lib/messages";
import { useVisiblePolling } from "@/lib/useVisiblePolling";
import { cn } from "@/lib/utils";

/** 控制台视图：展示代理运行状态、监听端口与代理地址，并提供启动/停止/重启及健康检查操作。 */
export function ConsoleView() {
  const { t } = useTranslation();
  /** 最新代理运行状态（null 表示尚未加载）。 */
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  /** 最近中继请求摘要（新在前）。 */
  const [requests, setRequests] = useState<RequestInfo[]>([]);
  /** 启停进行中的操作（防重复点击，null 表示空闲）。 */
  const [busy, setBusy] = useState<"start" | "stop" | "restart" | null>(null);
  const mounted = useRef(true); // 组件是否仍挂载，避免卸载后的异步回调再 setState

  /** 拉取一次代理状态；组件已卸载时丢弃结果。 */
  const refreshStatus = useCallback(async () => {
    try {
      const s = await api.proxyStatus();
      if (mounted.current) setStatus(s);
    } catch {
      /* 忽略轮询错误 */
    }
  }, []);

  // 挂载时加载初始数据并订阅后端事件，卸载时全部清理
  useEffect(() => {
    mounted.current = true;
    refreshStatus();
    // 初始拉取最近 20 条请求记录：与订阅事件合并去重而非整体替换，
    // 否则快照返回前推入的实时事件会被覆盖丢弃（本视图不重新拉取，丢了就不再出现）
    api
      .requestsGet(20)
      .then((snapshot) =>
        mounted.current &&
        setRequests((prev) => {
          const byId = new Map(prev.map((x) => [x.id, x]));
          for (const r of snapshot) byId.set(r.id, r);
          return [...byId.values()].sort((a, b) => b.started_at - a.started_at).slice(0, 30);
        }),
      )
      .catch(() => {});
    // 订阅后端推送的代理状态变化
    const offStatus = onStatus((s) => setStatus(s));
    // 订阅请求事件：新记录插到列表头部并按 id 去重，最多保留 30 条
    const offRequest = onRequest((r) =>
      setRequests((prev) => {
        const next = [r, ...prev.filter((x) => x.id !== r.id)];
        return next.slice(0, 30);
      }),
    );
    // 事件推送之外的兜底轮询见下方 useVisiblePolling
    return () => {
      // 卸载：停止异步回写并取消事件订阅
      mounted.current = false;
      offStatus.then((f) => f());
      offRequest.then((f) => f());
    };
  }, [refreshStatus]);

  // 状态兜底轮询：窗口隐藏时暂停，重新可见时立即刷一次
  useVisiblePolling(refreshStatus, 3000);
  // 请求列表低频自愈：终止事件丢失时 streaming 会永久卡死，
  // 每 10s 重拉一次合并（与事件增量同去重口径），丢失即自愈。
  useVisiblePolling(
    useCallback(async () => {
      try {
        const snapshot = await api.requestsGet(30);
        if (mounted.current) {
          setRequests((prev) => {
            const byId = new Map(snapshot.map((x) => [x.id, x]));
            for (const r of prev) {
              if (r.status === "streaming" && !byId.has(r.id)) {
                // 快照无此 id：若已开始超过 10 分钟，视为事件丢失，降级为 disconnect
                if (Date.now() - r.started_at > 10 * 60 * 1000) {
                  byId.set(r.id, { ...r, status: "disconnect" });
                  continue;
                }
              }
              if (!byId.has(r.id)) byId.set(r.id, r);
              else {
                // 快照优先（携带终止态），本地 streaming 行被覆盖即自愈
                const cur = byId.get(r.id)!;
                if (cur.status !== "streaming") byId.set(r.id, cur);
                else byId.set(r.id, r.status === "streaming" ? cur : r);
              }
            }
            return [...byId.values()].sort((a, b) => b.started_at - a.started_at).slice(0, 30);
          });
        }
      } catch {
        /* 忽略轮询错误 */
      }
    }, []),
    10000,
  );

  /** 执行启动/停止/重启操作，执行期间通过 busy 禁用按钮并反馈结果。 */
  async function run(action: "start" | "stop" | "restart") {
    setBusy(action);
    try {
      const s =
        action === "start"
          ? await api.proxyStart()
          : action === "stop"
            ? await api.proxyStop()
            : await api.proxyRestart();
      setStatus(s);
      toast.success(
        t(
          action === "start"
            ? "console.proxyStarted"
            : action === "stop"
              ? "console.proxyStopped"
              : "console.proxyRestarted",
        ),
      );
    } catch (e) {
      toast.error(errText(e));
    } finally {
      setBusy(null);
    }
  }

  /** 请求代理的 /health 端点验证可用性；代理未运行时直接提示。 */
  async function checkHealth() {
    if (!status?.running || !status) {
      toast.error(t("console.healthNotRunning"));
      return;
    }
    try {
      const r = await fetch(`${status.anthropic_url}/health`);
      if (r.ok) {
        toast.success(t("console.healthOk"));
      } else {
        toast.error(t("console.healthFailedHttp", { p0: r.status }));
      }
    } catch {
      toast.error(t("console.healthFailedConnect"));
    }
  }

  const running = status?.running ?? false;
  // 只要有一条请求仍在流式返回，就视为“转发中”
  const streaming = requests.some((r) => r.status === "streaming");
  // 状态文字颜色：转发中为警告色、运行中为成功色、已停止为灰色
  const statusTextClass = running
    ? streaming
      ? "text-signal-warn"
      : "text-signal-success"
    : "text-muted-foreground";

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-3 gap-4">
        <Card className="min-w-0">
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">{t("console.statusTitle")}</CardTitle>
          </CardHeader>
          <CardContent className="flex items-center gap-2 pt-0">
            <StatusLamp state={running ? (streaming ? "streaming" : "running") : "stopped"} pulse={running} />
            <span className={cn("min-w-0 truncate text-lg font-semibold", statusTextClass)}>
              {running
                ? streaming
                  ? t("console.statusStreaming")
                  : t("console.statusRunning")
                : t("console.statusStopped")}
            </span>
            {running && status && (
              <span className="ml-auto whitespace-nowrap text-xs text-muted-foreground">
                {t("console.uptime", { p0: formatUptime(status.uptime_secs) })}
              </span>
            )}
          </CardContent>
          {/* 自动启动/启动失败提示：未运行且最近一次启动带失败原因时展示（如端口被占用） */}
          {!running && status?.error && (
            <CardContent className="border-t border-destructive/30 bg-destructive/5 px-3 py-2 pt-2">
              <p className="text-xs leading-5 text-destructive">
                {t("console.startFailed", { p0: errText(status.error) })}
              </p>
            </CardContent>
          )}
        </Card>

        <Card className="min-w-0">
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">{t("console.portTitle")}</CardTitle>
          </CardHeader>
          <CardContent className="pt-0">
            <span className="font-mono text-lg font-semibold">{status ? `${status.host}:${status.port}` : "—"}</span>
          </CardContent>
        </Card>

        <Card className="min-w-0">
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              {t("console.upstreamVersionTitle")}
            </CardTitle>
          </CardHeader>
          <CardContent className="flex items-center gap-2 pt-0">
            <span className="min-w-0 truncate font-mono text-lg font-semibold">{status?.cc_version ?? "—"}</span>
            <Badge variant="secondary" className="ml-auto shrink-0 whitespace-nowrap">
              <Activity className="mr-1 h-3 w-3" />
              command-code
            </Badge>
          </CardContent>
        </Card>
      </div>

      <RecentRequestsCard limit={5} />

      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm">{t("console.proxyAddressTitle")}</CardTitle>
          <CardDescription>{t("console.proxyAddressDesc")}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-2 pt-0">
          <UrlRow url={status?.url ?? `http://127.0.0.1:${DEFAULT_PORT}/v1`} label="OpenAI" />
          <UrlRow url={status?.anthropic_url ?? `http://127.0.0.1:${DEFAULT_PORT}`} label="Anthropic" />
        </CardContent>
      </Card>

      <div className="flex items-center gap-2">
        <Button disabled={running || busy !== null} onClick={() => run("start")}>
          {busy === "start" ? <RefreshCw className="animate-spin" /> : <Play />}
          {t("console.start")}
        </Button>
        <Button variant="destructive" disabled={!running || busy !== null} onClick={() => run("stop")}>
          {busy === "stop" ? <RefreshCw className="animate-spin" /> : <Square />}
          {t("console.stop")}
        </Button>
        <Button variant="secondary" disabled={!running || busy !== null} onClick={() => run("restart")}>
          {busy === "restart" ? <RefreshCw className="animate-spin" /> : <RotateCw />}
          {t("console.restart")}
        </Button>
        <Button variant="secondary" onClick={checkHealth} title={t("console.healthCheckTitle")}>
          <Activity />
          {t("console.healthCheck")}
        </Button>
      </div>
    </div>
  );
}
