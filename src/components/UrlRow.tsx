import { useRef, useState } from "react";
import { Check, Copy } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import { copyText } from "@/lib/format";
import { cn } from "@/lib/utils";

/** 一行可复制的地址：左侧可选标签、中间等宽字体地址、右侧复制按钮。 */
export function UrlRow({
  url,
  label,
  className,
}: {
  url: string;
  label?: string;
  className?: string;
}) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  // 复位定时器句柄：重复点击时先清旧定时器，避免提示被提前清掉
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  /** 复制地址到剪贴板，并短暂显示“已复制”图标。 */
  async function handleCopy() {
    if (await copyText(url)) {
      setCopied(true);
      if (timer.current !== null) clearTimeout(timer.current);
      timer.current = setTimeout(() => {
        setCopied(false);
        timer.current = null;
      }, 1600);
    }
  }

  return (
    <div className={cn("flex items-center gap-2", className)}>
      {label && (
        <span className="text-xs text-muted-foreground whitespace-nowrap">{label}</span>
      )}
      <code className="select-text flex-1 truncate rounded-md border border-border bg-muted/50 px-2.5 py-1.5 font-mono text-xs text-foreground">
        {url}
      </code>
      <Button variant="ghost" size="icon" className="h-7 w-7" onClick={handleCopy} title={t("common.copyAddress")}>
        {copied ? <Check className="text-success" /> : <Copy />}
      </Button>
    </div>
  );
}
