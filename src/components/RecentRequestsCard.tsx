import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { ModelLogo } from "@/components/ModelLogo";
import { StatusLamp } from "@/components/StatusLamp";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { api, onStats, type UsageRecentRow } from "@/lib/api";
import { formatLogTime, formatTokens } from "@/lib/format";
import { lampForStatus } from "@/lib/status";
import { useVisiblePolling } from "@/lib/useVisiblePolling";
import { cn } from "@/lib/utils";

/** 最近请求卡片：展示最近成功计费请求的 token 用量，随统计事件实时刷新。 */
export function RecentRequestsCard({ limit = 20 }: { limit?: number }) {
  const { t } = useTranslation();
  /** 最近请求明细行（新在前）。 */
  const [rows, setRows] = useState<UsageRecentRow[]>([]);
  const mounted = useRef(true);

  /** 拉取一次最近请求；组件已卸载时丢弃结果。 */
  const refresh = useCallback(() => {
    api
      .statsRecent(limit)
      .then((r) => mounted.current && setRows(r))
      .catch(() => {});
  }, [limit]);

  useEffect(() => {
    mounted.current = true;
    refresh();
    // 订阅后端用量事件（节流合并到 1 秒）；兜底轮询见下方 useVisiblePolling
    let debounce: ReturnType<typeof setTimeout> | null = null;
    const off = onStats(() => {
      if (debounce) return;
      debounce = setTimeout(() => {
        debounce = null;
        refresh();
      }, 1000);
    });
    return () => {
      mounted.current = false;
      off.then((f) => f());
      if (debounce) clearTimeout(debounce);
    };
  }, [refresh]);

  // 兜底轮询：窗口隐藏时暂停；首刷由上方 useEffect 完成，此处不再立即执行
  useVisiblePolling(refresh, 3000, false);

  /** 双行 token 单元格：上行文字描述（输入/输出/缓存），下行数值。 */
  function TokenCell({ label, value, highlight }: { label: string; value: string; highlight?: boolean }) {
    return (
      <span className="flex w-16 flex-col items-end gap-0.5">
        <span className="text-2xs leading-none text-muted-foreground/80">{label}</span>
        <span
          className={cn(
            "font-mono text-xs leading-none",
            highlight ? "text-signal-info" : "text-muted-foreground",
          )}
        >
          {value}
        </span>
      </span>
    );
  }

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("stats.recentRequests")}</CardTitle>
        <CardDescription>{t("stats.recentRequestsDesc", { p0: limit })}</CardDescription>
      </CardHeader>
      <CardContent className="pt-0">
        {rows.length === 0 ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("stats.noRecentRequests")}
          </p>
        ) : (
          <div className="space-y-1">
            {rows.map((r) => (
              <div
                key={r.id}
                className="flex items-center gap-3 rounded-md border border-border/70 bg-secondary/30 px-3 py-2"
              >
                <ModelLogo model={r.model} size={16} className="!p-0.5" />
                <span className="w-20 shrink-0 select-text font-mono text-xs text-muted-foreground">
                  {formatLogTime(r.ts)}
                </span>
                <span className="min-w-0 flex-1 truncate select-text font-mono text-sm text-foreground/90">
                  {r.model}
                </span>
                <span className="min-w-0 truncate text-xs text-muted-foreground">{r.endpoint}</span>
                <StatusLamp state={lampForStatus(r.status)} />
                <TokenCell label={t("stats.input")} value={formatTokens(r.prompt_tokens)} />
                <TokenCell label={t("stats.output")} value={formatTokens(r.completion_tokens)} />
                <TokenCell
                  label={t("stats.cached")}
                  value={formatTokens(r.cached_tokens)}
                  highlight={r.cached_tokens > 0}
                />
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
