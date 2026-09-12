<p align="center">
  <img src="./assets/readme/hero.svg" width="88%" alt="68PROXY — Command Code Protocol Gateway">
</p>

<p align="center">
  <a href="./README.md">🌐 中文</a>
</p>

<p align="center">
  <img src="https://img.shields.io/github/stars/zhz8888/68proxy?style=flat-square&label=Stars&color=4B6BFB" alt="GitHub Stars">
  <img src="https://img.shields.io/github/v/release/zhz8888/68proxy?style=flat-square&label=Release&color=2E9E6B" alt="Latest Release">
  <img src="https://img.shields.io/github/license/zhz8888/68proxy?style=flat-square&label=License&color=E85642" alt="License">
  <img src="https://img.shields.io/badge/Platform-Windows%20%7C%20macOS-64748B?style=flat-square" alt="Platform">
</p>

> This repository is a personal fork of [evanfu0110/68proxy](https://github.com/evanfu0110/68proxy), actively maintained and enhanced on top of the original.

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

> Other pages (📈 Usage Stats, 🐞 Debug Logs, 🔌 Tool Integration, 👤 Accounts, ℹ️ About) are described in the feature table below.

<p align="center">
  <img src="./assets/readme/section-features.svg" width="100%" alt="Features">
</p>

| Module | Description |
|--------|-------------|
| 📊 **Console** | Running state, listening port and upstream version at a glance; a live relay track visualizes the whole 「client → proxy → upstream」path with one-click start / stop / restart and health check |
| 🛰️ **Relay Records** | Every request since proxy start rendered in real time: time, model, path, status and elapsed time; click for full details (request ID, streaming mode, token usage and last event) |
| 📈 **Usage Stats** | SQLite-persisted token usage stats: summary cards (requests / input / cached / output / estimated cost), an hourly-daily aggregated trend area chart, a last-10-minutes mini bar chart, per-model / per-endpoint breakdown and recent request list; supports Today / 24h / 7D / 30D / 60D / All ranges and one-click clear |
| 📦 **Models** | Models dynamically fetched from the Provider API (falls back to 30 built-in models on failure), with provider badges, search and one-click copy of model IDs |
| 🔌 **Tool Integration** | Enter a target tool name and model to auto-generate an integration prompt — let AI wire up Cursor / OpenCode / Cherry Studio for you; it replies 「not supported」 when the protocol is incompatible, and also offers a removal prompt |
| 🐞 **Debug Logs** | In-memory ring buffer with live push, level filter, keyword search, auto-scroll and one-click clear, plus export of the latest 1,000 entries |
| ⚙️ **Settings** | Port / listen address (with port-in-use detection and one-click release), model source and refresh interval, startup behavior (auto-run, autostart, tray), log level, token usage tracking toggle and retention days, local forwarding Key (`sk-` prefixed, one-click random generation) — changes saved automatically; plus two CC upstream behavior toggles: "empty system placeholder" and "ZDR mode" |
| 👤 **Accounts** | Manage Command Code upstream accounts (`user_`, multiple supported, requests rotated round-robin): add via browser OAuth or by pasting a Key, rename, remove, and view a single account's full quota (plan, monthly / purchased / free balance, 5-hour and weekly limits) |
| 🎛️ **System Tray** | Minimize to tray; tray menu shows the window and start / stop / restart the proxy or quit |

<p align="center">
  <img src="./assets/readme/section-flow.svg" width="100%" alt="Request Flow">
</p>

1. **Compatible entry** — Exposes OpenAI `/v1/chat/completions`, OpenAI Responses `/v1/responses` and Anthropic `/v1/messages` compatible endpoints, plus `/v1/models` and `/health`
2. **Protocol conversion** — Wraps requests into the Command Code CLI envelope format: extracts system prompts, maps multi-turn messages, tool calls, multimodal images and tool_choice; Responses `instructions`, `input` items and `function_call` / `function_call_output` round-tripping are converted as well
3. **Upstream forwarding** — Forwards to `/alpha/generate` with anti-detection signals (per-key sessions & device fingerprints, traceparent, fake project slug, dynamic CC version)
4. **Streaming translation** — Translates the upstream NDJSON stream into OpenAI / Responses / Anthropic SSE or non-stream JSON, handling error-code mapping, timeouts, disconnects and zero-output edge cases
5. **Usage tracking** — On completion, token usage is recorded to SQLite on both the streaming-finished and non-streaming success paths (zero-output or failed requests are skipped), cost is estimated from the built-in price table, and per-day pre-aggregation powers fast queries over large time windows

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

**Quick connect:** before starting the proxy, generate a local forwarding Key (`sk-` prefixed) under **Settings**, and add at least one CC account Key (`user_` prefixed) on the **Accounts** page. Then start the proxy and configure any OpenAI / Anthropic compatible client:

```
OpenAI-compatible Base URL   http://127.0.0.1:3050/v1
Anthropic Base URL           http://127.0.0.1:3050
Model                        deepseek/deepseek-v4-flash etc. (see Models)
API Key                      the local forwarding Key (sk- prefixed, see
                             Settings); multiple clients can share it
```

> The local forwarding Key (`sk-`) is only used for local proxy auth and is never sent to the CC upstream. CC account Keys (`user_`) are managed on the Accounts page; you can add several and requests are rotated across them round-robin. Both are stored in plaintext in the local config file (`config.json`) — visible only on this machine; don't share the config file with others.

<p align="center">
  <img src="./assets/readme/section-tech.svg" width="100%" alt="Tech Stack">
</p>

| Frontend | Backend | Tools |
|----------|---------|-------|
| Tauri 2 | Rust (axum + tokio + reqwest + rusqlite) | tauri-cli |
| React 19 + TypeScript | Local config file (config.json) + usage DB (usage.sqlite) | Windows / macOS |
| Vite + Tailwind CSS 4 | serde / uuid / rand / sha2 | shadcn/ui |
| Radix + lucide-react + sonner | tower-http + CORS | |

<p align="center">
  <img src="./assets/readme/section-structure.svg" width="100%" alt="Project Structure">
</p>

```
68proxy/
├── src/                      # React frontend
│   ├── components/           # UI components (StatusLamp / RelayRail / UrlRow / ModelLogo / UsageTrendChart / UsageMiniBars …)
│   ├── views/                # Pages (Console / Logs / Relay / Stats / Models / Tools / Accounts / Settings / About)
│   └── lib/                  # Tauri API bridge (api.ts), status mapping (status.ts), constants (constants.ts), formatting (format.ts)
├── src-tauri/
│   ├── src/
│   │   ├── lib.rs            # Tauri commands, system tray, lifecycle
│   │   ├── credentials.rs    # Plaintext API key access (local config file)
│   │   └── proxy/            # Rust reverse proxy core
│   │       ├── server.rs     # axum routes, streaming / non-streaming forwarding
│   │       ├── convert.rs    # OpenAI / Responses / Anthropic ↔ CC conversion
│   │       ├── cc_client.rs  # CC upstream client, sessions / fingerprint, model fetch
│   │       ├── sse.rs        # NDJSON → SSE translators
│   │       ├── usage.rs      # Token usage stats (SQLite 3 tables + per-day pre-aggregation)
│   │       ├── pricing.rs    # Model price table & cost estimation
│   │       ├── fingerprint.rs# Anti-detection device fingerprint
│   │       ├── config.rs     # Config load / validate / env overrides
│   │       ├── errors.rs     # Upstream error → downstream protocol mapping
│   │       ├── log.rs        # In-memory ring buffer logs
│   │       ├── state.rs      # Shared state (sessions / caches / request queue)
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
# Windows (NSIS installer)
pnpm tauri build --bundles nsis

# macOS (Apple Silicon arm64 dmg)
pnpm tauri build --bundles dmg --target aarch64-apple-darwin
```

Artifacts produced:

| Platform | Version | File | Notes |
|----------|---------|------|-------|
| 🖥️ Windows | **Installer** | `target/release/bundle/nsis/68proxy_<version>_x64-setup.exe` | NSIS setup with desktop shortcut, Start Menu entry and uninstaller — recommended for daily use |
| 🖥️ Windows | **Portable** | `68proxy.exe` | No install needed, double-click to run — great for on-the-go use |
| 🍎 macOS | **dmg image** | `target/aarch64-apple-darwin/release/bundle/dmg/68proxy_<version>_aarch64.dmg` | Apple Silicon (arm64) dmg; not Apple-notarized (see first-launch note below) |

> Both Windows versions and the macOS dmg are shipped on GitHub Releases, each verified by sha256.

> **First launch on macOS**: the app is not signed or notarized by Apple, so Gatekeeper may block it on first launch. Right-click the app in Finder → Applications → **Open** to bypass, or remove the quarantine attribute in Terminal:
>
> ```bash
> sudo xattr -r -d com.apple.quarantine /Applications/68proxy.app
> ```

<p align="center">
  <img src="./assets/readme/section-thanks.svg" width="100%" alt="Acknowledgments">
</p>

- [Command Code](https://commandcode.ai) — Upstream API provider
- [Tauri](https://tauri.app) — Desktop app framework
- [axum](https://github.com/tokio-rs/axum) — Rust web framework

<p align="center">
  <img src="./assets/readme/section-license.svg" width="100%" alt="License">
</p>

This repository is a personal fork of [evanfu0110/68proxy](https://github.com/evanfu0110/68proxy), licensed under the upstream [MIT](LICENSE) license. Upstream copyright belongs to 6ix8ight; fork maintenance and enhancements are © zhz8888.

[MIT](LICENSE) © 6ix8ight · fork © zhz8888
