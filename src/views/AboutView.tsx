import { Github, ServerCog, ShieldCheck, Workflow } from "lucide-react";
import { Trans, useTranslation } from "react-i18next";
import type { IconType } from "react-icons";
import { FaFile } from "react-icons/fa6";
import { SiReact, SiRust, SiTauri, SiTypescript, SiVite } from "react-icons/si";
import { toast } from "sonner";

import { translate } from "@/i18n";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import { errText } from "@/lib/messages";
import { openExternal } from "@/lib/platform";

// 代理实现逻辑的四个步骤，按顺序展示；标题与说明由组件内的 t() 提供
const LOGIC_STEPS = [
  { titleKey: "about.logic.step1Title", descKey: "about.logic.step1Desc" },
  { titleKey: "about.logic.step2Title", descKey: "about.logic.step2Desc" },
  { titleKey: "about.logic.step3Title", descKey: "about.logic.step3Desc" },
  { titleKey: "about.logic.step4Title", descKey: "about.logic.step4Desc" },
];

// 技术栈条目：图标、名称与简介；技术栈名保持原文，仅名称/简介中的自然语言走 t()
const TECH_STACK: Array<{
  icons: IconType[];
  name?: string;
  nameKey?: string;
  descKey: string;
}> = [
  { icons: [SiTauri], name: "Tauri 2", descKey: "about.tech.tauriDesc" },
  { icons: [SiReact, SiTypescript, SiVite], name: "React 19 + TypeScript + Vite", descKey: "about.tech.reactDesc" },
  { icons: [SiRust], name: "Rust (axum + tokio + reqwest)", descKey: "about.tech.rustDesc" },
  { icons: [FaFile], nameKey: "about.tech.localConfigName", descKey: "about.tech.localConfigDesc" },
];

/** 用系统默认浏览器打开外部链接，失败时弹出错误提示。 */
async function openLink(url: string) {
  try {
    await openExternal(url);
  } catch (e) {
    toast.error(errText(e));
  }
}

// 作者署名与本 fork 仓库链接；label 由组件内的 t() 提供
const LINKS: Array<{ icon: typeof Github; labelKey: string; value: string; href: string }> = [
  { icon: Github, labelKey: "about.originalAuthor", value: "6ix8ight", href: "https://github.com/evanfu0110" },
  { icon: Github, labelKey: "about.forkMaintainer", value: "zhz8888", href: "https://github.com/zhz8888/68proxy" },
];

/** 关于视图：项目简介、实现逻辑、技术栈与作者联系方式。 */
export function AboutView() {
  const { t } = useTranslation();

  return (
    <div className="space-y-4 pb-8">
      <Card>
        <CardContent className="pt-5">
          <p className="font-display text-xl font-semibold tracking-wide text-foreground">
            68PROXY
          </p>
          <p className="text-sm leading-6 text-muted-foreground">{t("about.intro")}</p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-sm">
            <Workflow className="h-4 w-4" />
            {t("about.logicTitle")}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 pt-0">
          {LOGIC_STEPS.map((step, i) => (
            <div key={step.titleKey} className="flex gap-3">
              <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-foreground font-mono text-2xs font-bold text-background">
                {i + 1}
              </span>
              <div className="space-y-0.5">
                <p className="text-sm font-medium text-foreground">{translate(step.titleKey)}</p>
                <p className="text-sm leading-5 text-muted-foreground">{translate(step.descKey)}</p>
              </div>
            </div>
          ))}
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-sm">
            <ServerCog className="h-4 w-4" />
            {t("about.techTitle")}
          </CardTitle>
        </CardHeader>
        <CardContent className="grid grid-cols-2 gap-3 pt-0">
          {TECH_STACK.map((tech) => (
            <div key={tech.name ?? tech.nameKey} className="rounded-lg border border-border bg-muted/30 p-3">
              <div className="flex items-center gap-2">
                <span className="flex shrink-0 items-center gap-1.5">
                  {tech.icons.map((Icon, i) => (
                    <Icon key={i} className="h-4 w-4 shrink-0 text-muted-foreground" />
                  ))}
                </span>
                <p className="font-mono text-xs font-semibold text-foreground">
                  {tech.nameKey ? translate(tech.nameKey) : tech.name}
                </p>
              </div>
              <p className="mt-1 text-xs leading-5 text-muted-foreground">{translate(tech.descKey)}</p>
            </div>
          ))}
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-sm">
            <ShieldCheck className="h-4 w-4" />
            {t("about.authorTitle")}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 pt-0">
          <p className="text-sm leading-6 text-muted-foreground">
            <Trans
              i18nKey="about.attribution"
              components={{ b: <span className="font-semibold text-foreground" /> }}
            />
          </p>
          <Separator />
          <div className="flex flex-col gap-2">
            {LINKS.map((l) => {
              const Icon = l.icon;
              return (
                <button
                  key={l.labelKey}
                  onClick={() => openLink(l.href)}
                  className="group flex items-center gap-3 rounded-md border border-border bg-muted/30 px-3 py-2 text-left transition-colors hover:bg-secondary"
                >
                  <Icon className="h-4 w-4 text-muted-foreground group-hover:text-foreground" />
                  <span className="w-20 shrink-0 text-xs text-muted-foreground">{translate(l.labelKey)}</span>
                  <span className="min-w-0 truncate font-mono text-sm text-foreground">{l.value}</span>
                </button>
              );
            })}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
