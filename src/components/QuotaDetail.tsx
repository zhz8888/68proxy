import { useTranslation } from "react-i18next";

import { translate } from "@/i18n";
import { type AccountQuota, type LimitWindow } from "@/lib/api";
import { formatCost } from "@/lib/format";
import { cn } from "@/lib/utils";

/** 用量百分比 → 进度条/文字配色（<70 绿、<90 黄、其余红）。 */
export function usageColor(pct: number): string {
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
  const { t } = useTranslation();
  const pct = win.cap > 0 ? Math.min((win.used / win.cap) * 100, 100) : 0;
  const resetText =
    win.reset_at != null
      ? (() => {
          const diff = win.reset_at - Date.now();
          if (diff <= 0) return t("quotaDetail.resettingSoon");
          const h = Math.floor(diff / 3_600_000);
          const m = Math.floor((diff % 3_600_000) / 60_000);
          return h > 0
            ? t("quotaDetail.resetInHours", { p0: h, p1: m })
            : t("quotaDetail.resetInMinutes", { p0: m });
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
  const { t } = useTranslation();
  if (quota.error) {
    return (
      <p className="text-xs text-destructive">
        {t("quota.fetchFailedPrefix", {
          p0: translate(`quota.error.${quota.error}`, { defaultValue: quota.error }),
        })}
      </p>
    );
  }
  if (!quota.has_billing) {
    return <p className="text-xs text-muted-foreground">{t("quotaDetail.noBilling")}</p>;
  }
  return (
    <div className="space-y-3">
      {/* 套餐与周期 */}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
        <span className="font-medium text-foreground">{quota.plan_name || t("plan.noSubscription")}</span>
        {quota.status && (
          <span className={cn(quota.status === "active" ? "text-signal-success" : "text-signal-warn")}>
            {quota.status}
          </span>
        )}
        {quota.days_left != null && (
          <span className="text-muted-foreground">
            {quota.days_left === 0
              ? t("quotaDetail.renewsToday")
              : t("quotaDetail.renewsInDays", { p0: quota.days_left })}
          </span>
        )}
      </div>

      {/* 总余量 + 余额视角用量 */}
      <div className="space-y-1">
        <div className="flex items-baseline justify-between text-xs">
          <span className="text-muted-foreground">{t("quotaDetail.remaining")}</span>
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
          <div className="text-muted-foreground">{t("quotaDetail.monthly")}</div>
          <div className="font-mono">{formatCost(quota.monthly_remaining)}</div>
        </div>
        <div className="rounded-md border bg-secondary/20 px-2 py-1.5">
          <div className="text-muted-foreground">{t("quotaDetail.purchased")}</div>
          <div className="font-mono">{formatCost(quota.purchased_remaining)}</div>
        </div>
        <div className="rounded-md border bg-secondary/20 px-2 py-1.5">
          <div className="text-muted-foreground">{t("quotaDetail.free")}</div>
          <div className="font-mono">{formatCost(quota.free_remaining)}</div>
        </div>
      </div>

      {/* 本周期上游实际消耗 */}
      {quota.total_spent > 0 && (
        <div className="flex items-baseline justify-between text-xs">
          <span className="text-muted-foreground">{t("quotaDetail.spentUpstream")}</span>
          <span className="font-mono">{formatCost(quota.total_spent)}</span>
        </div>
      )}

      {/* 5 小时 / 周窗口限额 */}
      {(quota.five_hour || quota.weekly) && (
        <div className="space-y-2 border-t pt-2">
          <div className="text-2xs uppercase tracking-wide text-muted-foreground">
            {t("quotaDetail.windowLimits")}
          </div>
          {quota.five_hour && <LimitWindowRow label={t("quotaDetail.fiveHour")} win={quota.five_hour} />}
          {quota.weekly && <LimitWindowRow label={t("quotaDetail.weekly")} win={quota.weekly} />}
        </div>
      )}

      {/* 组织级消费限额 */}
      {quota.org_limits.length > 0 && (
        <div className="space-y-2 border-t pt-2">
          <div className="text-2xs uppercase tracking-wide text-muted-foreground">
            {t("quotaDetail.orgLimits")}
          </div>
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
