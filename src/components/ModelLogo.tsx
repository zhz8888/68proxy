import { memo } from "react";
import type { ComponentType } from "react";
import DeepSeek from "@lobehub/icons/es/DeepSeek";
import Claude from "@lobehub/icons/es/Claude";
import Gemini from "@lobehub/icons/es/Gemini";
import Grok from "@lobehub/icons/es/Grok";
import Hunyuan from "@lobehub/icons/es/Hunyuan";
import LongCat from "@lobehub/icons/es/LongCat";
import Meta from "@lobehub/icons/es/Meta";
import Minimax from "@lobehub/icons/es/Minimax";
import Moonshot from "@lobehub/icons/es/Moonshot";
import Nvidia from "@lobehub/icons/es/Nvidia";
import OpenAI from "@lobehub/icons/es/OpenAI";
import Poolside from "@lobehub/icons/es/Poolside";
import Qwen from "@lobehub/icons/es/Qwen";
import Stepfun from "@lobehub/icons/es/Stepfun";
import Tencent from "@lobehub/icons/es/Tencent";
import XiaomiMiMo from "@lobehub/icons/es/XiaomiMiMo";
import ZAI from "@lobehub/icons/es/ZAI";
import { Bot } from "lucide-react";

import { translate } from "@/i18n";
import { cn } from "@/lib/utils";

/** 品牌头像组件：各品牌 Avatar 的公共签名（size 必填，与包内类型一致）。 */
type BrandAvatar = ComponentType<{ size: number }>;

/** 需要按 type 变体着色的品牌：OpenAI 的 Avatar 支持 gpt3/gpt4/gpt5/o1/oss/platform。 */
function openAiVariant(type: "gpt3" | "gpt4" | "gpt5" | "o1" | "oss" | "platform"): BrandAvatar {
  return ({ size }) => <OpenAI.Avatar size={size} type={type} />;
}

/** 品牌图标表：模型名关键字 → 品牌头像。
 *
 *  刻意逐品牌深导入（`@lobehub/icons/es/<Brand>`）而不用包根的 `ModelIcon` /
 *  `ProviderIcon`：那两个组件的映射表静态引入了全部约两千个品牌图标，无法摇树，
 *  单这一项就给产物增加约 3.4 MB（触发 Vite 的大 chunk 告警）。此处只保留这些
 *  品牌，关键字与变体照搬包内映射表，故任意模型名的归属与原先一致。
 *
 *  数组顺序即优先级：先命中的品牌胜出，与包内映射表的相对次序保持一致。*/
const BRANDS: Array<{ Avatar: BrandAvatar; keywords: RegExp[] }> = [
  { Avatar: openAiVariant("gpt3"), keywords: [/gpt-3/i] },
  { Avatar: openAiVariant("gpt4"), keywords: [/gpt-4/i] },
  { Avatar: openAiVariant("gpt5"), keywords: [/gpt-5/i] },
  { Avatar: openAiVariant("oss"), keywords: [/gpt-oss/i] },
  {
    Avatar: openAiVariant("o1"),
    keywords: [/o1-/i, /^o1/i, /\/o1/i, /o3-/i, /^o3/i, /\/o3/i, /o4-/i, /^o4/i, /\/o4/i],
  },
  {
    Avatar: openAiVariant("platform"),
    keywords: [
      /text-embedding-/i,
      /tts-/i,
      /whisper-/i,
      /codex/i,
      /davinci/i,
      /babbage/i,
      /omni-moderation/i,
      /text-moderation/i,
      /text-adb/i,
      /text-ada/i,
      /computer-use/i,
    ],
  },
  { Avatar: OpenAI.Avatar, keywords: [/^gpt-/i, /\/gpt-/i, /openai/i] },
  {
    Avatar: ZAI.Avatar,
    keywords: [/^glm-5/i, /\/glm-5/i, /\/glm5/i, /-glm-4/i, /^glm-4/i, /\/glm-4/i, /\/glm4/i, /-glm-5/i],
  },
  { Avatar: Claude.Avatar, keywords: [/claude/i] },
  {
    Avatar: Nvidia.Avatar,
    keywords: [/nemotron/i, /openreasoning/i, /nemoretriever/i, /neva-/i, /nv-/i],
  },
  { Avatar: Meta.Avatar, keywords: [/llama/i, /\/l3/i] },
  { Avatar: Gemini.Avatar, keywords: [/gemini/i] },
  { Avatar: Moonshot.Avatar, keywords: [/kimi/i, /moonshot/i] },
  {
    Avatar: Qwen.Avatar,
    keywords: [/qwen/i, /qwq/i, /qvq/i, /wanx/i, /wan\d\//i, /wan\d\.\d-/i, /tongyi/i, /gte-rerank/i],
  },
  { Avatar: Minimax.Avatar, keywords: [/minimax/i, /abab/i, /^image-/i] },
  { Avatar: Stepfun.Avatar, keywords: [/step/i] },
  { Avatar: Hunyuan.Avatar, keywords: [/hunyuan/i, /hy3/i] },
  { Avatar: Grok.Avatar, keywords: [/^grok-/i, /\/grok-/i] },
  { Avatar: Meta.Avatar, keywords: [/(^|\/)muse-spark($|-)/i] },
  { Avatar: DeepSeek.Avatar, keywords: [/deepseek/i] },
  { Avatar: LongCat.Avatar, keywords: [/longcat/i] },
  { Avatar: XiaomiMiMo.Avatar, keywords: [/^mimo-/i, /\/mimo-/i] },
];

/** 供应商标识 → 品牌头像；键为 providerForModel 的返回值。 */
const PROVIDER_ICONS: Record<string, BrandAvatar> = {
  deepseek: DeepSeek.Avatar,
  anthropic: Claude.Avatar,
  openai: OpenAI.Avatar,
  moonshot: Moonshot.Avatar,
  zhipu: ZAI.Avatar,
  minimax: Minimax.Avatar,
  qwen: Qwen.Avatar,
  stepfun: Stepfun.Avatar,
  xiaomimimo: XiaomiMiMo.Avatar,
  gemini: Gemini.Avatar,
  tencent: Tencent.Avatar,
  hunyuan: Hunyuan.Avatar,
};

/** 模型 ID 前缀 → 供应商标识的映射表，用于挑选对应的品牌图标。 */
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

/** 品牌匹配结果缓存：同一模型 ID 的判定在整个进程内恒定。 */
const brandCache = new Map<string, (typeof BRANDS)[number] | null>();

/** 按关键字挑出模型所属品牌；无匹配时返回 null。 */
function matchBrand(model: string): (typeof BRANDS)[number] | null {
  const cached = brandCache.get(model);
  if (cached !== undefined) return cached;
  const m = model.toLowerCase();
  const hit = BRANDS.find((b) => b.keywords.some((re) => re.test(m))) ?? null;
  brandCache.set(model, hit);
  return hit;
}

/** 模型品牌图标：依次尝试 Poolside 专属图标、品牌图标、供应商标志，均不匹配时回退为通用机器人图标。 */
export const ModelLogo = memo(function ModelLogo({
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
  const brand = matchBrand(model);
  // 是否识别出品牌：决定外框虚线样式与 title 提示文案
  const known = provider !== null || isPoolside || brand !== null;
  const ProviderAvatar = provider ? PROVIDER_ICONS[provider] : undefined;
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center justify-center rounded-md bg-secondary/60 p-1",
        !known && "border border-dashed border-border",
        className,
      )}
      style={{ width: size + 8, height: size + 8 }}
      title={known ? model : translate("modelLogo.noBrandIcon", { p0: model })}
    >
      {isPoolside ? (
        <Poolside size={size} />
      ) : brand ? (
        <brand.Avatar size={size} />
      ) : ProviderAvatar ? (
        <ProviderAvatar size={size} />
      ) : (
        <Bot className="text-muted-foreground" style={{ width: size, height: size }} />
      )}
    </span>
  );
});

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
  const Avatar = PROVIDER_ICONS[provider];
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center justify-center rounded-md bg-secondary/60 p-1",
        className,
      )}
      style={{ width: size + 8, height: size + 8 }}
      title={provider}
    >
      {Avatar ? (
        <Avatar size={size} />
      ) : (
        <Bot className="text-muted-foreground" style={{ width: size, height: size }} />
      )}
    </span>
  );
}
