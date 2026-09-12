import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Activity, AlertTriangle, RefreshCw, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { ModelLogo } from "@/components/ModelLogo";
import { StatusLamp } from "@/components/StatusLamp";
import { UsageMiniBars } from "@/components/UsageMiniBars";
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
import { api, onStats, type AccountQuota, type UsageChartPoint, type UsageGroupRow, type UsagePeriod, type UsageStats } from "@/lib/api";
import { formatCost, formatLogTime, formatTokens } from "@/lib/format";
import { lampForStatus } from "@/lib/status";
import { cn } from "@/lib/utils";
import { LimitWindowRow, MeterBar } from "@/components/QuotaDetail";

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
function GroupRowLine({ row }: { row: UsageGroupRow }) {
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
      <span className="w-16 text-right font-mono text-xs text-muted-foreground">
        {formatCost(row.cost)}
      </span>
    </div>
  );
}

/** 账户额度紧凑行：套餐 + 剩余额度 + 月/购买/赠送 + 5h/周窗口限额。 */
function QuotaRow({ q }: { q: AccountQuota }) {
  return (
    <div className="space-y-2 rounded-md border border-border/70 bg-secondary/20 px-3 py-2">
      <div className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <span className="truncate text-sm font-medium text-foreground">{q.user_name}</span>
          <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
            {q.plan_name}
          </span>
          {q.status && (
            <span className={cn("shrink-0 text-[10px]", q.status === "active" ? "text-signal-success" : "text-signal-warn")}>
              {q.status}
            </span>
          )}
        </div>
        <span className="shrink-0 font-mono text-xs">
          {formatCost(q.total_remaining)}
          {q.total_pool > 0 && <span className="text-muted-foreground"> / {formatCost(q.total_pool)}</span>}
        </span>
      </div>
      {q.error ? (
        <p className="text-xs text-destructive">额度获取失败：{q.error}</p>
      ) : !q.has_billing ? (
        <p className="text-xs text-muted-foreground">暂无计费数据</p>
      ) : (
        <>
          <MeterBar pct={q.usage_percent} />
          <div className="flex items-center gap-3 text-[10px] text-muted-foreground">
            <span>月 <span className="font-mono text-foreground/80">{formatCost(q.monthly_remaining)}</span></span>
            <span>购买 <span className="font-mono text-foreground/80">{formatCost(q.purchased_remaining)}</span></span>
            <span>赠送 <span className="font-mono text-foreground/80">{formatCost(q.free_remaining)}</span></span>
            {q.total_spent > 0 && <span>本期消耗 <span className="font-mono text-foreground/80">{formatCost(q.total_spent)}</span></span>}
          </div>
          {(q.five_hour || q.weekly) && (
            <div className="grid grid-cols-2 gap-3 pt-0.5">
              {q.five_hour && <LimitWindowRow label="5 小时" win={q.five_hour} />}
              {q.weekly && <LimitWindowRow label="每周" win={q.weekly} />}
            </div>
          )}
        </>
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
  const [loadError, setLoadError] = useState("");
  // 账户额度快照（全部 CC 账户）
  const [quotas, setQuotas] = useState<AccountQuota[]>([]);
  const [quotaLoading, setQuotaLoading] = useState(false);
  // 每次请求的序号：仅当结果仍属于最新一次请求时才落库，避免快速切换周期时旧结果覆盖新结果
  const reqSeq = useRef(0);

  /** 拉取当前时间范围的汇总与趋势数据。 */
  const refresh = useCallback(async (p: UsagePeriod) => {
    const seq = ++reqSeq.current;
    try {
      const [s, c] = await Promise.all([api.statsGet(p), api.statsChart(p)]);
      if (seq !== reqSeq.current) return; // 已有更新的请求，丢弃本次结果
      setStats(s);
      setChart(c);
      setLoadError("");
    } catch (e) {
      if (seq !== reqSeq.current) return;
      // 不再静默吞错：保留旧数据的同时给出可见提示，便于区分「无数据」与「查询失败」
      setLoadError(String(e));
    }
  }, []);

  /** 拉取账户额度（失败不阻塞统计页其他数据）。 */
  const refreshQuota = useCallback(async () => {
    setQuotaLoading(true);
    try {
      setQuotas(await api.accountsQuota());
    } catch {
      // 额度是附加信息，拉取失败时清空即可，页面另有统计错误提示位
      setQuotas([]);
    } finally {
      setQuotaLoading(false);
    }
  }, []);

  // 挂载时加载数据，订阅用量更新事件（节流合并），并以 3 秒轮询兜底
  useEffect(() => {
    refresh(period);
    refreshQuota();
    // 后端用量事件最密可达约 5 次/秒，这里合并为最多每 1 秒刷新一次，避免高频重量级查询
    let debounce: ReturnType<typeof setTimeout> | null = null;
    const off = onStats(() => {
      if (debounce) return;
      debounce = setTimeout(() => {
        debounce = null;
        refresh(period);
      }, 1000);
    });
    const timer = setInterval(() => refresh(period), 3000);
    return () => {
      off.then((f) => f());
      if (debounce) clearTimeout(debounce);
      clearInterval(timer);
    };
  }, [period, refresh, refreshQuota]);

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
        {loadError && (
          <div className="flex items-center gap-1.5 rounded-lg bg-destructive/15 px-3 py-1.5 text-xs text-destructive">
            <AlertTriangle className="h-3.5 w-3.5" />
            统计加载失败：{loadError}
          </div>
        )}
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
          {/* 账户剩余额度：显示全部 CC 账户的套餐余额与窗口限额 */}
          {quotas.length > 0 && (
            <Card>
              <CardHeader className="pb-2">
                <div className="flex items-center justify-between">
                  <CardTitle className="text-sm">账户剩余额度</CardTitle>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-6 px-2 text-xs"
                    disabled={quotaLoading}
                    onClick={refreshQuota}
                  >
                    <RefreshCw className={quotaLoading ? "animate-spin" : ""} />
                    刷新
                  </Button>
                </div>
                <CardDescription>各 CC 上游账户的套餐余额与 5 小时 / 周窗口限额（额度实时来自上游）</CardDescription>
              </CardHeader>
              <CardContent className="space-y-2 pt-0">
                {quotas.map((q, i) => (
                  <QuotaRow key={`${q.masked_key}-${i}`} q={q} />
                ))}
              </CardContent>
            </Card>
          )}

          {/* 汇总卡片行 */}
          <div className="grid grid-cols-5 gap-3">
            <SummaryCard title="总请求数" value={formatTokens(stats?.total_requests ?? 0)} />
            <SummaryCard title="输入 Tokens" value={formatTokens(stats?.total_prompt_tokens ?? 0)} />
            <SummaryCard title="缓存 Tokens" value={formatTokens(stats?.total_cached_tokens ?? 0)} />
            <SummaryCard title="输出 Tokens" value={formatTokens(stats?.total_completion_tokens ?? 0)} />
            <SummaryCard title="估算成本" value={formatCost(stats?.total_cost ?? 0)} hint="Estimated, not actual billing" />
          </div>

          {/* 最近 10 分钟迷你柱状图卡片 */}
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm">最近 10 分钟</CardTitle>
              <CardDescription>按分钟聚合的 token 用量（输入 + 输出）</CardDescription>
            </CardHeader>
            <CardContent className="pt-0">
              <UsageMiniBars buckets={stats?.last_10_minutes ?? []} />
            </CardContent>
          </Card>

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
              <CardDescription>请求数 / 输入 / 输出 / 总 Tokens / 成本</CardDescription>
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
                    <span className="w-16 text-right">成本</span>
                  </div>
                  {groupRows.map((r) => (
                    <GroupRowLine key={r.key} row={r} />
                  ))}
                </div>
              )}
            </CardContent>
          </Card>

          {/* 最近请求卡片 */}
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm">最近请求</CardTitle>
              <CardDescription>全时段最近 20 条成功计费请求的 token 用量（不随上方时间范围变化）</CardDescription>
            </CardHeader>
            <CardContent className="pt-0">
              {!stats || stats.recent_requests.length === 0 ? (
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
