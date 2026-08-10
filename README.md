<p align="center">
  <img src="./assets/readme/hero.svg" width="88%" alt="68PROXY — Command Code 协议转换网关">
</p>

<p align="center">
  <a href="./README_en.md">🌐 English</a>
</p>

<p align="center">
  <img src="https://img.shields.io/github/stars/evanfu0110/68proxy?style=flat-square&label=Stars&color=4B6BFB" alt="GitHub Stars">
  <img src="https://img.shields.io/github/v/release/evanfu0110/68proxy?style=flat-square&label=Release&color=2E9E6B" alt="Latest Release">
  <img src="https://img.shields.io/github/license/evanfu0110/68proxy?style=flat-square&label=License&color=E85642" alt="License">
  <img src="https://img.shields.io/badge/Platform-Windows-64748B?style=flat-square" alt="Platform">
</p>

**68PROXY** 是一款开箱即用的本地**反向代理 + 协议转换网关**：作为客户端与 Command Code 之间的一层中转，它把请求改写成 CC CLI 信封格式并代理至上游，同时对外暴露 OpenAI / Anthropic 兼容接口——让 Cursor、OpenCode、Cherry Studio 以及自研工具无需任何 SDK 适配即可直接接入。API Key 明文保存在本地配置文件中，一次配置、全局复用。

---

<p align="center">
  <img src="./assets/readme/section-preview.svg" width="100%" alt="界面预览 Preview">
</p>

| 页面 | 截图 |
|------|------|
| 📊 **控制台** | ![Console](Preview%20Photo/1.png) |
| 🛰️ **中继记录** | ![Relay Records](Preview%20Photo/2.png) |
| 📦 **模型列表** | ![Models](Preview%20Photo/3.png) |
| ⚙️ **配置** | ![Settings](Preview%20Photo/4.png) |

<p align="center">
  <img src="./assets/readme/section-features.svg" width="100%" alt="功能 Features">
</p>

| 模块 | 说明 |
|------|------|
| 📊 **控制台** | 运行状态、监听端口、上游版本一目了然；实时中继轨道可视化「客户端 → 代理 → 上游」整条链路，一键启动 / 停止 / 重启与健康检查 |
| 🛰️ **中继记录** | 自代理启动以来每次请求实时呈现：时间、模型、路径、状态与耗时；点击可查看完整详情（请求 ID、流式模式、Token 用量与最后事件） |
| 📦 **模型列表** | 从 Provider API 动态拉取模型（失败自动回退内置 30 个模型），展示厂商标识，支持搜索与一键复制模型 ID |
| 🔌 **工具接入** | 输入目标工具名与模型，自动生成接入提示词，让 AI 替你完成 Cursor / OpenCode / Cherry Studio 等工具的配置；协议不支持时自动回复「不支持」 |
| 🐞 **调试日志** | 内存环形日志 + 实时推送，支持级别过滤、关键词搜索、自动滚动与一键清空，可导出最近 1000 条 |
| ⚙️ **配置** | 端口 / 监听地址（含端口占用检测与一键释放）、模型来源与刷新间隔、启动行为（自动运行 / 开机自启 / 托盘）、日志级别、API Key 明文存储，改动自动保存 |
| 🎛️ **系统托盘** | 最小化到托盘运行，托盘菜单可显示窗口、启动 / 停止 / 重启代理与退出 |

<p align="center">
  <img src="./assets/readme/section-flow.svg" width="100%" alt="请求流程 Request Flow">
</p>

1. **兼容入口** — 对外暴露 OpenAI `/v1/chat/completions` 与 Anthropic `/v1/messages` 兼容端点，同时提供 `/v1/models` 模型列表与 `/health` 健康检查
2. **协议转换** — 将请求包装成 Command Code CLI 信封格式：提取 system 提示、映射多轮消息、工具调用、多模态图片与 tool_choice 等参数
3. **上游转发** — 携带反检测特征（每 Key 独立会话与设备指纹、traceparent、假项目 slug、动态 CC 版本）转发至 `/alpha/generate`
4. **流式翻译** — 把上游 NDJSON 流实时翻译为 OpenAI / Anthropic 的 SSE 或非流式 JSON，并处理错误码映射、超时、断连与零输出等边界情况

<p align="center">
  <img src="./assets/readme/section-quickstart.svg" width="100%" alt="快速开始 Quick Start">
</p>

**环境要求：** Node.js ≥ 20、pnpm、Rust 工具链（构建 Tauri 后端）。

```bash
# 安装依赖
pnpm install

# 前端开发模式（Vite）
pnpm dev

# 桌面开发模式（Tauri）
pnpm tauri dev

# 构建桌面安装包
pnpm tauri build
```

**快速接入**：启动代理后，在任意 OpenAI / Anthropic 兼容客户端中配置：

```
OpenAI 兼容 Base URL   http://127.0.0.1:3050/v1
Anthropic Base URL     http://127.0.0.1:3050
模型                   deepseek/deepseek-v4-flash 等（见「模型列表」）
API Key                任意占位符即可（如 sk-placeholder），
                       代理会自动使用本机已保存的真实 Key；也可传 user_ 开头
                       的 Key（请求头优先）
```

> API Key 明文保存在本地配置文件（`config.json`）中，仅本机可见；请勿将配置文件分享给他人。

<p align="center">
  <img src="./assets/readme/section-tech.svg" width="100%" alt="技术栈 Tech Stack">
</p>

| 前端 | 后端 | 工具 |
|------|------|------|
| Tauri 2 | Rust（axum + tokio + reqwest） | tauri-cli |
| React 19 + TypeScript | 本地配置文件（config.json） | Windows |
| Vite + Tailwind CSS 4 | serde / uuid / rand / sha2 | shadcn/ui |
| Radix + lucide-react + sonner | tower-http + CORS | |

<p align="center">
  <img src="./assets/readme/section-structure.svg" width="100%" alt="项目结构 Project Structure">
</p>

```
68proxy/
├── src/                      # React 前端
│   ├── components/           # UI 组件（StatusLamp / RelayRail / UrlRow / ModelLogo …）
│   ├── views/                # 页面（控制台 / 调试日志 / 中继记录 / 模型列表 / 工具接入 / 配置 / 关于）
│   └── lib/                  # Tauri API 桥接与工具函数
├── src-tauri/
│   ├── src/
│   │   ├── lib.rs            # Tauri 命令、系统托盘、生命周期
│   │   ├── credentials.rs    # API Key 明文存取（本地配置文件）
│   │   └── proxy/            # Rust 反向代理核心
│   │       ├── server.rs     # axum 路由，流式 / 非流式转发
│   │       ├── convert.rs    # OpenAI ↔ CC、Anthropic ↔ OpenAI 协议转换
│   │       ├── cc_client.rs  # CC 上游客户端、会话 / 指纹、模型拉取
│   │       ├── sse.rs        # NDJSON → SSE 翻译器
│   │       ├── fingerprint.rs# 反检测设备指纹
│   │       ├── config.rs     # 配置加载 / 校验 / 环境变量覆写
│   │       └── ...
│   ├── icons/                # 应用图标
│   └── tauri.conf.json       # Tauri 配置
├── assets/readme/            # README 装饰资源（SVG）
├── Preview Photo/            # 界面预览截图
└── tools/icon-render/        # 图标渲染工具
```

<p align="center">
  <img src="./assets/readme/section-build.svg" width="100%" alt="构建 Build">
</p>

```bash
pnpm tauri build
```

输出两种版本（`src-tauri/target/release/`）：

| 版本 | 文件 | 说明 |
|------|------|------|
| 🖥️ **安装版** | `bundle/nsis/68proxy_<version>_x64-setup.exe` | NSIS 安装程序，含桌面快捷方式、开始菜单、卸载入口，适合日常使用 |
| 📦 **便携版** | `68proxy.exe` | 免安装，双击即用，适合移动/绿色使用 |

> GitHub Releases 同时提供两种版本，均校验 sha256。

<p align="center">
  <img src="./assets/readme/section-thanks.svg" width="100%" alt="致谢 Acknowledgments">
</p>

- [Command Code](https://commandcode.ai) — 上游 API 提供商
- [Tauri](https://tauri.app) — 桌面应用框架
- [axum](https://github.com/tokio-rs/axum) — Rust Web 框架

<p align="center">
  <img src="./assets/readme/section-contact.svg" width="100%" alt="联系 Contact">
</p>

- GitHub：[evanfu0110](https://github.com/evanfu0110)
- 网站：[www.110.wtf](https://www.110.wtf)
- 邮箱：[1771005798@qq.com](mailto:1771005798@qq.com)
- Telegram：[@Z6ix8ightBot](https://t.me/Z6ix8ightBot)

<p align="center">
  <img src="./assets/readme/section-license.svg" width="100%" alt="许可 License">
</p>

[MIT](LICENSE) © 6ix8ight
