import { ModelIcon, Poolside, ProviderIcon, modelMappings } from "@lobehub/icons";
import { Bot } from "lucide-react";
import { cn } from "@/lib/utils";

// 模型 ID 前缀 → 供应商标识的映射表，用于挑选对应的品牌图标
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

/** 按模型 ID 前缀推断所属供应商。
 * @returns 供应商标识；无法识别时返回 null
 */
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

/** 模型品牌图标：依次尝试 Poolside 专属图标、lobehub 模型图标、供应商标志，均不匹配时回退为通用机器人图标。 */
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
  // 是否识别出品牌：决定外框虚线样式与 title 提示文案
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

/** 供应商品牌图标（按供应商标识渲染，不做起模型级匹配）。 */
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
