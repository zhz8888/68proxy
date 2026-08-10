import { useEffect, useMemo, useState } from "react";
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
import { copyText } from "@/lib/format";

type ModelMode = "upstream" | "custom";

function buildPrompt(port: number, mode: ModelMode, selected: string[], toolName: string): string {
  const tool = toolName.trim() || "目标工具";
  const modelBullet =
    mode === "upstream"
      ? `- 模型配置方式: 通过 GET /v1/models 自动获取全部模型；如果「${tool}」支持从 base_url 自动拉取模型列表，只需配置 base_url，不要手动枚举模型`
      : `- 所需模型（只配置这些）: ${selected.join("、")}`;
  const reqBullet =
    mode === "upstream"
      ? `- 调用 GET http://127.0.0.1:${port}/v1/models 获取全量模型列表并全部配置到「${tool}」；若「${tool}」支持从 base_url 自动拉取模型列表，直接填 base_url 即可`
      : `- 只配置我指定的这些模型：${selected.join("、")}，不要添加列表以外的任何模型`;
  return `# 任务：把本地代理接入「${tool}」

请先阅读以下本地代理的接口信息，然后帮我把「${tool}」配置为使用该代理。如果该工具不支持下面列出的任何一种协议，请直接回复「不支持」，不要尝试其他方案。

## 代理信息
- OpenAI 兼容基础地址（Chat Completions）: http://127.0.0.1:${port}/v1
- Anthropic Messages 基础地址: http://127.0.0.1:${port}
- 模型列表: GET http://127.0.0.1:${port}/v1/models（返回全部可用模型）
- ${modelBullet}
- API Key: 任意占位符即可（例如 sk-placeholder），代理会自动使用本机已保存的真实 Key；也可以传 user_ 开头的 Key（请求头优先）
- 健康检查: GET http://127.0.0.1:${port}/health（返回 OK 表示可用）

## 支持的协议（务必核对工具支持哪一种，优先 OpenAI）
1) OpenAI 兼容协议（首选）:
   - POST /v1/chat/completions，流式 SSE 与非流式 JSON 均支持
   - 支持 system/user/assistant/tool 消息角色、工具调用 tools / tool_choice、多模态图片输入（image_url）、reasoning_effort、max_tokens、temperature
   - 流式响应为标准 SSE：data: {"choices":[{"delta":{"content":...}}]}，结束为 data: [DONE]
2) Anthropic Messages 协议（备选）:
   - POST /v1/messages，流式 SSE 与非流式 JSON 均支持
   - 支持顶层 system 字段、content 数组（text / tool_use / tool_result）、thinking.budget_tokens 映射、max_tokens
3) 模型列表: GET /v1/models，返回 OpenAI 格式的模型数组
4) 健康检查: GET /health

## 接入要求
- ${reqBullet}
- **禁止对模型列表中的每个模型逐一发消息测试可用性**；如需发测试消息验证，只允许使用模型 「deepseek/deepseek-v4-flash」，不要向其他模型发送任何请求
- 如果健康检查或验证请求失败，**必须先问用户两件事：① 是否已在 68proxy 中填写了有效的 API Key；② 是否已启动中继代理服务**；在得到用户明确回答之前，不要自行判断原因或跳过排查
- 给「${tool}」配置代理时，必须**新建一个独立的自定义供应商/服务商条目**（例如命名 68proxy），指向本代理的 base_url；**绝对禁止修改、覆盖、替换该工具已有的其他供应商、模型或 base_url 配置**
- 如果「${tool}」支持自定义供应商且支持 OpenAI 兼容协议：新增供应商，base_url 设为 http://127.0.0.1:${port}/v1，模型的 models 列表写入 /v1/models 返回的全量结果
- 如果「${tool}」只支持 Anthropic 协议：新增供应商，base_url 设为 http://127.0.0.1:${port}，模型用 /v1/models 返回的列表
- 如果两者都不支持：直接回复「不支持」
- OpenCode 具体做法参考：在 opencode.json 的 provider 字段下**新增**一项（例如 "68proxy"，npm 包用 @ai-sdk/openai-compatible，options.baseURL 指向上述地址，apiKey 用占位符），models 里写入 /v1/models 返回的模型 id；已有项一律不动
- 给出具体的配置位置（界面字段 / 配置文件 / 环境变量），以及如何验证（例如调用 GET /v1/models 或发一条测试消息）
- 配置完成后，说明如何切换回原服务，方便随时恢复`;
}

function buildRemovePrompt(port: number, toolName: string): string {
  const tool = toolName.trim() || "目标工具";
  return `# 任务：把「${tool}」中接入的本地代理供应商移除

请帮我把之前接入到「${tool}」的 68proxy 本地代理供应商/服务商删除。

## 移除目标
- 该供应商条目（例如 opencode.json 中 provider 下名为 68proxy 的那一项，或 Cursor / 其他工具中 base_url 指向本代理的 provider）
- 该供应商下所有模型（当时从 /v1/models 写入的全部模型 id）
- **绝对不要改动「${tool}」中其他任何供应商、模型或配置**

## 判断依据
- 供应商的 base_url 指向 http://127.0.0.1:${port} 或 http://127.0.0.1:${port}/v1，即视为本次接入的本地代理
- 如果找不到这样的供应商，直接回复「未找到」，不要删除任何内容

## 移除要求
- 删除时保留该工具原有的其他所有供应商与配置
- 说明删除的位置（界面字段 / 配置文件），以及如何验证（例如重新打开模型选择器确认该供应商已消失）`;
}

export function ToolsView() {
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [mode, setMode] = useState<ModelMode>("upstream");
  const [selected, setSelected] = useState<string[]>([]);
  const [toolName, setToolName] = useState("");
  const [copied, setCopied] = useState(false);
  const [removeCopied, setRemoveCopied] = useState(false);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [draftMode, setDraftMode] = useState<ModelMode>("upstream");
  const [draftSelected, setDraftSelected] = useState<string[]>([]);

  useEffect(() => {
    api.proxyStatus().then(setStatus).catch(() => {});
    const off = onStatus(setStatus);
    api
      .modelsGet(false)
      .then((r) => setModels(r.data))
      .catch(() => {});
    return () => {
      off.then((f) => f());
    };
  }, []);

  const port = status?.port ?? 3050;

  const summary = useMemo(() => {
    if (mode === "upstream") return "根据上游自动获取";
    if (selected.length === 0) return "未选择模型";
    if (selected.length <= 2) return selected.join("、");
    return `${selected.length} 个模型`;
  }, [mode, selected]);

  const prompt = useMemo(
    () => buildPrompt(port, mode, selected, toolName),
    [port, mode, selected, toolName],
  );
  const removePrompt = useMemo(
    () => buildRemovePrompt(port, toolName),
    [port, toolName],
  );

  async function copyPrompt() {
    if (await copyText(prompt)) {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    }
  }

  async function copyRemovePrompt() {
    if (await copyText(removePrompt)) {
      setRemoveCopied(true);
      setTimeout(() => setRemoveCopied(false), 1500);
    }
  }

  function openDialog() {
    setDraftMode(mode);
    setDraftSelected(mode === "upstream" ? [] : selected);
    setDialogOpen(true);
  }

  function toggleDraftModel(id: string) {
    setDraftMode("custom");
    setDraftSelected((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
  }

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
            工具接入
          </CardTitle>
          <CardDescription>
            把 68proxy 的本地代理接入 Cursor / OpenCode / Cherry Studio / 自研工具等：填好工具名与模型，
            复制下方提示词发给 AI，让它替你完成接入；协议不支持时它会回复「不支持」。
          </CardDescription>
        </CardHeader>
        <CardContent className="grid grid-cols-2 gap-4 pt-0">
          <div className="space-y-1.5">
            <Label>目标工具</Label>
            <Input
              value={toolName}
              onChange={(e) => setToolName(e.target.value)}
              placeholder="例如 Cursor、OpenCode、Cherry Studio（留空则写“目标工具”）"
            />
          </div>
          <div className="space-y-1.5">
            <Label>模型</Label>
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
            接入提示词
            <span className="ml-2 rounded-md border border-border bg-muted/50 px-2 py-0.5 font-mono text-[10px] text-muted-foreground">
              端口 {port} · {summary}
            </span>
          </CardTitle>
        </CardHeader>
        <CardContent className="pt-0">
          <div className="relative">
            <pre className="select-text max-h-80 overflow-y-auto whitespace-pre-wrap rounded-md border border-border bg-muted/30 p-4 font-mono text-[12px] leading-6 text-foreground/90">
              {prompt}
            </pre>
            <Button
              variant="secondary"
              size="sm"
              className="absolute right-2 top-2"
              onClick={copyPrompt}
            >
              {copied ? <Check /> : <Copy />}
              {copied ? "已复制" : "复制提示词"}
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-sm">
            <Plug className="h-4 w-4" />
            移除接入
          </CardTitle>
          <CardDescription>
            如需撤销之前的接入，复制下方提示词发给 AI，让它删除「{toolName.trim() || "目标工具"}」里指向本代理的本地供应商。
          </CardDescription>
        </CardHeader>
        <CardContent className="pt-0">
          <div className="relative">
            <pre className="select-text max-h-80 overflow-y-auto whitespace-pre-wrap rounded-md border border-border bg-muted/30 p-4 font-mono text-[12px] leading-6 text-foreground/90">
              {removePrompt}
            </pre>
            <Button
              variant="secondary"
              size="sm"
              className="absolute right-2 top-2"
              onClick={copyRemovePrompt}
            >
              {removeCopied ? <Check /> : <Copy />}
              {removeCopied ? "已复制" : "复制移除提示词"}
            </Button>
          </div>
        </CardContent>
      </Card>

      <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>选择模型</DialogTitle>
            <DialogDescription>根据上游让 AI 自动拉取全部模型，或手动指定只配置哪些模型。</DialogDescription>
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
                <div className="text-sm font-medium">根据上游自动获取</div>
                <div className="mt-0.5 text-xs leading-5 text-muted-foreground">
                  AI 通过 GET /v1/models 拉取全部模型填入；工具支持自动拉取模型列表时只需填 base_url。
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
                <div className="text-sm font-medium">手动指定模型</div>
                <div className="mt-0.5 text-xs leading-5 text-muted-foreground">
                  只为所选模型创建供应商，不添加其他模型。
                </div>
              </div>
            </button>
          </div>

          {draftMode === "custom" && (
            <div className="space-y-1.5">
              <Label>已选 {draftSelected.length} 个</Label>
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
                      模型列表为空，请先启动代理。
                    </p>
                  )}
                </div>
              </div>
            </div>
          )}

          <DialogFooter>
            <Button variant="secondary" onClick={() => setDialogOpen(false)}>
              取消
            </Button>
            <Button
              onClick={confirmDialog}
              disabled={draftMode === "custom" && draftSelected.length === 0}
            >
              确定
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
