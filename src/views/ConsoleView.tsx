import { useCallback, useEffect, useRef, useState } from "react";
import { Activity, Play, RefreshCw, RotateCw, Square } from "lucide-react";
import { toast } from "sonner";

import { RelayRail } from "@/components/RelayRail";
import { StatusLamp } from "@/components/StatusLamp";
import { UrlRow } from "@/components/UrlRow";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { api, onRequest, onStatus, type ProxyStatus, type RequestInfo } from "@/lib/api";
import { formatUptime } from "@/lib/format";
import { cn } from "@/lib/utils";

export function ConsoleView() {
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [requests, setRequests] = useState<RequestInfo[]>([]);
  const [busy, setBusy] = useState<"start" | "stop" | "restart" | null>(null);
  const mounted = useRef(true);

  const refreshStatus = useCallback(async () => {
    try {
      const s = await api.proxyStatus();
      if (mounted.current) setStatus(s);
    } catch {
      /* 忽略轮询错误 */
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    refreshStatus();
    api.requestsGet(20).then((r) => mounted.current && setRequests(r)).catch(() => {});
    const offStatus = onStatus((s) => setStatus(s));
    const offRequest = onRequest((r) =>
      setRequests((prev) => {
        const next = [r, ...prev.filter((x) => x.id !== r.id)];
        return next.slice(0, 30);
      }),
    );
    const timer = setInterval(refreshStatus, 3000);
    return () => {
      mounted.current = false;
      clearInterval(timer);
      offStatus.then((f) => f());
      offRequest.then((f) => f());
    };
  }, [refreshStatus]);

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
      toast.success(action === "start" ? "代理已启动" : action === "stop" ? "代理已停止" : "代理已重启");
    } catch (e) {
      toast.error(String(e));
    } finally {
      setBusy(null);
    }
  }

  async function checkHealth() {
    if (!status?.running) {
      toast.error("代理未运行，先启动代理再健康检查");
      return;
    }
    try {
      const r = await fetch(`${status?.anthropic_url ?? `http://127.0.0.1:${status?.port ?? 3050}`}/health`);
      if (r.ok) {
        toast.success("健康检查通过（OK）");
      } else {
        toast.error(`健康检查失败：HTTP ${r.status}`);
      }
    } catch {
      toast.error("健康检查失败：无法连接代理");
    }
  }

  const running = status?.running ?? false;
  const streaming = requests.some((r) => r.status === "streaming");
  const statusTextClass = running
    ? streaming
      ? "text-signal-warn"
      : "text-signal-success"
    : "text-muted-foreground";

  return (
    <div className="space-y-4">
      <RelayRail running={running} streaming={streaming} port={status?.port ?? 3050} requests={requests} />

      <div className="grid grid-cols-3 gap-4">
        <Card className="min-w-0">
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">运行状态</CardTitle>
          </CardHeader>
          <CardContent className="flex items-center gap-2 pt-0">
            <StatusLamp state={running ? (streaming ? "streaming" : "running") : "stopped"} pulse={running} />
            <span className={cn("min-w-0 truncate text-lg font-semibold", statusTextClass)}>
              {running ? (streaming ? "转发中" : "运行中") : "已停止"}
            </span>
            {running && status && (
              <span className="ml-auto whitespace-nowrap text-xs text-muted-foreground">
                已运行 {formatUptime(status.uptime_secs)}
              </span>
            )}
          </CardContent>
        </Card>

        <Card className="min-w-0">
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">监听端口</CardTitle>
          </CardHeader>
          <CardContent className="pt-0">
            <span className="font-mono text-lg font-semibold">{status ? `${status.host}:${status.port}` : "—"}</span>
          </CardContent>
        </Card>

        <Card className="min-w-0">
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">上游版本</CardTitle>
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

      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm">代理地址</CardTitle>
          <CardDescription>粘贴到 Cursor / OpenCode / SDK 的 base URL</CardDescription>
        </CardHeader>
        <CardContent className="space-y-2 pt-0">
          <UrlRow url={status?.url ?? `http://127.0.0.1:${status?.port ?? 3050}/v1`} label="OpenAI" />
          <UrlRow url={status?.anthropic_url ?? `http://127.0.0.1:${status?.port ?? 3050}`} label="Anthropic" />
        </CardContent>
      </Card>

      <div className="flex items-center gap-2">
        <Button disabled={running || busy !== null} onClick={() => run("start")}>
          {busy === "start" ? <RefreshCw className="animate-spin" /> : <Play />}
          启动代理
        </Button>
        <Button variant="destructive" disabled={!running || busy !== null} onClick={() => run("stop")}>
          {busy === "stop" ? <RefreshCw className="animate-spin" /> : <Square />}
          停止代理
        </Button>
        <Button variant="secondary" disabled={!running || busy !== null} onClick={() => run("restart")}>
          {busy === "restart" ? <RefreshCw className="animate-spin" /> : <RotateCw />}
          重启代理
        </Button>
        <Button variant="secondary" onClick={checkHealth} title="检查代理健康状态">
          <Activity />
          健康检查
        </Button>
      </div>
    </div>
  );
}
