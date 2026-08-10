import { useEffect, useMemo, useState } from "react";
import { Check, Copy, RefreshCw, Search } from "lucide-react";

import { ModelLogo, providerForModel } from "@/components/ModelLogo";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { api, type ModelInfo } from "@/lib/api";
import { copyText } from "@/lib/format";

export function ModelsView() {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [fallback, setFallback] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [search, setSearch] = useState("");
  const [copiedId, setCopiedId] = useState("");

  async function load(force: boolean) {
    setLoading(true);
    setError("");
    try {
      const res = await api.modelsGet(force);
      setModels(res.data);
      setFallback(res.fallback);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    load(false);
  }, []);

  const filtered = useMemo(() => {
    const q = search.toLowerCase();
    if (!q) return models;
    return models.filter((m) => m.id.toLowerCase().includes(q) || m.name.toLowerCase().includes(q));
  }, [models, search]);

  async function copy(id: string) {
    if (await copyText(id)) {
      setCopiedId(id);
      setTimeout(() => setCopiedId(""), 1400);
    }
  }

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
            <Card key={i} className="h-20 animate-pulse bg-secondary/40" />
          ))}
        </div>
      ) : filtered.length === 0 ? (
        <div className="flex flex-1 items-center justify-center rounded-lg border border-dashed border-border">
          <p className="text-sm text-muted-foreground">
            {search ? "没有匹配的模型" : "还没有模型——先启动代理，再用已保存的 API Key 刷新一次。"}
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-3 gap-3 overflow-y-auto pb-4 pr-1">
          {filtered.map((m) => {
            const provider = providerForModel(m.id);
            return (
              <Card
                key={m.id}
                className="group flex items-center gap-3 p-3 transition-colors hover:border-primary/40"
              >
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
              </Card>
            );
          })}
        </div>
      )}
    </div>
  );
}
