import { useCallback, useEffect, useMemo, useState } from "react";
import { Activity, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { ModelLogo } from "@/components/ModelLogo";
import { StatusLamp } from "@/components/StatusLamp";
import { UsageTrendChart } from "@/components/UsageTrendChart";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { api, onStats, type UsageChartPoint, type UsageGroupRow, type UsagePeriod, type UsageStats } from "@/lib/api";
import { formatCost, formatLogTime, formatTokens } from "@/lib/format";
import { lampForStatus } from "@/lib/status";
import { cn } from "@/lib/utils";

/** 时间范围选项：值与中文标签。 */
const PERIODS: Array<{ value: UsagePeriod; label: string }> = [
  { value: "today", label: "今日" },
  { value: "24h", label: "24 小时" },
  { value: "7d", label: "7 天" },
  { value: "30d", label: "30 天" },
  { value: "60d", label: "60 天" },
  { value: "all", label: "全部" },
];

/** 汇总卡片：小字标题 + 大字数值（font-mono）。 */
function SummaryCard({ title, value, hint }: { title: string; value: string; hint?: string }) {
  return (
    <Card className="min-w-0">
      <CardHeader className="pb-2">
        <CardTitle className="text-xs font-medium text-muted-foreground">{title}</CardTitle>
      </CardHeader>
      <CardContent className="pt-0">
        <span className="block truncate font-mono text-lg font-semibold">{value}</span>
        {hint && <span className="text-[10px] text-muted-foreground/70">{hint}</span>}
      </CardContent>
    </Card>
  );
}

/** 分组表格行：key + 请求数 + 各 token 列 + 成本。 */
function GroupRowLine({ row, showCost }: { row: UsageGroupRow; showCost: boolean }) {
  return (
    <div className="flex items-center gap-3 rounded-md border border-border/70 bg-secondary/30 px-3 py-2">
      <ModelLogo model={row.key} size={16} className="!p-0.5" />
      <span className="min-w-0 flex-1 truncate select-text font-mono text-sm text-foreground/90">
        {row.key}
      </span>
      <span className="w-14 text-right font-mono text-xs text-muted-foreground">{row.requests}</span>
      <span className="w-20 text-right font-mono text-xs text-muted-foreground">
        {formatTokens(row.prompt_tokens)}
      </span>
      <span className="w-20 text-right font-mono text-xs text-muted-foreground">
        {formatTokens(row.completion_tokens)}
      </span>
      <span className="w-16 text-right font-mono text-xs text-muted-foreground">
        {formatTokens(row.total_tokens)}
      </span>
      {showCost && (
        <span className="w-16 text-right font-mono text-xs text-muted-foreground">
          {formatCost(row.cost)}
        </span>
      )}
    </div>
  );
}

/** 用量统计视图：汇总卡片、趋势图、按模型/端点分组表与最近请求明细。 */
export function StatsView() {
  const [period, setPeriod] = useState<UsagePeriod>("7d");
  const [stats, setStats] = useState<UsageStats | null>(null);
  const [chart, setChart] = useState<UsageChartPoint[]>([]);
  const [chartMode, setChartMode] = useState<"tokens" | "cost">("tokens");
  const [groupBy, setGroupBy] = useState<"model" | "endpoint">("model");
  const [confirmClear, setConfirmClear] = useState(false);

  /** 拉取当前时间范围的汇总与趋势数据。 */
  const refresh = useCallback(async (p: UsagePeriod) => {
    try {
      const [s, c] = await Promise.all([api.statsGet(p), api.statsChart(p)]);
      setStats(s);
      setChart(c);
    } catch {
      /* 忽略刷新错误，保留旧数据 */
    }
  }, []);

  // 挂载时加载数据，订阅用量更新事件，并以 3 秒轮询兜底
  useEffect(() => {
    refresh(period);
    const off = onStats(() => refresh(period));
    const timer = setInterval(() => refresh(period), 3000);
    return () => {
      off.then((f) => f());
      clearInterval(timer);
    };
  }, [period, refresh]);

  /** 清空统计：确认后调用后端并刷新。 */
  async function clearAll() {
    setConfirmClear(false);
    try {
      const res = await api.statsClearAll();
      toast.success(`用量统计已清空（${res.cleared} 条）`);
      refresh(period);
    } catch (e) {
      toast.error(String(e));
    }
  }

  // 分组表数据：按模型或端点
  const groupRows = useMemo(() => {
    if (!stats) return [];
    return groupBy === "model" ? stats.by_model : stats.by_endpoint;
  }, [stats, groupBy]);

  const hasData = (stats?.total_requests ?? 0) > 0;

  return (
    <div className="flex h-full flex-col gap-3">
      {/* 顶部工具条：时间范围切换 + 分组视图切换 + 清空 */}
      <div className="flex items-center gap-2">
        <Select value={period} onValueChange={(v) => setPeriod(v as UsagePeriod)}>
          <SelectTrigger className="h-8 w-32">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {PERIODS.map((p) => (
              <SelectItem key={p.value} value={p.value}>
                {p.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <div className="flex items-center gap-2 rounded-lg bg-muted px-3 py-1.5 text-xs text-muted-foreground">
          <Activity className="h-3.5 w-3.5" />
          共 {stats?.total_requests ?? 0} 次请求 · 估算成本仅供参考
        </div>
        <div className="flex-1" />
        <Button
          variant="secondary"
          size="sm"
          className="h-8 px-2.5"
          onClick={() => setConfirmClear(true)}
          disabled={!hasData}
        >
          <Trash2 className="h-3.5 w-3.5" />
          清空统计
        </Button>
      </div>

      <ScrollArea className="flex-1 rounded-lg border border-border bg-card/60">
        <div className="space-y-4 p-4">
          {/* 汇总卡片行 */}
          <div className="grid grid-cols-5 gap-3">
            <SummaryCard title="总请求数" value={formatTokens(stats?.total_requests ?? 0)} />
            <SummaryCard title="输入 Tokens" value={formatTokens(stats?.total_prompt_tokens ?? 0)} />
            <SummaryCard title="缓存 Tokens" value={formatTokens(stats?.total_cached_tokens ?? 0)} />
            <SummaryCard title="输出 Tokens" value={formatTokens(stats?.total_completion_tokens ?? 0)} />
            <SummaryCard title="估算成本" value={formatCost(stats?.total_cost ?? 0)} hint="Estimated, not actual billing" />
          </div>

          {/* 趋势图卡片 */}
          <Card>
            <CardHeader className="pb-2">
              <div className="flex items-center justify-between">
                <CardTitle className="text-sm">用量趋势</CardTitle>
                <Tabs value={chartMode} onValueChange={(v) => setChartMode(v as "tokens" | "cost")}>
                  <TabsList className="h-7">
                    <TabsTrigger value="tokens" className="text-xs">Tokens</TabsTrigger>
                    <TabsTrigger value="cost" className="text-xs">成本</TabsTrigger>
                  </TabsList>
                </Tabs>
              </div>
              <CardDescription>近 {PERIODS.find((p) => p.value === period)?.label ?? ""} · 按小时/天聚合</CardDescription>
            </CardHeader>
            <CardContent className="pt-0">
              <UsageTrendChart points={chart} mode={chartMode} />
            </CardContent>
          </Card>

          {/* 分组表格卡片 */}
          <Card>
            <CardHeader className="pb-2">
              <div className="flex items-center justify-between">
                <CardTitle className="text-sm">用量明细</CardTitle>
                <Select value={groupBy} onValueChange={(v) => setGroupBy(v as "model" | "endpoint")}>
                  <SelectTrigger className="h-8 w-32">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="model">按模型</SelectItem>
                    <SelectItem value="endpoint">按端点</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <CardDescription>请求数 / 输入 / 输出 / 总 Tokens{chartMode === "cost" ? " / 成本" : ""}</CardDescription>
            </CardHeader>
            <CardContent className="pt-0">
              {groupRows.length === 0 ? (
                <p className="py-8 text-center text-sm text-muted-foreground">
                  当前时间范围内暂无用量数据。启动代理发起请求后，这里会按模型/端点汇总。
                </p>
              ) : (
                <div className="space-y-1">
                  {/* 表头 */}
                  <div className="flex items-center gap-3 px-3 pb-1 text-[10px] uppercase tracking-wide text-muted-foreground/70">
                    <span className="w-4 shrink-0" />
                    <span className="min-w-0 flex-1 truncate">{groupBy === "model" ? "模型" : "端点"}</span>
                    <span className="w-14 text-right">请求</span>
                    <span className="w-20 text-right">输入</span>
                    <span className="w-20 text-right">输出</span>
                    <span className="w-16 text-right">总 Tokens</span>
                    {chartMode === "cost" && <span className="w-16 text-right">成本</span>}
                  </div>
                  {groupRows.map((r) => (
                    <GroupRowLine key={r.key} row={r} showCost={chartMode === "cost"} />
                  ))}
                </div>
              )}
            </CardContent>
          </Card>

          {/* 最近请求卡片 */}
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm">最近请求</CardTitle>
              <CardDescription>最近 20 条成功计费请求的 token 用量</CardDescription>
            </CardHeader>
            <CardContent className="pt-0">
              {!hasData || !stats || stats.recent_requests.length === 0 ? (
                <p className="py-8 text-center text-sm text-muted-foreground">
                  暂无最近请求记录。请求完成且产出 token 后显示在此。
                </p>
              ) : (
                <div className="space-y-1">
                  {stats.recent_requests.map((r, i) => (
                    <div
                      key={i}
                      className="flex items-center gap-3 rounded-md border border-border/70 bg-secondary/30 px-3 py-2"
                    >
                      <ModelLogo model={r.model} size={16} className="!p-0.5" />
                      <span className="w-20 shrink-0 select-text font-mono text-xs text-muted-foreground/80">
                        {formatLogTime(r.ts)}
                      </span>
                      <span className="min-w-0 flex-1 truncate select-text font-mono text-sm text-foreground/90">
                        {r.model}
                      </span>
                      <span className="min-w-0 truncate text-xs text-muted-foreground">{r.endpoint}</span>
                      <StatusLamp state={lampForStatus(r.status)} />
                      <span className="w-14 text-right font-mono text-xs text-muted-foreground">
                        in {formatTokens(r.prompt_tokens)}
                      </span>
                      <span className="w-14 text-right font-mono text-xs text-muted-foreground">
                        out {formatTokens(r.completion_tokens)}
                      </span>
                      <span
                        className={cn(
                          "w-14 text-right font-mono text-xs",
                          r.cached_tokens > 0 ? "text-signal-info" : "text-muted-foreground",
                        )}
                      >
                        cached {formatTokens(r.cached_tokens)}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </CardContent>
          </Card>
        </div>
      </ScrollArea>

      {/* 清空确认对话框 */}
      <Dialog open={confirmClear} onOpenChange={setConfirmClear}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>清空用量统计</DialogTitle>
            <DialogDescription>
              将删除全部历史用量记录与趋势数据，且不可恢复。确认继续？
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="secondary" onClick={() => setConfirmClear(false)}>
              取消
            </Button>
            <Button variant="destructive" onClick={clearAll}>
              确认清空
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
