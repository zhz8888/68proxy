import { useEffect, useMemo, useState } from "react";
import { Check, Copy, Eye, RefreshCw, Search, Sparkles } from "lucide-react";

import { ModelLogo, providerForModel } from "@/components/ModelLogo";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { api, type ModelInfo, type ModelPricing } from "@/lib/api";
import { copyText, formatContextTokens, formatPrice } from "@/lib/format";

/** 能力/价格筛选维度。 */
type Filter = "all" | "vision" | "reasoning" | "free";

/** 模型卡片当前应展示的最低档费率（用于列表内的价格概览）。 */
function baseRates(p: ModelPricing | undefined) {
  return p?.tiers[0]?.rates;
}

/**
 * 按与后端一致的口径匹配模型 ID：精确 → 去 provider 前缀的短名 → 最长前缀。
 * @param catalog 内置计费表（后端下发，id 为官方小写形式）
 * @param id 上游返回的模型 ID（可能带前缀/日期后缀、大小写不一）
 */
function matchPricing(catalog: Map<string, ModelPricing>, id: string): ModelPricing | undefined {
  const target = id.toLowerCase();
  const exact = catalog.get(target);
  if (exact) return exact;
  const short = target.split("/").pop() ?? target;
  const byShort = catalog.get(short);
  if (byShort) return byShort;
  let best: ModelPricing | undefined;
  for (const [key, p] of catalog) {
    if (short.startsWith(key) && (!best || key.length > best.id.length)) best = p;
  }
  return best;
}

/** 模型视图：展示可用模型列表及其能力与价格，支持搜索、能力筛选、复制与手动刷新。 */
export function ModelsView() {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [catalog, setCatalog] = useState<Map<string, ModelPricing>>(new Map());
  const [fallback, setFallback] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [copiedId, setCopiedId] = useState("");

  /** 拉取模型列表与内置计费表。
   * @param force 为 true 时跳过缓存，强制向上游刷新
   */
  async function load(force: boolean) {
    setLoading(true);
    setError("");
    try {
      const [res, cat] = await Promise.all([api.modelsGet(force), api.modelsCatalog()]);
      setModels(res.data);
      setFallback(res.fallback);
      setCatalog(new Map(cat.map((c) => [c.id.toLowerCase(), c])));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  // 首次进入优先用缓存，不强制刷新上游
  useEffect(() => {
    load(false);
  }, []);

  // 每行模型都带上匹配到的计费信息，供筛选与展示复用
  const rows = useMemo(
    () => models.map((m) => ({ model: m, pricing: matchPricing(catalog, m.id) })),
    [models, catalog],
  );

  const counts = useMemo(
    () => ({
      vision: rows.filter((r) => r.pricing?.caps.vision).length,
      reasoning: rows.filter((r) => r.pricing?.caps.reasoning).length,
      free: rows.filter((r) => r.pricing?.deal?.free).length,
    }),
    [rows],
  );

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    return rows.filter(({ model, pricing }) => {
      if (q && !model.id.toLowerCase().includes(q) && !model.name.toLowerCase().includes(q)) {
        return false;
      }
      switch (filter) {
        case "vision":
          return !!pricing?.caps.vision;
        case "reasoning":
          return !!pricing?.caps.reasoning;
        case "free":
          return !!pricing?.deal?.free;
        default:
          return true;
      }
    });
  }, [rows, search, filter]);

  /** 复制模型 ID 到剪贴板，并在对应按钮上短暂显示“已复制”图标。 */
  async function copy(id: string) {
    if (await copyText(id)) {
      setCopiedId(id);
      setTimeout(() => setCopiedId(""), 1400);
    }
  }

  const FILTERS: Array<{ value: Filter; label: string; count?: number }> = [
    { value: "all", label: "全部" },
    { value: "vision", label: "视觉", count: counts.vision },
    { value: "reasoning", label: "思考", count: counts.reasoning },
    { value: "free", label: "免费", count: counts.free },
  ];

  return (
    <div className="flex h-full flex-col gap-3">
      <div className="flex items-center gap-2">
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="搜索模型…"
            className="h-8 w-64 pl-8 text-xs"
          />
        </div>
        {/* 能力 / 免费筛选 */}
        <div className="flex items-center gap-1">
          {FILTERS.map((f) => (
            <Button
              key={f.value}
              variant={filter === f.value ? "secondary" : "ghost"}
              size="sm"
              className="h-7 px-2 text-xs"
              onClick={() => setFilter(f.value)}
              disabled={f.count === 0 && f.value !== "all"}
            >
              {f.label}
              {f.count !== undefined && f.count > 0 && (
                <span className="ml-1 text-muted-foreground">{f.count}</span>
              )}
            </Button>
          ))}
        </div>
        <span className="text-xs text-muted-foreground">
          {models.length > 0 ? `共 ${models.length} 个模型` : ""}
        </span>
        <Button
          variant="ghost"
          size="sm"
          className="ml-auto"
          disabled={loading}
          onClick={() => load(true)}
        >
          <RefreshCw className={loading ? "animate-spin" : ""} />
          刷新
        </Button>
      </div>

      {error && (
        <div className="rounded-md border border-destructive/40 bg-destructive/5 px-4 py-3 text-sm text-destructive">
          {error}
          <button className="ml-2 underline" onClick={() => load(true)}>
            重试
          </button>
        </div>
      )}

      {fallback && !error && !loading && models.length > 0 && (
        <div className="rounded-md border border-signal-warn/40 bg-signal-warn/5 px-3 py-2 text-xs text-signal-warn">
          当前显示内置模型列表：未保存 API Key 或 Provider 拉取失败。可在「配置 → 凭据」保存 Key 后刷新。
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
            {search || filter !== "all" ? "没有匹配的模型" : "还没有模型——先启动代理，再用已保存的 API Key 刷新一次。"}
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-3 gap-3 overflow-y-auto pb-4 pr-1">
          {filtered.map(({ model: m, pricing }) => {
            const provider = pricing?.provider ?? providerForModel(m.id);
            const rates = baseRates(pricing);
            const free = !!pricing?.deal?.free;
            const discount = pricing?.deal && !free ? pricing.deal.discountPercent : 0;
            const multiTier = (pricing?.tiers.length ?? 0) > 1;
            return (
              <Card
                key={m.id}
                className="group flex flex-col gap-2 p-3 transition-colors hover:border-primary/40"
              >
                <div className="flex items-center gap-3">
                  <ModelLogo model={m.id} size={22} />
                  <div className="min-w-0 flex-1">
                    <p className="select-text truncate font-mono text-[12.5px]" title={m.id}>
                      {m.id}
                    </p>
                    <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                      {provider && (
                        <Badge variant="secondary" className="shrink-0 px-1.5 py-0 text-[10px]">
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
                    title="复制模型 ID"
                  >
                    {copiedId === m.id ? <Check className="text-success" /> : <Copy />}
                  </Button>
                </div>

                {/* 能力与促销标记 */}
                <div className="flex flex-wrap items-center gap-1">
                  {free && (
                    <Badge className="shrink-0 bg-signal-success/15 px-1.5 py-0 text-[10px] text-signal-success">
                      免费
                    </Badge>
                  )}
                  {discount > 0 && (
                    <Badge className="shrink-0 bg-signal-warn/15 px-1.5 py-0 text-[10px] text-signal-warn">
                      -{discount}%
                    </Badge>
                  )}
                  {pricing?.caps.vision && (
                    <Badge variant="outline" className="shrink-0 gap-0.5 px-1.5 py-0 text-[10px]">
                      <Eye className="h-2.5 w-2.5" />
                      视觉
                    </Badge>
                  )}
                  {pricing?.caps.reasoning && (
                    <Badge variant="outline" className="shrink-0 gap-0.5 px-1.5 py-0 text-[10px]">
                      <Sparkles className="h-2.5 w-2.5" />
                      思考
                    </Badge>
                  )}
                </div>

                {/* 价格概览：按 1M tokens 的输入/输出单价；免费或未收录时给出对应说明 */}
                <div className="flex items-center justify-between text-[10.5px] text-muted-foreground">
                  {rates ? (
                    free ? (
                      <span className="text-signal-success">限时免费，0 成本</span>
                    ) : (
                      <span className="font-mono">
                        {formatPrice(rates.input)} in / {formatPrice(rates.output)} out
                        <span className="ml-1 opacity-70">per 1M</span>
                      </span>
                    )
                  ) : (
                    <span>未收录价格</span>
                  )}
                  <span className="flex items-center gap-1">
                    {multiTier && <span title="按输入规模分档计费">分档</span>}
                    {pricing?.timeOfDay && (
                      <span className="text-signal-info" title={pricing.timeOfDay.windows}>
                        闲/忙时
                      </span>
                    )}
                    {pricing?.contextWindow && (
                      <span>ctx {formatContextTokens(pricing.contextWindow)}</span>
                    )}
                  </span>
                </div>
              </Card>
            );
          })}
        </div>
      )}
    </div>
  );
}
