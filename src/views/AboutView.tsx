import { Github, Globe, Mail, ServerCog, ShieldCheck, Workflow } from "lucide-react";
import type { IconType } from "react-icons";
import { FaFile } from "react-icons/fa6";
import { SiReact, SiRust, SiTauri, SiTypescript, SiVite } from "react-icons/si";
import { toast } from "sonner";
import { openUrl } from "@tauri-apps/plugin-opener";

import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";

const LOGIC_STEPS = [
  {
    title: "兼容入口",
    desc: "对外暴露 OpenAI /v1/chat/completions 与 Anthropic /v1/messages 兼容端点，同时提供 /v1/models 模型列表与 /health 健康检查。",
  },
  {
    title: "协议转换",
    desc: "将请求包成 Command Code CLI 信封格式：提取 system 提示、映射多轮消息、工具调用、多模态图片与 tool_choice 等参数。",
  },
  {
    title: "上游转发",
    desc: "携带反检测特征（每 Key 独立会话与设备指纹、traceparent、假项目 slug、动态 CC 版本）转发到 /alpha/generate。",
  },
  {
    title: "流式翻译",
    desc: "把上游 NDJSON 流实时翻译为 OpenAI / Anthropic 的 SSE 或非流式 JSON，并处理错误码映射、超时、断连与零输出等边界情况。",
  },
];

const TECH_STACK: Array<{ icons: IconType[]; name: string; desc: string }> = [
  { icons: [SiTauri], name: "Tauri 2", desc: "桌面应用壳：Rust 后端 + 系统 WebView，体积小、资源占用低" },
  { icons: [SiReact, SiTypescript, SiVite], name: "React 19 + TypeScript + Vite", desc: "前端界面：shadcn/ui 组件 + Tailwind CSS 4" },
  { icons: [SiRust], name: "Rust (axum + tokio + reqwest)", desc: "本地反向代理服务：高并发流式转发与协议转换" },
  { icons: [FaFile], name: "本地配置文件", desc: "API Key 明文保存在本地配置文件（config.json）中，方便迁移与备份" },
];

async function openLink(url: string) {
  try {
    await openUrl(url);
  } catch (e) {
    toast.error(String(e));
  }
}

const LINKS: Array<{ icon: typeof Github; label: string; value: string; href: string }> = [
  { icon: Github, label: "GitHub", value: "evanfu0110", href: "https://github.com/evanfu0110" },
  { icon: Globe, label: "网站", value: "www.110.wtf", href: "https://www.110.wtf" },
  { icon: Mail, label: "邮箱", value: "1771005798@qq.com", href: "mailto:1771005798@qq.com" },
];

export function AboutView() {
  return (
    <div className="space-y-4 pb-8">
      <Card>
        <CardContent className="pt-5">
          <p className="font-display text-xl font-semibold tracking-wide text-foreground">
            68PROXY
          </p>
          <p className="text-sm leading-6 text-muted-foreground">
            一款开箱即用的本地反向代理桌面工具：把 Command Code 的私有 API 包装成
            OpenAI / Anthropic 兼容接口，让 Cursor、OpenCode、Cherry Studio 以及自研工具
            无需任何 SDK 适配即可直接接入。API Key 明文保存在本地配置文件中，一次配置、全局复用。
          </p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-sm">
            <Workflow className="h-4 w-4" />
            实现逻辑
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 pt-0">
          {LOGIC_STEPS.map((step, i) => (
            <div key={step.title} className="flex gap-3">
              <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-foreground font-mono text-[10px] font-bold text-background">
                {i + 1}
              </span>
              <div className="space-y-0.5">
                <p className="text-sm font-medium text-foreground">{step.title}</p>
                <p className="text-[13px] leading-5 text-muted-foreground">{step.desc}</p>
              </div>
            </div>
          ))}
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-sm">
            <ServerCog className="h-4 w-4" />
            技术栈
          </CardTitle>
        </CardHeader>
        <CardContent className="grid grid-cols-2 gap-3 pt-0">
          {TECH_STACK.map((t) => (
            <div key={t.name} className="rounded-lg border border-border bg-muted/30 p-3">
              <div className="flex items-center gap-2">
                <span className="flex shrink-0 items-center gap-1.5">
                  {t.icons.map((Icon, i) => (
                    <Icon key={i} className="h-4 w-4 shrink-0 text-muted-foreground" />
                  ))}
                </span>
                <p className="font-mono text-[12px] font-semibold text-foreground">{t.name}</p>
              </div>
              <p className="mt-1 text-[12px] leading-5 text-muted-foreground">{t.desc}</p>
            </div>
          ))}
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-sm">
            <ShieldCheck className="h-4 w-4" />
            作者与联系方式
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 pt-0">
          <p className="text-sm leading-6 text-muted-foreground">
            68proxy 由 <span className="font-semibold text-foreground">6ix8ight</span> 开发维护，
            欢迎 Star、提 Issue 或邮件交流：
          </p>
          <Separator />
          <div className="flex flex-col gap-2">
            {LINKS.map((l) => {
              const Icon = l.icon;
              return (
                <button
                  key={l.label}
                  onClick={() => openLink(l.href)}
                  className="group flex items-center gap-3 rounded-md border border-border bg-muted/30 px-3 py-2 text-left transition-colors hover:bg-secondary"
                >
                  <Icon className="h-4 w-4 text-muted-foreground group-hover:text-foreground" />
                  <span className="w-14 shrink-0 text-xs text-muted-foreground">{l.label}</span>
                  <span className="min-w-0 truncate font-mono text-[13px] text-foreground">{l.value}</span>
                </button>
              );
            })}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
