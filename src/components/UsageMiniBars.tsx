import { formatTokens } from "@/lib/format";
import type { UsageMinuteBucket } from "@/lib/api";

/**
 * 最近 10 分钟迷你柱状图：纯 SVG 手绘，每根柱代表一分钟。
 * 按输入+输出 token 总量画柱，高度归一化到桶内最大值；无数据时展示占位提示。
 */
export function UsageMiniBars({ buckets }: { buckets: UsageMinuteBucket[] }) {
  // 空数据（含全零）时展示占位
  const hasData = buckets.some(
    (b) => b.prompt_tokens + b.completion_tokens > 0 || b.requests > 0,
  );
  if (buckets.length === 0 || !hasData) {
    return (
      <div className="flex h-24 items-center justify-center text-xs text-muted-foreground">
        最近 10 分钟暂无请求
      </div>
    );
  }

  const W = 360;
  const H = 96;
  const PAD = { top: 8, right: 4, bottom: 16, left: 4 };
  const innerW = W - PAD.left - PAD.right;
  const innerH = H - PAD.top - PAD.bottom;
  const stepX = innerW / Math.max(buckets.length, 1);
  const barW = Math.max(stepX * 0.62, 4);

  const max = Math.max(...buckets.map((b) => b.prompt_tokens + b.completion_tokens), 1);

  // 每根柱：底部对齐的高度 + 顶部 tooltip（时间偏移 + 数值）
  const bars = buckets.map((b, i) => {
    const x = PAD.left + i * stepX + (stepX - barW) / 2;
    const v = b.prompt_tokens + b.completion_tokens;
    const h = Math.max((v / max) * innerH, v > 0 ? 2 : 0);
    const y = PAD.top + innerH - h;
    const minsAgo = buckets.length - 1 - i; // 数组按时间从旧到新排列：i=0 最旧，末尾最新
    const label =
      minsAgo === 0
        ? "刚刚"
        : minsAgo === 1
          ? "1 分钟前"
          : `${minsAgo} 分钟前`;
    return { x, y, h, v, label };
  });

  return (
    <div className="select-none">
      {/* 主题色经 currentColor 继承：柱体用信号信息色、基线用边框色，随明暗主题切换 */}
      <svg
        viewBox={`0 0 ${W} ${H}`}
        className="h-24 w-full text-signal-info"
        role="img"
        aria-label="最近 10 分钟用量柱状图"
      >
        {/* 基线 */}
        <line
          x1={PAD.left}
          x2={W - PAD.right}
          y1={PAD.top + innerH}
          y2={PAD.top + innerH}
          className="stroke-border"
          strokeWidth="1"
        />
        {bars.map((b, i) => (
          <rect
            key={i}
            x={b.x}
            y={b.y}
            width={barW}
            height={b.h}
            rx="1.5"
            fill={b.h > 0 ? "currentColor" : "transparent"}
          >
            <title>{`${b.label} · ${formatTokens(b.v)} tokens`}</title>
          </rect>
        ))}
        {/* X 轴标签：首、中、尾三个时刻 */}
        {[0, Math.floor((buckets.length - 1) / 2), buckets.length - 1].map((i, k) => (
          <text
            key={k}
            x={PAD.left + i * stepX + stepX / 2}
            y={H - 4}
            textAnchor="middle"
            className="fill-muted-foreground"
            fontSize="9"
          >
            {bars[i]?.label}
          </text>
        ))}
      </svg>
    </div>
  );
}
