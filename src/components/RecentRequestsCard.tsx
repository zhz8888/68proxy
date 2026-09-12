import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { ModelLogo } from "@/components/ModelLogo";
import { StatusLamp } from "@/components/StatusLamp";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { api, onStats, type UsageRecentRow } from "@/lib/api";
import { formatLogTime, formatTokens } from "@/lib/format";
import { lampForStatus } from "@/lib/status";
import { cn } from "@/lib/utils";

/** 最近请求卡片：展示最近 20 条成功计费请求的 token 用量，随统计事件实时刷新。 */
export function RecentRequestsCard() {
  const { t } = useTranslation();
  const [rows, setRows] = useState<UsageRecentRow[]>([]);

  useEffect(() => {
    let mounted = true;
    /** 拉取一次最近请求；组件已卸载时丢弃结果。 */
    const refresh = () => api.statsRecent(20).then((r) => mounted && setRows(r)).catch(() => {});
    refresh();
    // 订阅后端用量事件（节流合并到 1 秒），并以 3 秒轮询兜底
    let debounce: ReturnType<typeof setTimeout> | null = null;
    const off = onStats(() => {
      if (debounce) return;
      debounce = setTimeout(() => {
        debounce = null;
        refresh();
      }, 1000);
    });
    const timer = setInterval(refresh, 3000);
    return () => {
      mounted = false;
      off.then((f) => f());
      if (debounce) clearTimeout(debounce);
      clearInterval(timer);
    };
  }, []);

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("stats.recentRequests")}</CardTitle>
        <CardDescription>{t("stats.recentRequestsDesc")}</CardDescription>
      </CardHeader>
      <CardContent className="pt-0">
        {rows.length === 0 ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("stats.noRecentRequests")}
          </p>
        ) : (
          <div className="space-y-1">
            {rows.map((r, i) => (
              <div
                key={i}
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
                <span className="w-14 text-right font-mono text-xs text-muted-foreground">
                  {t("stats.tokenIn", { p0: formatTokens(r.prompt_tokens) })}
                </span>
                <span className="w-14 text-right font-mono text-xs text-muted-foreground">
                  {t("stats.tokenOut", { p0: formatTokens(r.completion_tokens) })}
                </span>
                <span
                  className={cn(
                    "w-14 text-right font-mono text-xs",
                    r.cached_tokens > 0 ? "text-signal-info" : "text-muted-foreground",
                  )}
                >
                  {t("stats.tokenCached", { p0: formatTokens(r.cached_tokens) })}
                </span>
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
