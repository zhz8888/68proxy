import { useEffect, useMemo, useRef, useState } from "react";
import { Download, Eraser, Search } from "lucide-react";
import { toast } from "sonner";
import { save } from "@tauri-apps/plugin-dialog";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Switch } from "@/components/ui/switch";
import { api, onLog, type LogEntry } from "@/lib/api";
import { formatLogTime } from "@/lib/format";
import { cn } from "@/lib/utils";

type Level = "all" | "info" | "warn" | "error";

const LEVEL_COLOR: Record<string, string> = {
  info: "text-signal-info",
  warn: "text-signal-warn",
  error: "text-signal-error",
  debug: "text-muted-foreground",
};

const LEVEL_DOT: Record<string, string> = {
  info: "bg-signal-info",
  warn: "bg-signal-warn",
  error: "bg-signal-error",
  debug: "bg-muted-foreground/60",
};

export function LogsView() {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [level, setLevel] = useState<Level>("all");
  const [keyword, setKeyword] = useState("");
  const [autoscroll, setAutoscroll] = useState(true);
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    api
      .logsGet(500)
      .then((entries) => setLogs(entries))
      .catch(() => {});
    const off = onLog((entry) => setLogs((prev) => [...prev.slice(-999), entry]));
    return () => {
      off.then((f) => f());
    };
  }, []);

  useEffect(() => {
    if (autoscroll) {
      bottomRef.current?.scrollIntoView({ behavior: "auto" });
    }
  }, [logs, autoscroll]);

  const filtered = useMemo(() => {
    return logs.filter((l) => {
      if (level !== "all" && l.level !== level) return false;
      if (keyword && !l.msg.toLowerCase().includes(keyword.toLowerCase())) return false;
      return true;
    });
  }, [logs, level, keyword]);

  async function exportLogs() {
    try {
      const path = await save({
        defaultPath: "68proxy-logs.log",
        filters: [{ name: "日志文件", extensions: ["log", "txt"] }],
      });
      if (!path) return;
      const count = await api.logsExport(path);
      toast.success(`已导出 ${count} 条日志`);
    } catch (e) {
      toast.error(String(e));
    }
  }

  return (
    <div className="flex h-full flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="flex items-center gap-1 rounded-lg bg-muted p-1">
          {(["all", "info", "warn", "error"] as Level[]).map((l) => (
            <button
              key={l}
              onClick={() => setLevel(l)}
              className={cn(
                "rounded-md px-3 py-1 text-xs transition-colors",
                level === l ? "bg-background text-foreground shadow" : "text-muted-foreground hover:text-foreground",
              )}
            >
              {l === "all" ? "全部" : l === "info" ? "信息" : l === "warn" ? "警告" : "错误"}
            </button>
          ))}
        </div>
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={keyword}
            onChange={(e) => setKeyword(e.target.value)}
            placeholder="搜索日志…"
            className="h-8 w-56 pl-8 text-xs"
          />
        </div>
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          自动滚动
          <Switch checked={autoscroll} onCheckedChange={setAutoscroll} />
        </div>
        <div className="ml-auto flex items-center gap-1">
          <Button variant="ghost" size="sm" onClick={exportLogs}>
            <Download />
            导出
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              api.logsClear();
              setLogs([]);
            }}
          >
            <Eraser />
            清空
          </Button>
        </div>
      </div>

      <ScrollArea className="flex-1 rounded-lg border border-border bg-card/60">
        <div className="select-text p-3 font-mono text-[12.5px] leading-6">
          {filtered.length === 0 ? (
            <p className="px-2 py-8 text-center text-muted-foreground">
              {keyword || level !== "all" ? "没有匹配的日志" : "还没有日志——启动代理后这里会实时显示运行记录。"}
            </p>
          ) : (
            filtered.map((l) => (
              <div key={l.seq} className="flex gap-3 whitespace-pre-wrap break-all px-2 hover:bg-secondary/40">
                <span className="shrink-0 text-muted-foreground/70">{formatLogTime(l.ts)}</span>
                <span className={cn("mt-[9px] h-1.5 w-1.5 shrink-0 rounded-full", LEVEL_DOT[l.level] ?? "bg-muted-foreground")} />
                <span className={LEVEL_COLOR[l.level] ?? "text-foreground"}>{l.msg}</span>
              </div>
            ))
          )}
          <div ref={bottomRef} />
        </div>
      </ScrollArea>
    </div>
  );
}
