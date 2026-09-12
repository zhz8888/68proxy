// shadcn/ui 基础组件：按钮（Button），支持多种变体与尺寸
import * as React from "react";
import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

// 按钮的样式变体定义（cva）
const buttonVariants = cva(
  "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0",
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground shadow hover:bg-primary/90",
        // 危险操作（删除/清空/停止/结束进程）统一红色强调：--destructive 已是真正的红，
        // 文字色随明暗自翻转（浅色白字 / 深色黑字），与 --primary 同一逻辑。
        destructive: "bg-destructive text-destructive-foreground shadow-sm hover:bg-destructive/90",
        // 无底红字：用于列表行内等轻量删除操作，保留 ghost 的轻量感但以红色提示破坏性
        "destructive-ghost": "text-destructive hover:bg-destructive/10 hover:text-destructive",
        outline: "border border-input bg-transparent shadow-sm hover:bg-accent hover:text-accent-foreground",
        secondary: "bg-secondary text-secondary-foreground shadow-sm hover:bg-secondary/80",
        ghost: "hover:bg-accent hover:text-accent-foreground",
        link: "text-primary underline-offset-4 hover:underline",
      },
      size: {
        default: "h-9 px-4 py-2",
        sm: "h-8 rounded-md px-3 text-xs",
        lg: "h-10 rounded-md px-6",
        icon: "h-9 w-9",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  },
);

/** 按钮属性：原生 button 属性 + variant/size 变体 + asChild。 */
export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {
  asChild?: boolean;
}

/** 基础按钮，通过 cva 支持多种变体与尺寸；asChild 时经 Slot 把样式渲染到子元素上。 */
const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, asChild = false, ...props }, ref) => {
    const Comp = asChild ? Slot : "button";
    return (
      <Comp
        className={cn(buttonVariants({ variant, size, className }))}
        ref={ref}
        {...props}
      />
    );
  },
);
Button.displayName = "Button";

export { Button, buttonVariants };
