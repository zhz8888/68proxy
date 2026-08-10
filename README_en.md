<p align="center">
  <img src="./assets/readme/hero.svg" width="88%" alt="68PROXY — Command Code Protocol Gateway">
</p>

<p align="center">
  <a href="./README.md">🌐 中文</a>
</p>

<p align="center">
  <img src="https://img.shields.io/github/stars/evanfu0110/68proxy?style=flat-square&label=Stars&color=4B6BFB" alt="GitHub Stars">
  <img src="https://img.shields.io/github/v/release/evanfu0110/68proxy?style=flat-square&label=Release&color=2E9E6B" alt="Latest Release">
  <img src="https://img.shields.io/github/license/evanfu0110/68proxy?style=flat-square&label=License&color=E85642" alt="License">
  <img src="https://img.shields.io/badge/Platform-Windows-64748B?style=flat-square" alt="Platform">
</p>

**68PROXY** is a plug-and-play local **reverse proxy + protocol gateway**: a relay between your clients and Command Code that rewrites requests into the CC CLI envelope format and forwards them upstream, while exposing OpenAI / Anthropic compatible endpoints — so Cursor, OpenCode, Cherry Studio and your own tools can connect directly without any SDK adaptation. Your API key is stored in plaintext in a local config file — configure once, reuse everywhere.

---

<p align="center">
  <img src="./assets/readme/section-preview.svg" width="100%" alt="Preview">
</p>

| Page | Preview |
|------|---------|
| 📊 **Console** | ![Console](Preview%20Photo/1.png) |
| 🛰️ **Relay Records** | ![Relay Records](Preview%20Photo/2.png) |
| 📦 **Models** | ![Models](Preview%20Photo/3.png) |
| ⚙️ **Settings** | ![Settings](Preview%20Photo/4.png) |

> Placeholders — they will be replaced with real screenshots on release.

<p align="center">
  <img src="./assets/readme/section-features.svg" width="100%" alt="Features">
</p>

| Module | Description |
|--------|-------------|
| 📊 **Console** | Running state, listening port and upstream version at a glance; a live relay track visualizes the whole 「client → proxy → upstream」path with one-click start / stop / restart and health check |
| 🛰️ **Relay Records** | Every request since proxy start rendered in real time: time, model, path, status and elapsed time; click for full details (request ID, streaming mode, token usage and last event) |
| 📦 **Models** | Models dynamically fetched from the Provider API (falls back to 30 built-in models on failure), with provider badges, search and one-click copy of model IDs |
| 🔌 **Tool Integration** | Enter a target tool name and model to auto-generate an integration prompt — let AI wire up Cursor / OpenCode / Cherry Studio for you; it replies 「not supported」 when the protocol is incompatible |
| 🐞 **Debug Logs** | In-memory ring buffer with live push, level filter, keyword search, auto-scroll and one-click clear, plus export of the latest 1,000 entries |
| ⚙️ **Settings** | Port / listen address (with port-in-use detection and one-click release), model source and refresh interval, startup behavior (auto-run, autostart, tray), log level, plaintext API key storage — changes saved automatically |
| 🎛️ **System Tray** | Minimize to tray; tray menu shows the window and start / stop / restart the proxy or quit |

<p align="center">
  <img src="./assets/readme/section-flow.svg" width="100%" alt="Request Flow">
</p>

1. **Compatible entry** — Exposes OpenAI `/v1/chat/completions` and Anthropic `/v1/messages` compatible endpoints, plus `/v1/models` and `/health`
2. **Protocol conversion** — Wraps requests into the Command Code CLI envelope format: extracts system prompts, maps multi-turn messages, tool calls, multimodal images and tool_choice
3. **Upstream forwarding** — Forwards to `/alpha/generate` with anti-detection signals (per-key sessions & device fingerprints, traceparent, fake project slug, dynamic CC version)
4. **Streaming translation** — Translates the upstream NDJSON stream into OpenAI / Anthropic SSE or non-stream JSON, handling error-code mapping, timeouts, disconnects and zero-output edge cases

<p align="center">
  <img src="./assets/readme/section-quickstart.svg" width="100%" alt="Quick Start">
</p>

**Requirements:** Node.js ≥ 20, pnpm, and a Rust toolchain (for the Tauri backend).

```bash
# Install dependencies
pnpm install

# Frontend dev mode (Vite)
pnpm dev

# Desktop dev mode (Tauri)
pnpm tauri dev

# Build desktop installers
pnpm tauri build
```

**Quick connect:** after starting the proxy, configure any OpenAI / Anthropic compatible client:

```
OpenAI-compatible Base URL   http://127.0.0.1:3050/v1
Anthropic Base URL           http://127.0.0.1:3050
Model                        deepseek/deepseek-v4-flash etc. (see Models)
API Key                      any placeholder (e.g. sk-placeholder) — the proxy
                             reuses the real key saved on this machine;
                             a user_-prefixed key is also accepted (header wins)
```

> The API key is stored in plaintext in the local config file (`config.json`) — visible only on this machine; don't share the config file with others.

<p align="center">
  <img src="./assets/readme/section-tech.svg" width="100%" alt="Tech Stack">
</p>

| Frontend | Backend | Tools |
|----------|---------|-------|
| Tauri 2 | Rust (axum + tokio + reqwest) | tauri-cli |
| React 19 + TypeScript | Local config file (config.json) | Windows |
| Vite + Tailwind CSS 4 | serde / uuid / rand / sha2 | shadcn/ui |
| Radix + lucide-react + sonner | tower-http + CORS | |

<p align="center">
  <img src="./assets/readme/section-structure.svg" width="100%" alt="Project Structure">
</p>

```
68proxy/
├── src/                      # React frontend
│   ├── components/           # UI components (StatusLamp / RelayRail / UrlRow / ModelLogo …)
│   ├── views/                # Pages (Console / Logs / Relay / Models / Tools / Config / About)
│   └── lib/                  # Tauri API bridge & helpers
├── src-tauri/
│   ├── src/
│   │   ├── lib.rs            # Tauri commands, system tray, lifecycle
│   │   ├── credentials.rs    # Plaintext API key access (local config file)
│   │   └── proxy/            # Rust reverse proxy core
│   │       ├── server.rs     # axum routes, streaming / non-streaming forwarding
│   │       ├── convert.rs    # OpenAI ↔ CC, Anthropic ↔ OpenAI conversion
│   │       ├── cc_client.rs  # CC upstream client, sessions / fingerprint, model fetch
│   │       ├── sse.rs        # NDJSON → SSE translators
│   │       ├── fingerprint.rs# Anti-detection device fingerprint
│   │       ├── config.rs     # Config load / validate / env overrides
│   │       └── ...
│   ├── icons/                # App icons
│   └── tauri.conf.json       # Tauri config
├── assets/readme/            # README decoration assets (SVG)
├── Preview Photo/            # UI preview screenshots
└── tools/icon-render/        # Icon rendering tool
```

<p align="center">
  <img src="./assets/readme/section-build.svg" width="100%" alt="Build">
</p>

```bash
pnpm tauri build
```

Two artifacts are produced (in `src-tauri/target/release/`):

| Version | File | Notes |
|---------|------|-------|
| 🖥️ **Installer** | `bundle/nsis/68proxy_<version>_x64-setup.exe` | NSIS setup with desktop shortcut, Start Menu entry and uninstaller — recommended for daily use |
| 📦 **Portable** | `68proxy.exe` | No install needed, double-click to run — great for on-the-go use |

> Both versions are shipped on GitHub Releases, each verified by sha256.

<p align="center">
  <img src="./assets/readme/section-thanks.svg" width="100%" alt="Acknowledgments">
</p>

- [Command Code](https://commandcode.ai) — Upstream API provider
- [Tauri](https://tauri.app) — Desktop app framework
- [axum](https://github.com/tokio-rs/axum) — Rust web framework

<p align="center">
  <img src="./assets/readme/section-contact.svg" width="100%" alt="Contact">
</p>

- GitHub: [evanfu0110](https://github.com/evanfu0110)
- Website: [www.110.wtf](https://www.110.wtf)
- Email: [1771005798@qq.com](mailto:1771005798@qq.com)
- Telegram: [@Z6ix8ightBot](https://t.me/Z6ix8ightBot)

<p align="center">
  <img src="./assets/readme/section-license.svg" width="100%" alt="License">
</p>

[MIT](LICENSE) © 6ix8ight
