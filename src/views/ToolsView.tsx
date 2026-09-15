import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { Check, ChevronDown, Copy, ListFilter, Plug, Sparkles } from "lucide-react";

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
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { api, onStatus, type ModelInfo, type ProxyStatus } from "@/lib/api";
import { DEFAULT_PORT } from "@/lib/constants";
import { copyText } from "@/lib/format";

// 模型配置模式：由上游自动获取全部模型，或手动指定要接入的模型
type ModelMode = "upstream" | "custom";

/** 生成发给 AI 的接入提示词：描述本地代理的接口信息，让 AI 替用户把代理接入目标工具。
 * @param t 翻译函数
 * @param selected 手动模式下选中的模型 ID 列表
 */
function buildPrompt(
  t: TFunction,
  port: number,
  mode: ModelMode,
  selected: string[],
  toolName: string,
): string {
  const tool = toolName.trim() || t("tools.defaultToolName");
  const selectedText = selected.join(t("tools.listSeparator"));
  const modelBullet =
    mode === "upstream"
      ? t("tools.prompt.modelUpstream", { p0: tool })
      : t("tools.prompt.modelCustom", { p0: selectedText });
  const reqBullet =
    mode === "upstream"
      ? t("tools.prompt.reqUpstream", { p0: port, p1: tool })
      : t("tools.prompt.reqCustom", { p0: selectedText });
  return [
    t("tools.prompt.title", { p0: tool }),
    t("tools.prompt.intro", { p0: tool }),
    t("tools.prompt.proxyInfo", { p0: port, p1: modelBullet }),
    t("tools.prompt.protocols"),
    t("tools.prompt.requirements", { p0: reqBullet, p1: tool, p2: port }),
  ].join("\n\n");
}

/** 生成发给 AI 的移除提示词：让 AI 删除目标工具中指向本代理的供应商配置。 */
function buildRemovePrompt(t: TFunction, port: number, toolName: string): string {
  const tool = toolName.trim() || t("tools.defaultToolName");
  return [
    t("tools.removePrompt.title", { p0: tool }),
    t("tools.removePrompt.intro", { p0: tool }),
    t("tools.removePrompt.targets", { p0: tool }),
    t("tools.removePrompt.criteria", { p0: port }),
    t("tools.removePrompt.requirements"),
  ].join("\n\n");
}

/** 工具接入视图：生成接入/移除本地代理的 AI 提示词，用户复制后发给目标工具的 AI 助手完成配置。 */
export function ToolsView() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [mode, setMode] = useState<ModelMode>("upstream");
  const [selected, setSelected] = useState<string[]>([]);
  const [toolName, setToolName] = useState("");
  const [copied, setCopied] = useState(false);
  const [removeCopied, setRemoveCopied] = useState(false);
  // 两个“已复制”提示的复位定时器句柄（重复点击时需先清旧定时器）
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const removeCopyTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  // draftMode/draftSelected 是弹窗中的临时选择，点“确定”后才应用到 mode/selected
  const [draftMode, setDraftMode] = useState<ModelMode>("upstream");
  const [draftSelected, setDraftSelected] = useState<string[]>([]);

  // 挂载时获取端口与模型列表，并订阅状态变化以保持端口实时准确
  useEffect(() => {
    api.proxyStatus().then(setStatus).catch(() => {});
    const off = onStatus(setStatus);
    api
      .modelsGet(false)
      .then((r) => setModels(r.data))
      .catch(() => {});
    return () => {
      // 卸载时取消状态订阅
      off.then((f) => f());
    };
  }, []);

  // 端口未就绪时回退到默认端口
  const port = status?.port ?? DEFAULT_PORT;

  // 模型选择结果的摘要文案（用于按钮与标题展示）
  const summary = useMemo(() => {
    if (mode === "upstream") return t("tools.summary.upstream");
    if (selected.length === 0) return t("tools.summary.none");
    if (selected.length <= 2) return selected.join(t("tools.listSeparator"));
    return t("tools.summary.count", { p0: selected.length });
  }, [mode, selected, t]);

  const prompt = useMemo(
    () => buildPrompt(t, port, mode, selected, toolName),
    [t, port, mode, selected, toolName],
  );
  const removePrompt = useMemo(
    () => buildRemovePrompt(t, port, toolName),
    [t, port, toolName],
  );

  /** 复制接入提示词，并短暂显示“已复制”状态。 */
  async function copyPrompt() {
    if (await copyText(prompt)) {
      setCopied(true);
      // 清掉上一次的复位定时器，避免频繁点击时提示被旧定时器提前清掉
      if (copyTimer.current !== null) clearTimeout(copyTimer.current);
      copyTimer.current = setTimeout(() => {
        setCopied(false);
        copyTimer.current = null;
      }, 1500);
    }
  }

  /** 复制移除提示词，并短暂显示“已复制”状态。 */
  async function copyRemovePrompt() {
    if (await copyText(removePrompt)) {
      setRemoveCopied(true);
      if (removeCopyTimer.current !== null) clearTimeout(removeCopyTimer.current);
      removeCopyTimer.current = setTimeout(() => {
        setRemoveCopied(false);
        removeCopyTimer.current = null;
      }, 1500);
    }
  }

  /** 打开模型选择弹窗，用当前生效的配置初始化草稿。 */
  function openDialog() {
    setDraftMode(mode);
    setDraftSelected(mode === "upstream" ? [] : selected);
    setDialogOpen(true);
  }

  /** 勾选/取消草稿中的模型；任何勾选操作都会自动切换到手动指定模式。 */
  function toggleDraftModel(id: string) {
    setDraftMode("custom");
    setDraftSelected((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
  }

  /** 把弹窗草稿应用为正式配置并关闭弹窗。 */
  function confirmDialog() {
    setMode(draftMode);
    setSelected(draftMode === "upstream" ? [] : draftSelected);
    setDialogOpen(false);
  }

  return (
    <div className="space-y-4 pb-8">
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-sm">
            <Plug className="h-4 w-4" />
            {t("tools.title")}
          </CardTitle>
          <CardDescription>{t("tools.description")}</CardDescription>
        </CardHeader>
        <CardContent className="grid grid-cols-2 gap-4 pt-0">
          <div className="space-y-1.5">
            <Label>{t("tools.targetTool")}</Label>
            <Input
              value={toolName}
              onChange={(e) => setToolName(e.target.value)}
              placeholder={t("tools.toolPlaceholder")}
            />
          </div>
          <div className="space-y-1.5">
            <Label>{t("tools.models")}</Label>
            <Button
              type="button"
              variant="outline"
              onClick={openDialog}
              className="h-9 w-full justify-between px-3 font-mono text-xs"
            >
              <span className="truncate">{summary}</span>
              {mode === "upstream" ? (
                <Sparkles className="h-3.5 w-3.5 shrink-0 text-signal-success" />
              ) : (
                <ChevronDown className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              )}
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center text-sm">
            {t("tools.installPromptTitle")}
            <span className="ml-2 rounded-md border border-border bg-muted/50 px-2 py-0.5 font-mono text-2xs text-muted-foreground">
              {t("tools.portBadge", { p0: port, p1: summary })}
            </span>
          </CardTitle>
        </CardHeader>
        <CardContent className="pt-0">
          <div className="relative">
            <pre className="select-text max-h-80 overflow-y-auto whitespace-pre-wrap rounded-md border border-border bg-muted/30 p-4 font-mono text-xs leading-6 text-foreground/90">
              {prompt}
            </pre>
            <Button
              variant="secondary"
              size="sm"
              className="absolute right-2 top-2"
              onClick={copyPrompt}
            >
              {copied ? <Check /> : <Copy />}
              {copied ? t("common.copied") : t("tools.copyPrompt")}
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-sm">
            <Plug className="h-4 w-4" />
            {t("tools.removeTitle")}
          </CardTitle>
          <CardDescription>
            {t("tools.removeDescription", {
              p0: toolName.trim() || t("tools.defaultToolName"),
            })}
          </CardDescription>
        </CardHeader>
        <CardContent className="pt-0">
          <div className="relative">
            <pre className="select-text max-h-80 overflow-y-auto whitespace-pre-wrap rounded-md border border-border bg-muted/30 p-4 font-mono text-xs leading-6 text-foreground/90">
              {removePrompt}
            </pre>
            <Button
              variant="secondary"
              size="sm"
              className="absolute right-2 top-2"
              onClick={copyRemovePrompt}
            >
              {removeCopied ? <Check /> : <Copy />}
              {removeCopied ? t("common.copied") : t("tools.copyRemovePrompt")}
            </Button>
          </div>
        </CardContent>
      </Card>

      <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>{t("tools.dialog.title")}</DialogTitle>
            <DialogDescription>{t("tools.dialog.description")}</DialogDescription>
          </DialogHeader>

          <div className="space-y-2">
            <button
              type="button"
              onClick={() => setDraftMode("upstream")}
              className={cn(
                "flex w-full items-start gap-3 rounded-lg border p-3 text-left transition-colors",
                draftMode === "upstream"
                  ? "border-signal-success/60 bg-signal-success/10"
                  : "border-border hover:bg-muted/30",
              )}
            >
              <Sparkles className="mt-0.5 h-4 w-4 shrink-0 text-signal-success" />
              <div>
                <div className="text-sm font-medium">{t("tools.dialog.upstreamTitle")}</div>
                <div className="mt-0.5 text-xs leading-5 text-muted-foreground">
                  {t("tools.dialog.upstreamDesc")}
                </div>
              </div>
            </button>
            <button
              type="button"
              onClick={() => setDraftMode("custom")}
              className={cn(
                "flex w-full items-start gap-3 rounded-lg border p-3 text-left transition-colors",
                draftMode === "custom"
                  ? "border-signal-warn/60 bg-signal-warn/10"
                  : "border-border hover:bg-muted/30",
              )}
            >
              <ListFilter className="mt-0.5 h-4 w-4 shrink-0 text-signal-warn" />
              <div>
                <div className="text-sm font-medium">{t("tools.dialog.customTitle")}</div>
                <div className="mt-0.5 text-xs leading-5 text-muted-foreground">
                  {t("tools.dialog.customDesc")}
                </div>
              </div>
            </button>
          </div>

          {draftMode === "custom" && (
            <div className="space-y-1.5">
              <Label>{t("tools.dialog.selectedCount", { p0: draftSelected.length })}</Label>
              <div className="h-64 overflow-y-auto rounded-md border border-border">
                <div className="p-1.5">
                  {models.map((m) => {
                    const active = draftSelected.includes(m.id);
                    return (
                      <button
                        key={m.id}
                        type="button"
                        onClick={() => toggleDraftModel(m.id)}
                        className={cn(
                          "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left font-mono text-xs transition-colors",
                          active
                            ? "bg-muted/60 text-foreground"
                            : "text-muted-foreground hover:bg-muted/30 hover:text-foreground",
                        )}
                      >
                        <span
                          className={cn(
                            "flex h-4 w-4 shrink-0 items-center justify-center rounded-sm border",
                            active ? "border-signal-success bg-signal-success/20" : "border-border",
                          )}
                        >
                          {active && <Check className="h-3 w-3 text-signal-success" />}
                        </span>
                        <span className="truncate">{m.id}</span>
                      </button>
                    );
                  })}
                  {models.length === 0 && (
                    <p className="px-2 py-3 text-xs text-muted-foreground">
                      {t("tools.dialog.emptyModels")}
                    </p>
                  )}
                </div>
              </div>
            </div>
          )}

          <DialogFooter>
            <Button variant="secondary" onClick={() => setDialogOpen(false)}>
              {t("common.cancel")}
            </Button>
            <Button
              onClick={confirmDialog}
              disabled={draftMode === "custom" && draftSelected.length === 0}
            >
              {t("common.confirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
