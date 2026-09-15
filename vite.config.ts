// Vite 构建配置：React + Tailwind 插件、@ 路径别名，以及 Tauri 开发服务器相关设置
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";
// @ts-expect-error type error without @types/node package
import process from "node:process";

// Tauri 真机/远程开发时的宿主机地址（TAURI_DEV_HOST），本地开发时为空
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      // 将 @ 指向 src 目录，与 tsconfig 中的 paths 保持一致
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },

  // 把桥接端口/令牌透传给前端：后端读 CC_DEV_BRIDGE_PORT / CC_DEV_BRIDGE_TOKEN，
  // 前端只能读到 VITE_ 前缀的变量，若两处各写各的，改了后端端口前端就会失联。
  define: {
    "import.meta.env.VITE_CC_DEV_BRIDGE_PORT": JSON.stringify(
      process.env.CC_DEV_BRIDGE_PORT ?? "",
    ),
    "import.meta.env.VITE_CC_DEV_BRIDGE_TOKEN": JSON.stringify(
      process.env.CC_DEV_BRIDGE_TOKEN ?? "",
    ),
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  clearScreen: false,
  server: {
    // 固定 1420 端口供 Tauri 窗口加载；忽略 src-tauri 的文件变更以免误触发热更新
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
}));
