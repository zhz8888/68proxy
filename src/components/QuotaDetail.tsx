import { type AccountQuota, type LimitWindow } from "@/lib/api";
import { formatCost } from "@/lib/format";
import { cn } from "@/lib/utils";

/** 用量百分比 → 进度条/文字配色（<70 绿、<90 黄、其余红）。 */
function usageColor(pct: number): string {
  if (pct >= 90) return "text-signal-error";
  if (pct >= 70) return "text-signal-warn";
  return "text-signal-success";
}

/** 进度条填充色（与 usageColor 同档）。 */
function barColor(pct: number): string {
  if (pct >= 90) return "bg-signal-error";
  if (pct >= 70) return "bg-signal-warn";
  return "bg-signal-success";
}

/** 通用进度条：pct 为 0-100。 */
export function MeterBar({ pct, className }: { pct: number; className?: string }) {
  const clamped = Math.max(0, Math.min(100, pct));
  return (
    <div className={cn("h-1.5 w-full overflow-hidden rounded-full bg-muted", className)}>
      <div className={cn("h-full rounded-full transition-all", barColor(clamped))} style={{ width: `${clamped}%` }} />
    </div>
  );
}

/** 把一个限额窗口渲染为「标签 + 进度条 + 已用/上限 + 重置倒计时」一行。 */
export function LimitWindowRow({ label, win }: { label: string; win: LimitWindow }) {
  const pct = win.cap > 0 ? Math.min((win.used / win.cap) * 100, 100) : 0;
  const resetText =
    win.reset_at != null
      ? (() => {
          const diff = win.reset_at - Date.now();
          if (diff <= 0) return "即将重置";
          const h = Math.floor(diff / 3_600_000);
          const m = Math.floor((diff % 3_600_000) / 60_000);
          return h > 0 ? `${h}小时${m}分后重置` : `${m}分钟后重置`;
        })()
      : "";
  return (
    <div className="space-y-1">
      <div className="flex items-baseline justify-between text-xs">
        <span className="text-muted-foreground">{label}</span>
        <span className={cn("font-mono", usageColor(pct))}>
          {pct.toFixed(0)}% {resetText && <span className="text-muted-foreground">· {resetText}</span>}
        </span>
      </div>
      <MeterBar pct={pct} />
    </div>
  );
}

/** 额度明细弹窗/区块共用：套餐、总余量、三类余额、窗口限额与组织限额。 */
export function QuotaDetail({ quota }: { quota: AccountQuota }) {
  if (quota.error) {
    return <p className="text-xs text-destructive">额度获取失败：{quota.error}</p>;
  }
  if (!quota.has_billing) {
    return <p className="text-xs text-muted-foreground">该账户暂无计费数据。</p>;
  }
  return (
    <div className="space-y-3">
      {/* 套餐与周期 */}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
        <span className="font-medium text-foreground">{quota.plan_name}</span>
        {quota.status && (
          <span className={cn(quota.status === "active" ? "text-signal-success" : "text-signal-warn")}>
            {quota.status}
          </span>
        )}
        {quota.days_left != null && (
          <span className="text-muted-foreground">
            {quota.days_left === 0 ? "今日续期" : `${quota.days_left} 天后续期`}
          </span>
        )}
      </div>

      {/* 总余量 + 余额视角用量 */}
      <div className="space-y-1">
        <div className="flex items-baseline justify-between text-xs">
          <span className="text-muted-foreground">剩余额度</span>
          <span className="font-mono">
            {formatCost(quota.total_remaining)}
            {quota.total_pool > 0 && <span className="text-muted-foreground"> / {formatCost(quota.total_pool)}</span>}
          </span>
        </div>
        <MeterBar pct={quota.usage_percent} />
      </div>

      {/* 三类余额明细 */}
      <div className="grid grid-cols-3 gap-2 text-xs">
        <div className="rounded-md border bg-secondary/20 px-2 py-1.5">
          <div className="text-muted-foreground">月额度</div>
          <div className="font-mono">{formatCost(quota.monthly_remaining)}</div>
        </div>
        <div className="rounded-md border bg-secondary/20 px-2 py-1.5">
          <div className="text-muted-foreground">购买额度</div>
          <div className="font-mono">{formatCost(quota.purchased_remaining)}</div>
        </div>
        <div className="rounded-md border bg-secondary/20 px-2 py-1.5">
          <div className="text-muted-foreground">赠送额度</div>
          <div className="font-mono">{formatCost(quota.free_remaining)}</div>
        </div>
      </div>

      {/* 本周期上游实际消耗 */}
      {quota.total_spent > 0 && (
        <div className="flex items-baseline justify-between text-xs">
          <span className="text-muted-foreground">本周期消耗（上游统计）</span>
          <span className="font-mono">{formatCost(quota.total_spent)}</span>
        </div>
      )}

      {/* 5 小时 / 周窗口限额 */}
      {(quota.five_hour || quota.weekly) && (
        <div className="space-y-2 border-t pt-2">
          <div className="text-[10px] uppercase tracking-wide text-muted-foreground/70">窗口限额</div>
          {quota.five_hour && <LimitWindowRow label="5 小时" win={quota.five_hour} />}
          {quota.weekly && <LimitWindowRow label="每周" win={quota.weekly} />}
        </div>
      )}

      {/* 组织级消费限额 */}
      {quota.org_limits.length > 0 && (
        <div className="space-y-2 border-t pt-2">
          <div className="text-[10px] uppercase tracking-wide text-muted-foreground/70">组织限额</div>
          {quota.org_limits.map((o) => (
            <div key={o.label} className="space-y-1">
              <div className="flex items-baseline justify-between text-xs">
                <span className="text-muted-foreground">{o.label}</span>
                <span className={cn("font-mono", o.reached ? "text-signal-error" : usageColor(o.pct))}>
                  {o.pct.toFixed(0)}%
                </span>
              </div>
              <MeterBar pct={o.pct} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
