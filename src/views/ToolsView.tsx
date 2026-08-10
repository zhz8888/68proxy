import { useEffect, useMemo, useState } from "react";
import { Check, Copy, Plug } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { api, onStatus, type ModelInfo, type ProxyStatus } from "@/lib/api";
import { copyText } from "@/lib/format";

function buildPrompt(port: number, model: string, toolName: string): string {
  const tool = toolName.trim() || "目标工具";
  return `# 任务：把本地代理接入「${tool}」

请先阅读以下本地代理的接口信息，然后帮我把「${tool}」配置为使用该代理。如果该工具不支持下面列出的任何一种协议，请直接回复「不支持」，不要尝试其他方案。

## 代理信息
- OpenAI 兼容基础地址（Chat Completions）: http://127.0.0.1:${port}/v1
- Anthropic Messages 基础地址: http://127.0.0.1:${port}
- 建议使用的模型: ${model}
- API Key: 任意占位符即可（例如 sk-placeholder），代理会自动使用本机已保存的真实 Key；也可以传 user_ 开头的 Key（请求头优先）
- 健康检查: GET http://127.0.0.1:${port}/health（返回 OK 表示可用）

## 支持的协议（务必核对工具支持哪一种）
1) OpenAI 兼容协议（推荐）:
   - POST /v1/chat/completions，流式 SSE 与非流式 JSON 均支持
   - 支持 system/user/assistant/tool 消息角色、工具调用 tools / tool_choice、多模态图片输入（image_url）、reasoning_effort、max_tokens、temperature
   - 流式响应为标准 SSE：data: {"choices":[{"delta":{"content":...}}]}，结束为 data: [DONE]
2) Anthropic Messages 协议:
   - POST /v1/messages，流式 SSE 与非流式 JSON 均支持
   - 支持顶层 system 字段、content 数组（text / tool_use / tool_result）、thinking.budget_tokens 映射、max_tokens
3) 模型列表: GET /v1/models，返回 OpenAI 格式的模型数组
4) 健康检查: GET /health

## 接入要求
- 如果「${tool}」支持 OpenAI 兼容协议：把 base_url 设为 http://127.0.0.1:${port}/v1，模型设为 ${model}
- 如果「${tool}」只支持 Anthropic 协议：把 base_url 设为 http://127.0.0.1:${port}，模型设为 ${model}
- 如果两者都不支持：直接回复「不支持」
- 给出具体的配置位置（界面字段 / 配置文件 / 环境变量），以及如何验证（例如调用 GET /v1/models 或发一条测试消息）
- 配置完成后，说明如何切换回原服务，方便随时恢复`;
}

export function ToolsView() {
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [model, setModel] = useState("deepseek/deepseek-v4-flash");
  const [toolName, setToolName] = useState("");
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    api.proxyStatus().then(setStatus).catch(() => {});
    const off = onStatus(setStatus);
    api
      .modelsGet(false)
      .then((r) => {
        setModels(r.data);
        if (r.data.length > 0 && !r.data.some((m) => m.id === model)) {
          setModel(r.data[0].id);
        }
      })
      .catch(() => {});
    return () => {
      off.then((f) => f());
    };
  }, []);

  const port = status?.port ?? 3050;
  const prompt = useMemo(() => buildPrompt(port, model, toolName), [port, model, toolName]);

  async function copyPrompt() {
    if (await copyText(prompt)) {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    }
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
            <Select value={model} onValueChange={setModel}>
              <SelectTrigger className="font-mono text-xs">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {models.map((m) => (
                  <SelectItem key={m.id} value={m.id} className="font-mono text-xs">
                    {m.id}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center text-sm">
            接入提示词
            <span className="ml-2 rounded-md border border-border bg-muted/50 px-2 py-0.5 font-mono text-[10px] text-muted-foreground">
              端口 {port} · {model}
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

    </div>
  );
}
