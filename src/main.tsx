// 前端入口：加载 Space Grotesk / JetBrains Mono 字体与全局样式，将 App 挂载到 #root 节点
import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource/space-grotesk/500.css";
import "@fontsource/space-grotesk/600.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import "./index.css";
// 初始化 i18n（副作用导入）：须在渲染 App 前完成，保证首帧即为正确语言
import "@/i18n";
import App from "./App";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
