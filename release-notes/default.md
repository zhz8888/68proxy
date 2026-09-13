本地**反向代理 + 协议转换网关**：把 Cursor、OpenCode、Cherry Studio、Codex CLI 等工具直接接入 Command Code，无需任何 SDK 适配。

## 📦 下载

| 平台 | 架构 | 下载 | 说明 |
| :--- | :--- | :--- | :--- |
| 🪟 **Windows** | x86_64 | [安装版](https://github.com/{{REPO}}/releases/download/{{TAG}}/68proxy-{{TAG}}-Windows-x64-setup.exe) · [绿色版](https://github.com/{{REPO}}/releases/download/{{TAG}}/68proxy-{{TAG}}-Windows-x64-Portable.zip) | 安装版含快捷方式与卸载项；绿色版解压即用，不写注册表 |
| 🍎 **macOS** | Apple Silicon (arm64) | [磁盘映像 dmg](https://github.com/{{REPO}}/releases/download/{{TAG}}/68proxy-{{TAG}}-macOS-aarch64.dmg) | 拖入「应用程序」即可 |

## ⚠️ 首次打开提示

应用尚未做代码签名与公证，两个平台首次运行都可能有系统拦截提示，按下述方式放行即可。

**macOS** — Gatekeeper 可能提示「无法验证开发者」，任选一种方式：

- 在「访达 → 应用程序」中右键（或按住 Control 单击）应用图标 →「打开」
- 若提示「已损坏」，先在终端移除隔离属性，再重新打开：

  ```bash
  sudo xattr -r -d com.apple.quarantine /Applications/68proxy.app
  ```

**Windows** — SmartScreen 可能提示「Windows 已保护你的电脑」，点击「更多信息」→「仍要运行」。

## ✅ 使用前提

- 需要一个 `user_` 开头的 Command Code API Key，在应用「账户」页授权登录或手动粘贴
- 对外兼容 OpenAI Chat Completions / Responses 与 Anthropic Messages 协议
- 使用说明与界面预览见 [README](https://github.com/{{REPO}}#readme) · [English](https://github.com/{{REPO}}/blob/main/README_en.md)
