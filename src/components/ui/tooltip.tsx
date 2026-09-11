// shadcn/ui 基础组件：文字提示（Tooltip），基于 Radix Tooltip
import * as React from "react";
import * as TooltipPrimitive from "@radix-ui/react-tooltip";

import { cn } from "@/lib/utils";

/** 提示组件的上下文提供者，包裹在使用 Tooltip 的组件树外层。 */
const TooltipProvider = TooltipPrimitive.Provider;
/** 提示根组件，控制提示的开合（受控/非受控）。 */
const Tooltip = TooltipPrimitive.Root;
/** 触发提示的悬停/聚焦元素。 */
const TooltipTrigger = TooltipPrimitive.Trigger;

/** 提示气泡内容，渲染在传送门中，带淡入缩放动画。 */
const TooltipContent = React.forwardRef<
  React.ElementRef<typeof TooltipPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TooltipPrimitive.Content>
>(({ className, sideOffset = 4, ...props }, ref) => (
  <TooltipPrimitive.Portal>
    <TooltipPrimitive.Content
      ref={ref}
      sideOffset={sideOffset}
      className={cn(
        "z-50 overflow-hidden rounded-md bg-primary px-3 py-1.5 text-xs text-primary-foreground animate-in fade-in-0 zoom-in-95",
        className,
      )}
      {...props}
    />
  </TooltipPrimitive.Portal>
));
TooltipContent.displayName = TooltipPrimitive.Content.displayName;

export { Tooltip, TooltipTrigger, TooltipContent, TooltipProvider };
