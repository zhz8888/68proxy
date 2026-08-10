import { ModelIcon, Poolside, ProviderIcon, modelMappings } from "@lobehub/icons";
import { Bot } from "lucide-react";
import { cn } from "@/lib/utils";

const PROVIDER_BY_PREFIX: Array<[RegExp, string]> = [
  [/^deepseek\//, "deepseek"],
  [/^claude-/, "anthropic"],
  [/^gpt-/, "openai"],
  [/^moonshotai\//, "moonshot"],
  [/^zai-org\//, "zhipu"],
  [/^MiniMaxAI\//, "minimax"],
  [/^Qwen\//, "qwen"],
  [/^stepfun\//, "stepfun"],
  [/^xiaomi\//, "xiaomimimo"],
  [/^google\//, "gemini"],
  [/^tencent\//, "tencent"],
  [/^hunyuan\//, "hunyuan"],
];

export function providerForModel(id: string): string | null {
  for (const [re, provider] of PROVIDER_BY_PREFIX) {
    if (re.test(id)) return provider;
  }
  return null;
}

/** 与 @lobehub/icons ModelIcon 内部相同的映射判断：是否有对应模型/品牌图标 */
function hasLobeMapping(model: string): boolean {
  const m = model.toLowerCase();
  return modelMappings.some((item) =>
    item.keywords.some((keyword) => new RegExp(keyword, "i").test(m)),
  );
}

export function ModelLogo({
  model,
  size = 20,
  className,
}: {
  model: string;
  size?: number;
  className?: string;
}) {
  const provider = providerForModel(model);
  const isPoolside = /^poolside\//i.test(model);
  const lobeMapped = hasLobeMapping(model);
  const known = provider !== null || isPoolside || lobeMapped;
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center justify-center rounded-md bg-secondary/60 p-1",
        !known && "border border-dashed border-border",
        className,
      )}
      style={{ width: size + 8, height: size + 8 }}
      title={known ? model : `${model}（暂无品牌图标）`}
    >
      {isPoolside ? (
        <Poolside size={size} />
      ) : lobeMapped ? (
        <ModelIcon model={model} size={size} />
      ) : provider ? (
        <ProviderIcon provider={provider} size={size} />
      ) : (
        <Bot className="text-muted-foreground" style={{ width: size, height: size }} />
      )}
    </span>
  );
}

export function ProviderLogo({
  provider,
  size = 20,
  className,
}: {
  provider: string;
  size?: number;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center justify-center rounded-md bg-secondary/60 p-1",
        className,
      )}
      style={{ width: size + 8, height: size + 8 }}
      title={provider}
    >
      <ProviderIcon provider={provider} size={size} />
    </span>
  );
}
