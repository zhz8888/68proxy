import { useEffect, useState } from "react";
import { Activity } from "lucide-react";

import { ModelLogo } from "@/components/ModelLogo";
import { StatusLamp, type LampState } from "@/components/StatusLamp";
import { ScrollArea } from "@/components/ui/scroll-area";
import { api, onRequest, onStatus, type RequestInfo } from "@/lib/api";
import { formatDuration, formatLogTime } from "@/lib/format";

/** 把请求状态字符串映射为指示灯状态。 */
function lampForStatus(status: string): LampState {
  switch (status) {
    case "streaming":
      return "streaming";
    case "ok":
      return "ok";
    case "timeout":
      return "timeout";
    case "error":
      return "error";
    default:
      return "stopped";
  }
}

/** 把请求状态字符串翻译成中文文案。 */
function statusLabel(status: string): string {
  switch (status) {
    case "streaming":
      return "流式中";
    case "ok":
      return "成功";
    case "timeout":
      return "超时";
    case "error":
      return "错误";
    case "disconnect":
      return "断连";
    default:
      return status;
  }
}

/** 中继视图：实时展示代理启动以来每一次请求的中继记录。 */
export function RelayView() {
  const [relay, setRelay] = useState<RequestInfo[]>([]);

  // 挂载时加载历史记录并订阅请求/状态事件，卸载时取消订阅
  useEffect(() => {
    // 初始拉取最近 500 条请求记录
    api
      .requestsGet(500)
      .then((r) => setRelay(r))
      .catch(() => {});
    // 新请求插入列表头部并按 id 去重，最多保留 500 条
    const offReq = onRequest((r) =>
      setRelay((prev) => {
        const next = [r, ...prev.filter((x) => x.id !== r.id)];
        return next.slice(0, 500);
      }),
    );
    // 代理停止时后端会丢弃记录，前端同步清空列表
    const offStatus = onStatus((s) => {
      if (!s.running) setRelay([]);
    });
    return () => {
      offReq.then((f) => f());
      offStatus.then((f) => f());
    };
  }, []);

  return (
    <div className="flex h-full flex-col gap-3">
      <div className="flex items-center gap-2">
        <div className="flex items-center gap-2 rounded-lg bg-muted px-3 py-1.5 text-xs text-muted-foreground">
          <Activity className="h-3.5 w-3.5" />
          自代理启动以来 · 共 {relay.length} 条
        </div>
      </div>

      <ScrollArea className="flex-1 rounded-lg border border-border bg-card/60">
        <div className="space-y-1 p-3">
          {relay.length === 0 ? (
            <p className="px-2 py-16 text-center text-sm text-muted-foreground">
              代理启动后，这里会实时显示每次请求的中继记录。代理停止或关闭软件时自动清空。
            </p>
          ) : (
            relay.map((r) => (
              <div
                key={r.id}
                className="flex items-center gap-3 rounded-md border border-border/70 bg-secondary/30 px-3 py-2"
              >
                <ModelLogo model={r.model} size={16} className="!p-0.5" />
                <span className="w-24 shrink-0 select-text font-mono text-xs text-muted-foreground/80">
                  {formatLogTime(r.started_at)}
                </span>
                <span className="min-w-0 truncate select-text font-mono text-sm text-foreground/90">{r.model}</span>
                <span className="min-w-0 truncate text-xs text-muted-foreground">{r.path}</span>
                <span className="flex-1" />
                <StatusLamp state={lampForStatus(r.status)} pulse={r.status === "streaming"} />
                <span className="w-12 text-right whitespace-nowrap text-xs text-muted-foreground">
                  {statusLabel(r.status)}
                </span>
                <span className="w-16 text-right whitespace-nowrap font-mono text-xs text-muted-foreground">
                  {formatDuration(r.elapsed_ms)}
                </span>
              </div>
            ))
          )}
        </div>
      </ScrollArea>
    </div>
  );
}
