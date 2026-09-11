import { useState } from "react";
import type { ReactNode } from "react";
import { AppWindow, Cloud, Server } from "lucide-react";

import { ModelLogo } from "@/components/ModelLogo";
import { StatusLamp } from "@/components/StatusLamp";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { formatDuration } from "@/lib/format";
import { lampForStatus, statusLabel } from "@/lib/status";
import type { RequestInfo } from "@/lib/api";
import { cn } from "@/lib/utils";

/** 中继轨道上的一个节点（客户端/代理/上游）。
 * @param left 节点在轨道内的水平位置（CSS 定位表达式）
 * @param accent 高亮配色：amber 强调代理节点、blue 强调上游节点
 */
function Node({
  label,
  sub,
  icon,
  accent,
  left,
}: {
  label: string;
  sub?: string;
  icon?: ReactNode;
  accent?: "amber" | "blue";
  left: string;
}) {
  const boxClass =
    accent === "amber"
      ? "border-signal-warn/60 bg-signal-warn/15 text-signal-warn shadow-[0_0_10px_rgba(245,165,36,0.25)]"
      : accent === "blue"
        ? "border-signal-info/60 bg-signal-info/15 text-signal-info shadow-[0_0_10px_rgba(111,179,224,0.25)]"
        : "border-border bg-card text-muted-foreground";
  const labelClass =
    accent === "amber"
      ? "text-signal-warn"
      : accent === "blue"
        ? "text-signal-info"
        : "text-muted-foreground";
  return (
    <div
      className="absolute top-1/2 flex -translate-x-1/2 -translate-y-1/2 flex-col items-center gap-1"
      style={{ left }}
    >
      <div className={cn("flex h-8 w-8 items-center justify-center rounded-full border", boxClass)}>
        {icon ?? <Server className="h-4 w-4" />}
      </div>
      <span className={cn("whitespace-nowrap text-[11px] leading-none", labelClass)}>
        {label}
      </span>
      {sub && (
        <span className="max-w-[88px] truncate whitespace-nowrap font-mono text-[8px] leading-none text-muted-foreground/70">
          {sub}
        </span>
      )}
    </div>
  );
}

/** 实时中继轨道卡片：可视化“客户端 → 代理 → 上游”链路，展示最近一次请求并可点开详情。 */
export function RelayRail({
  running,
  streaming,
  port,
  requests,
}: {
  running: boolean;
  streaming: boolean;
  port: number;
  requests: RequestInfo[];
}) {
  const [detail, setDetail] = useState<RequestInfo | null>(null);

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between">
          <CardTitle className="text-sm">实时中继轨道</CardTitle>
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <StatusLamp
              state={running ? (streaming ? "streaming" : "running") : "stopped"}
              pulse={running}
            />
            {running ? (streaming ? "转发中" : "链路就绪") : "已停止"}
          </div>
        </div>
      </CardHeader>
      <CardContent className="pt-0">
        <div className="relative h-16">
          <div
            className={cn(
              "absolute inset-x-10 top-[22px] h-px",
              running
                ? "bg-gradient-to-r from-border via-signal-warn/50 to-signal-info/50"
                : "bg-border",
            )}
          />
          {/* 运行时轨道上有正向流动的光点；流式转发时叠加一个反向流动的光点 */}
          {running && (
            <span className="rail-dot absolute top-[19px] -mt-0.5 h-1.5 w-1.5 rounded-full bg-signal-warn shadow-[0_0_8px_rgba(245,165,36,0.9)]" />
          )}
          {streaming && (
            <span className="rail-dot-reverse absolute top-[19px] -mt-0.5 h-1.5 w-1.5 rounded-full bg-signal-info shadow-[0_0_8px_rgba(111,179,224,0.9)]" />
          )}
          <Node left="1rem" label="客户端" icon={<AppWindow className="h-4 w-4" />} />
          <Node left="calc(50% - 16px)" label="代理" sub={`127.0.0.1:${port}`} accent="amber" icon={<Server className="h-4 w-4" />} />
          <Node left="calc(100% - 1rem)" label="上游" sub="commandcode.ai" accent="blue" icon={<Cloud className="h-4 w-4" />} />
        </div>

        <div className="mt-2 space-y-1.5">
          {requests.length === 0 ? (
            <p className="rounded-md border border-dashed border-border px-3 py-4 text-center text-xs text-muted-foreground">
              还没有请求记录——启动代理后，向 /v1/chat/completions 发一次请求试试。
            </p>
          ) : (
            // 轨道下方只展示最近一次请求
            requests.slice(0, 1).map((r) => (
              <button
                key={r.id}
                onClick={() => setDetail(r)}
                className="flex w-full items-center gap-2.5 rounded-md border border-border/70 bg-secondary/30 px-2.5 py-1.5 text-left transition-colors hover:bg-secondary"
              >
                <ModelLogo model={r.model} size={14} className="!p-0.5" />
                <span className="min-w-0 truncate font-mono text-xs text-foreground/90">{r.model}</span>
                <span className="min-w-0 truncate text-xs text-muted-foreground">{r.path}</span>
                <span className="flex-1" />
                <StatusLamp state={lampForStatus(r.status)} pulse={r.status === "streaming"} />
                <span className="whitespace-nowrap text-xs text-muted-foreground">{formatDuration(r.elapsed_ms)}</span>
              </button>
            ))
          )}
        </div>
      </CardContent>

      <Dialog open={detail !== null} onOpenChange={(o) => !o && setDetail(null)}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              {detail && <ModelLogo model={detail.model} size={16} />}
              请求详情
            </DialogTitle>
            <DialogDescription>单次代理请求的中继摘要</DialogDescription>
          </DialogHeader>
          {detail && (
            <div className="select-text space-y-2 font-mono text-xs">
              <Row k="请求 ID" v={detail.id} />
              <Row k="路径" v={detail.path} />
              <Row k="模型" v={detail.model} />
              <Row k="模式" v={detail.stream ? "流式" : "非流式"} />
              <Row k="状态" v={statusLabel(detail.status)} />
              <Row k="耗时" v={formatDuration(detail.elapsed_ms)} />
              <Row k="Token" v={`in ${detail.input_tokens} / out ${detail.output_tokens} / cached ${detail.cached_tokens}`} />
              <Row k="最后事件" v={detail.last_event || "—"} />
            </div>
          )}
          {detail && (
            <div className="flex justify-end">
              <Badge variant={detail.status === "ok" ? "success" : "outline"}>
                {statusLabel(detail.status)}
              </Badge>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </Card>
  );
}

/** 请求详情弹窗中的一行“键 - 值”展示。 */
function Row({ k, v }: { k: string; v: string }) {
  return (
    <div className="flex justify-between gap-4">
      <span className="shrink-0 text-muted-foreground">{k}</span>
      <span className="truncate text-right">{v}</span>
    </div>
  );
}
