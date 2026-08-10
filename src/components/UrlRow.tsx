import { useState } from "react";
import { Check, Copy } from "lucide-react";

import { Button } from "@/components/ui/button";
import { copyText } from "@/lib/format";
import { cn } from "@/lib/utils";

export function UrlRow({
  url,
  label,
  className,
}: {
  url: string;
  label?: string;
  className?: string;
}) {
  const [copied, setCopied] = useState(false);

  async function handleCopy() {
    if (await copyText(url)) {
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    }
  }

  return (
    <div className={cn("flex items-center gap-2", className)}>
      {label && (
        <span className="text-xs text-muted-foreground whitespace-nowrap">{label}</span>
      )}
      <code className="select-text flex-1 truncate rounded-md border border-border bg-muted/50 px-2.5 py-1.5 font-mono text-[12.5px] text-foreground">
        {url}
      </code>
      <Button variant="ghost" size="icon" className="h-7 w-7" onClick={handleCopy} title="复制地址">
        {copied ? <Check className="text-success" /> : <Copy />}
      </Button>
    </div>
  );
}
