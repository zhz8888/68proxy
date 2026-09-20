import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy, Eye, RefreshCw, Search, Sparkles } from "lucide-react";

import { ModelLogo, providerForModel } from "@/components/ModelLogo";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { api, type ModelAccessInfo, type ModelInfo, type ModelPricing, type ModelRates, type ModelTier, type PlanContext } from "@/lib/api";
import { copyText, formatContextTokens, formatPeakWindows, formatPrice } from "@/lib/format";
import { errText } from "@/lib/messages";
import { cn } from "@/lib/utils";

/** 能力/价格筛选维度。 */
type Filter = "all" | "vision" | "reasoning" | "free" | "unavailable";

/** 厂商筛选哨兵值：表示「全部厂商」。 */
const ALL_PROVIDERS = "__all__";

/** 模型归属厂商显示名：与卡片 Badge 同一口径（上游 provider 优先，缺省按 ID 前缀推断）。 */
function modelProvider(model: ModelInfo): string {
  return model.provider ?? providerForModel(model.id) ?? "";
}

/** 四项费率完全一致时视为同价档位。 */
function ratesEqual(a: ModelRates, b: ModelRates): boolean {
  return a.input === b.input && a.output === b.output && a.cacheRead === b.cacheRead && a.cacheWrite === b.cacheWrite;
}

/** 相邻档位费率相同时合并为一档（如 minimax-m3 两档同价），避免重复的价格行。 */
function mergeEqualTiers(tiers: ModelTier[]): ModelTier[] {
  const out: ModelTier[] = [];
  for (const tier of tiers) {
    const prev = out[out.length - 1];
    if (prev && ratesEqual(prev.rates, tier.rates)) {
      // 合并后的档位上界取并集（即靠后档位的 maxContext）
      out[out.length - 1] = { maxContext: tier.maxContext, rates: prev.rates };
    } else {
      out.push(tier);
    }
  }
  return out;
}

/** 档位区间标签：首档 ≤X、中档 X–Y、末档 >X；唯一档位不显示标签。 */
function tierLabel(low: number, high: number | null): string {
  if (high === null) return low > 0 ? `>${formatContextTokens(low)}` : "";
  if (low === 0) return `≤${formatContextTokens(high)}`;
  return `${formatContextTokens(low)}–${formatContextTokens(high)}`;
}

/** 卡片上展示的模型 ID：厂商已知（有厂商标识）时省略 ID 的厂商前缀段，归属由厂商标表达；完整 ID 仍通过复制与悬停提示获取。 */
function displayId(id: string, provider?: string | null): string {
  if (!provider) return id;
  const slash = id.indexOf("/");
  return slash > 0 && slash < id.length - 1 ? id.slice(slash + 1) : id;
}

/** 卡片价格区：单档一行；闲/忙时各一行；分档模型每档一行（同价档位已合并）。 */
function PriceLines({ pricing, free }: { pricing?: ModelPricing; free: boolean }) {
  const { t } = useTranslation();
  if (!pricing || pricing.tiers.length === 0) {
    return <span>{t("models.priceUnknown")}</span>;
  }
  if (free) {
    return <span className="text-signal-success">{t("models.freeLimited")}</span>;
  }
  const perM = (r: ModelRates) =>
    t("models.pricePerM", { p0: formatPrice(r.input), p1: formatPrice(r.output) });

  // 闲/忙时模型（DeepSeek 系列）均为单档：档位价即闲时价，忙时价单独给出
  if (pricing.timeOfDay) {
    const off = pricing.tiers[0].rates;
    const peak = pricing.timeOfDay.peak;
    return (
      <div
        className="flex flex-col gap-0.5"
        title={t("models.timeOfDayWindows", { p0: formatPeakWindows(pricing.timeOfDay) })}
      >
        <span className="font-mono">
          {t("models.priceOffPeak", { p0: formatPrice(off.input), p1: formatPrice(off.output) })}
        </span>
        <span className="font-mono text-signal-info">
          {t("models.pricePeak", { p0: formatPrice(peak.input), p1: formatPrice(peak.output) })}
        </span>
      </div>
    );
  }

  const tiers = mergeEqualTiers(pricing.tiers);
  if (tiers.length > 1) {
    return (
      <div className="flex flex-col gap-0.5" title={t("models.tieredTitle")}>
        {tiers.map((tier, i) => {
          const low = i === 0 ? 0 : tiers[i - 1].maxContext ?? 0;
          const label = tierLabel(low, tier.maxContext);
          return (
            <span key={i} className="font-mono">
              {label
                ? t("models.priceTier", {
                    p0: label,
                    p1: formatPrice(tier.rates.input),
                    p2: formatPrice(tier.rates.output),
                  })
                : perM(tier.rates)}
            </span>
          );
        })}
      </div>
    );
  }
  return <span className="font-mono">{perM(tiers[0].rates)}</span>;
}

/** 去连字符与点的紧凑形式，用于兜住 `Qwen/Qwen3.7-Max` ↔ `qwen-3.7-max` 这类命名差异。 */
function compactId(id: string): string {
  return id.replace(/[-.]/g, "");
}

/** 剥离尾部 8 位日期后缀（`-20251001` / `@20251001` / 紧凑残留 `20251001`），与后端同口径。 */
function stripDateSuffix(s: string): string {
  if (s.length > 9) {
    const sep = s[s.length - 9];
    if ((sep === "-" || sep === "@") && /^\d{8}$/.test(s.slice(-8))) {
      return s.slice(0, -9);
    }
  }
  if (s.length > 12 && /^\d{8}$/.test(s.slice(-8))) {
    const prefix = s.slice(0, -8);
    if (prefix.length >= 4 && /[^0-9]/.test(prefix)) return prefix;
  }
  return s;
}

/**
 * 上游注册表 ID（紧凑形式）→ 定价表 ID：两处数据源的个例别名。
 * 定价页会省略 `Preview` 这类营销后缀，紧凑比对要求逐字符相等，覆盖不到。
 */
const PRICING_ALIASES: Record<string, string> = {
  qwen36maxpreview: "qwen-3.6-max",
};

/**
 * 把上游模型 ID 解析为计费表/准入表的键，口径与后端 `pricing::find_pricing` 一致：
 * 精确 → 去 provider 前缀的短名（含日期剥离）→ 紧凑形式 → 显式别名 → 最长前缀
 * （原始与紧凑双形态）。
 *
 * 命名风格在两处数据源间并不统一：上游注册表写 `Qwen/Qwen3.7-Max`（驼峰无连字符），
 * 定价页写 `qwen-3.7-max`（全小写带连字符），只去前缀无法互相命中。
 * @param keys 候选键（计费表已小写；准入表键同样小写，与后端注册表同源）
 * @param id 上游返回的模型 ID（可能带前缀/日期后缀、大小写不一）
 */
function resolveCatalogKey(keys: Iterable<string>, id: string): string | undefined {
  const keySet = keys instanceof Set ? keys : new Set(keys);
  const target = id.toLowerCase();
  if (keySet.has(target)) return target;
  const short = stripDateSuffix(target.split("/").pop() ?? target);
  if (keySet.has(short)) return short;
  // 候选键同样小写后再紧凑：准入表键若含大写也不 miss（与后端两侧 lower 一致）
  const compact = stripDateSuffix(compactId(short));
  let compactBest: string | undefined;
  for (const key of keySet) {
    if (compactId(key.toLowerCase()) === compact) {
      if (!compactBest || key.length > compactBest.length) compactBest = key;
    }
  }
  if (compactBest) return compactBest;
  const aliasId = PRICING_ALIASES[compact];
  if (aliasId && keySet.has(aliasId)) return aliasId;
  // 最长前缀匹配，避免短前缀（如 gpt-5）抢走更具体的档位；
  // 原始与紧凑双形态同时比（紧凑命中加权，优先于更短的原始命中）
  let best: string | undefined;
  let bestScore = -1;
  for (const key of keySet) {
    const lower = key.toLowerCase();
    if (short.startsWith(lower)) {
      const score = lower.length;
      if (score > bestScore) {
        best = key;
        bestScore = score;
      }
    }
    if (compact.startsWith(compactId(lower))) {
      const score = lower.length + 10000;
      if (score > bestScore) {
        best = key;
        bestScore = score;
      }
    }
  }
  return best;
}

/**
 * 按与后端一致的口径匹配模型计费信息。
 * @param catalog 内置计费表（后端下发，id 为官方小写形式）
 * @param id 上游返回的模型 ID
 */
function matchPricing(catalog: Map<string, ModelPricing>, id: string): ModelPricing | undefined {
  const key = resolveCatalogKey(catalog.keys(), id);
  return key === undefined ? undefined : catalog.get(key);
}

/**
 * 按套餐准入结果匹配模型：键与计费表同源，故复用同一套解析口径。
 * @param access 后端下发的「模型 ID → 准入结果」映射
 * @param id 列表中的模型 ID
 */
function matchAccess(access: Record<string, ModelAccessInfo>, id: string): ModelAccessInfo | undefined {
  const key = resolveCatalogKey(Object.keys(access), id);
  return key === undefined ? undefined : access[key];
}

/** 模型视图：展示可用模型列表及其能力与价格，支持搜索、能力筛选、复制与手动刷新。 */
export function ModelsView() {
  const { t } = useTranslation();
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [catalog, setCatalog] = useState<Map<string, ModelPricing>>(new Map());
  const [fallback, setFallback] = useState(false);
  const [plan, setPlan] = useState<PlanContext | null>(null);
  const [access, setAccess] = useState<Record<string, ModelAccessInfo>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [providerFilter, setProviderFilter] = useState(ALL_PROVIDERS);
  const [copiedId, setCopiedId] = useState("");

  /** 拉取模型列表、内置计费表与套餐准入结果。
   * @param force 为 true 时跳过缓存，强制向上游刷新
   */
  async function load(force: boolean) {
    setLoading(true);
    setError("");
    try {
      const [res, cat, ps] = await Promise.all([
        api.modelsGet(force),
        api.modelsCatalog(),
        api.planStatus(force).catch(() => null),
      ]);
      setModels(res.data);
      setFallback(res.fallback);
      setCatalog(new Map(cat.map((c) => [c.id.toLowerCase(), c])));
      // 套餐信息不可用时静默降级：不标注可用性，不影响模型列表展示
      setPlan(ps?.plan ?? null);
      setAccess(ps?.access ?? {});
    } catch (e) {
      // 保存原始错误串，渲染时再翻译（切换语言后已显示的提示随之更新）
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  // 首次进入优先用缓存，不强制刷新上游
  useEffect(() => {
    load(false);
  }, []);

  // 每行模型都带上匹配到的计费信息与套餐准入结果，供筛选与展示复用
  const rows = useMemo(
    () =>
      models.map((m) => ({
        model: m,
        pricing: matchPricing(catalog, m.id),
        access: matchAccess(access, m.id),
      })),
    [models, catalog, access],
  );

  const counts = useMemo(
    () => ({
      vision: rows.filter((r) => r.model.caps?.vision).length,
      reasoning: rows.filter((r) => r.model.caps?.reasoning).length,
      free: rows.filter((r) => r.pricing?.deal?.free).length,
      unavailable: rows.filter((r) => r.access?.allowed === false).length,
    }),
    [rows],
  );

  // 厂商列表：按「模型卡片展示口径」聚合（上游 provider 显示名，缺省按 ID 前缀推断），
  // 带各自模型数量，按数量降序（数量相同时按名称排序），保证下拉框顺序稳定
  const providerOptions = useMemo(() => {
    const map = new Map<string, number>();
    for (const { model } of rows) {
      const p = modelProvider(model);
      if (!p) continue;
      map.set(p, (map.get(p) ?? 0) + 1);
    }
    return [...map.entries()]
      .map(([name, count]) => ({ name, count }))
      .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
  }, [rows]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    return rows.filter(({ model, pricing, access: acc }) => {
      if (q && !model.id.toLowerCase().includes(q) && !model.name.toLowerCase().includes(q)) {
        return false;
      }
      if (providerFilter !== ALL_PROVIDERS && modelProvider(model) !== providerFilter) {
        return false;
      }
      switch (filter) {
        case "vision":
          return !!model.caps?.vision;
        case "reasoning":
          return !!model.caps?.reasoning;
        case "free":
          return !!pricing?.deal?.free;
        case "unavailable":
          return acc?.allowed === false;
        default:
          return true;
      }
    });
  }, [rows, search, filter, providerFilter]);

  /** 复制模型 ID 到剪贴板，并在对应按钮上短暂显示“已复制”图标。 */
  async function copy(id: string) {
    if (await copyText(id)) {
      setCopiedId(id);
      // 带身份判断：连续复制 A→B 时，A 的旧定时器不应提前清掉 B 的提示
      setTimeout(() => setCopiedId((cur) => (cur === id ? "" : cur)), 1400);
    }
  }

  const FILTERS: Array<{ value: Filter; label: string; count?: number }> = [
    { value: "all", label: t("models.filter.all") },
    { value: "vision", label: t("models.filter.vision"), count: counts.vision },
    { value: "reasoning", label: t("models.filter.reasoning"), count: counts.reasoning },
    { value: "free", label: t("models.filter.free"), count: counts.free },
    { value: "unavailable", label: t("models.filter.unavailable"), count: counts.unavailable },
  ];

  /** 能力下拉框当前选中的选项文案（含数量，无计数项不显示）。 */
  const activeFilterLabel =
    FILTERS.find((f) => f.value === filter)?.label ?? t("models.filter.all");

  return (
    <div className="flex h-full flex-col gap-3">
      <div className="flex items-center gap-2">
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder={t("models.searchPlaceholder")}
            className="h-8 w-64 pl-8 text-xs"
          />
        </div>
        {/* 能力 / 厂商筛选下拉框 */}
        <Select value={filter} onValueChange={(v) => setFilter(v as Filter)}>
          <SelectTrigger className="h-8 w-36 text-xs" title={t("models.filter.capabilityLabel")}>
            <SelectValue>{activeFilterLabel}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            {FILTERS.map((f) => (
              <SelectItem key={f.value} value={f.value}>
                {f.label}
                {f.count !== undefined && f.count > 0 && (
                  <span className="ml-2 text-muted-foreground">{f.count}</span>
                )}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={providerFilter} onValueChange={setProviderFilter}>
          <SelectTrigger className="h-8 w-40 text-xs" title={t("models.filter.providerLabel")}>
            <SelectValue>
              {providerFilter === ALL_PROVIDERS
                ? t("models.filter.providerAll")
                : providerFilter}
            </SelectValue>
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL_PROVIDERS}>{t("models.filter.providerAll")}</SelectItem>
            {providerOptions.map((p) => (
              <SelectItem key={p.name} value={p.name}>
                {p.name}
                <span className="ml-2 text-muted-foreground">{p.count}</span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <span className="text-xs text-muted-foreground">
          {models.length > 0 ? t("models.totalModels", { p0: models.length }) : ""}
        </span>
        <Button
          variant="ghost"
          size="sm"
          className="ml-auto"
          disabled={loading}
          onClick={() => load(true)}
        >
          <RefreshCw className={loading ? "animate-spin" : ""} />
          {t("common.refresh")}
        </Button>
      </div>

      {error && (
        <div className="rounded-md border border-destructive/40 bg-destructive/5 px-4 py-3 text-sm text-destructive">
          {errText(error)}
          <button className="ml-2 underline" onClick={() => load(true)}>
            {t("common.retry")}
          </button>
        </div>
      )}

      {fallback && !error && !loading && models.length > 0 && (
        <div className="rounded-md border border-signal-warn/40 bg-signal-warn/5 px-3 py-2 text-xs text-signal-warn">
          {t("models.fallbackNotice")}
        </div>
      )}

      {plan && !plan.fetch_failed && (
        <div className="rounded-md border border-border bg-secondary/40 px-3 py-2 text-xs text-muted-foreground">
          {t("models.currentPlan")}
          <span className="text-foreground">{plan.plan_name || t("plan.noSubscription")}</span>
          {(plan.purchased_credits > 0 || plan.free_credits > 0) && (
            <>
              {" "}
              {t("models.creditsUnlock", { p0: plan.purchased_credits + plan.free_credits })}
            </>
          )}
          {counts.unavailable > 0 && (
            <> {t("models.unavailableCount", { p0: counts.unavailable })}</>
          )}
        </div>
      )}

      {loading ? (
        <div className="grid grid-cols-3 gap-3">
          {Array.from({ length: 9 }).map((_, i) => (
            <Card key={i} className="h-24 animate-pulse bg-secondary/40" />
          ))}
        </div>
      ) : filtered.length === 0 ? (
        <div className="flex flex-1 items-center justify-center rounded-lg border border-dashed border-border">
          <p className="text-sm text-muted-foreground">
            {search || filter !== "all" || providerFilter !== ALL_PROVIDERS
              ? t("models.noMatch")
              : t("models.emptyHint")}
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-3 gap-3 overflow-y-auto pb-4 pr-1">
          {filtered.map(({ model: m, pricing, access: acc }) => {
            const provider = m.provider ?? providerForModel(m.id);
            const free = !!pricing?.deal?.free;
            const discount = pricing?.deal && !free ? pricing.deal.discountPercent : 0;
            const unavailable = acc?.allowed === false;
            const accessHint = unavailable
              ? acc?.minimum_plan
                ? t("plan.requires", { p0: acc.minimum_plan })
                : t("models.unavailableShort")
              : undefined;
            return (
              <Card
                key={m.id}
                className={cn(
                  "group flex flex-col gap-2 p-3 transition-colors",
                  unavailable ? "opacity-60 hover:border-border" : "hover:border-primary/40",
                )}
                title={accessHint}
              >
                <div className="flex items-center gap-3">
                  <ModelLogo model={m.id} size={22} />
                  <div className="min-w-0 flex-1">
                    <p className="select-text truncate font-mono text-xs" title={m.id}>
                      {displayId(m.id, m.provider ?? providerForModel(m.id))}
                    </p>
                    <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                      {provider && (
                        <Badge variant="secondary" className="shrink-0 px-1.5 py-0 text-2xs">
                          {provider}
                        </Badge>
                      )}
                      <span className="truncate">{m.name}</span>
                    </div>
                  </div>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7 opacity-0 transition-opacity group-hover:opacity-100"
                    onClick={() => copy(m.id)}
                    title={t("models.copyId")}
                  >
                    {copiedId === m.id ? <Check className="text-success" /> : <Copy />}
                  </Button>
                </div>

                {/* 能力与促销标记 */}
                <div className="flex flex-wrap items-center gap-1">
                  {unavailable && (
                    <Badge
                      variant="outline"
                      className="shrink-0 border-destructive/40 px-1.5 py-0 text-2xs text-destructive"
                    >
                      {accessHint}
                    </Badge>
                  )}
                  {free && (
                    <Badge className="shrink-0 bg-signal-success/15 px-1.5 py-0 text-2xs text-signal-success">
                      {t("models.free")}
                    </Badge>
                  )}
                  {discount > 0 && (
                    <Badge className="shrink-0 bg-signal-warn/15 px-1.5 py-0 text-2xs text-signal-warn">
                      -{discount}%
                    </Badge>
                  )}
                  {m.caps?.vision && (
                    <Badge variant="outline" className="shrink-0 gap-0.5 px-1.5 py-0 text-2xs">
                      <Eye className="h-2.5 w-2.5" />
                      {t("models.caps.vision")}
                    </Badge>
                  )}
                  {m.caps?.reasoning && (
                    <Badge variant="outline" className="shrink-0 gap-0.5 px-1.5 py-0 text-2xs">
                      <Sparkles className="h-2.5 w-2.5" />
                      {t("models.caps.reasoning")}
                    </Badge>
                  )}
                </div>

                {/* 价格概览：闲/忙时或多档模型分行展示，右侧保留上下文规模 */}
                <div className="flex items-center justify-between gap-2 text-2xs text-muted-foreground">
                  <PriceLines pricing={pricing} free={free} />
                  {m.contextLength != null && (
                    <span className="shrink-0">
                      {t("models.contextTokens", {
                        p0: formatContextTokens(m.contextLength),
                      })}
                    </span>
                  )}
                </div>
              </Card>
            );
          })}
        </div>
      )}
    </div>
  );
}
