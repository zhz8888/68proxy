import { useId } from "react";
import { useTranslation } from "react-i18next";

import type { UsageChartPoint } from "@/lib/api";
import { formatCompactNumber } from "@/lib/format";

/**
 * 成本模式的 Y 轴刻度文本：美元值不能取整后再格式化为紧凑数字，
 * 否则亚美元区间（如最大 $0.55）的刻度会全部四舍五入成 0，看起来像没有成本。
 * 按数值大小选择精度：≥$10 取整、≥$0.1 保留 2 位、更小保留 4 位。
 */
function formatCostTick(v: number): string {
  if (v === 0) return "$0";
  if (v >= 10) return `$${Math.round(v)}`;
  if (v >= 0.1) return `$${v.toFixed(2)}`;
  return `$${v.toFixed(4)}`;
}

/**
 * token 用量趋势图：纯 SVG 手绘面积图（不引入图表库，保持项目零依赖极简风格）。
 * 支持 tokens / cost 双模式切换；tokens 模式画输入+输出叠加面积，cost 模式画成本线。
 */
export function UsageTrendChart({
  points,
  mode,
}: {
  points: UsageChartPoint[];
  mode: "tokens" | "cost";
}) {
  const { t } = useTranslation();
  const gradId = useId().replace(/:/g, "");
  // 空数据时展示占位提示
  if (points.length === 0 || points.every((p) => p.prompt_tokens + p.completion_tokens === 0)) {
    return (
      <div className="flex h-52 items-center justify-center text-sm text-muted-foreground">
        {t("common.noData")}
      </div>
    );
  }

  const W = 720;
  const H = 200;
  const PAD = { top: 12, right: 8, bottom: 24, left: 44 };
  const innerW = W - PAD.left - PAD.right;
  const innerH = H - PAD.top - PAD.bottom;

  // 取模式对应的值序列：tokens 为输入+输出合计，cost 为成本
  const values = points.map((p) =>
    mode === "tokens" ? p.prompt_tokens + p.completion_tokens : p.cost,
  );
  const max = Math.max(...values, 1) * 1.1;
  const stepX = innerW / Math.max(points.length - 1, 1);
  const y = (v: number) => PAD.top + innerH - (v / max) * innerH;

  // 面积路径：折线 + 底部闭合
  const line = points
    .map((_, i) => {
      const x = PAD.left + i * stepX;
      return `${i === 0 ? "M" : "L"}${x.toFixed(1)},${y(values[i]).toFixed(1)}`;
    })
    .join(" ");
  const area = `${line} L${(PAD.left + (points.length - 1) * stepX).toFixed(1)},${(
    PAD.top + innerH
  ).toFixed(1)} L${PAD.left},${(PAD.top + innerH).toFixed(1)} Z`;

  // Y 轴刻度：4 档
  const ticks = [0, 0.25, 0.5, 0.75, 1].map((t) => ({
    y: PAD.top + innerH - t * innerH,
    value: max * t,
  }));

  return (
    <div className="select-none">
      {/* 主题色经 currentColor 继承：折线/渐变/数据点描边统一用信号信息色，
          网格线用边框色，数据点填充用卡片底色，随明暗主题自动切换 */}
      <svg
        viewBox={`0 0 ${W} ${H}`}
        className="h-52 w-full text-signal-info"
        role="img"
        aria-label={t("usageTrendChart.chartLabel")}
      >
        <defs>
          <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="currentColor" stopOpacity="0.35" />
            <stop offset="100%" stopColor="currentColor" stopOpacity="0.02" />
          </linearGradient>
        </defs>
        {/* 网格线 + Y 轴标签 */}
        {ticks.map((t, i) => (
          <g key={i}>
            <line
              x1={PAD.left}
              x2={W - PAD.right}
              y1={t.y}
              y2={t.y}
              className="stroke-border"
              strokeWidth="1"
              strokeDasharray={i === 4 ? "" : "3 3"}
            />
            <text
              x={PAD.left - 6}
              y={t.y + 3}
              textAnchor="end"
              className="fill-muted-foreground"
              fontSize="10"
            >
              {mode === "tokens"
                ? formatCompactNumber(t.value)
                : formatCostTick(t.value)}
            </text>
          </g>
        ))}
        {/* 面积与折线 */}
        <path d={area} fill={`url(#${gradId})`} />
        <path d={line} fill="none" stroke="currentColor" strokeWidth="2" strokeLinejoin="round" />
        {/* 数据点 */}
        {points.map((p, i) => {
          const x = PAD.left + i * stepX;
          const hover = points.length <= 48; // 点数过多时不画点避免拥挤
          return hover ? (
            <circle
              key={i}
              cx={x}
              cy={y(values[i])}
              r="2.5"
              className="fill-card"
              stroke="currentColor"
              strokeWidth="1.5"
            >
              <title>{`${p.label} · ${
                mode === "tokens"
                  ? `${formatCompactNumber(p.prompt_tokens + p.completion_tokens)} tokens`
                  : `$${p.cost.toFixed(4)}`
              }`}</title>
            </circle>
          ) : null;
        })}
        {/* X 轴标签：首、中、尾三点 */}
        {[0, Math.floor((points.length - 1) / 2), points.length - 1].map((i, k) => (
          <text
            key={k}
            x={PAD.left + i * stepX}
            y={H - 6}
            textAnchor={i === 0 ? "start" : i === points.length - 1 ? "end" : "middle"}
            className="fill-muted-foreground"
            fontSize="10"
          >
            {points[i].label}
          </text>
        ))}
      </svg>
    </div>
  );
}
