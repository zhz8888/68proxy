import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Config {
  port: number;
  host: string;
  api_base: string;
  project_slug: string;
  log_file: string;
  log_level: string;
  use_provider_models: boolean;
  model_refresh_interval_ms: number;
  auto_start_proxy: boolean;
  show_window_on_start: boolean;
  autostart: boolean;
  close_to_tray: boolean;
}

export interface ProxyStatus {
  running: boolean;
  port: number;
  host: string;
  url: string;
  anthropic_url: string;
  cc_version: string;
  uptime_secs: number;
}

export interface LogEntry {
  seq: number;
  ts: number;
  level: string;
  msg: string;
}

export interface ModelInfo {
  id: string;
  name: string;
}

export interface RequestInfo {
  id: string;
  path: string;
  model: string;
  stream: boolean;
  status: string;
  started_at: number;
  elapsed_ms: number;
  input_tokens: number;
  output_tokens: number;
  cached_tokens: number;
  last_event: string;
}

export interface ApiKeyState {
  has_key: boolean;
  masked: string;
}

export const api = {
  proxyStart: () => invoke<ProxyStatus>("proxy_start"),
  proxyStop: () => invoke<ProxyStatus>("proxy_stop"),
  proxyRestart: () => invoke<ProxyStatus>("proxy_restart"),
  proxyStatus: () => invoke<ProxyStatus>("proxy_status"),
  configGet: () => invoke<Config>("config_get"),
  configSave: (config: Config) => invoke<{ needs_restart: boolean }>("config_save", { config }),
  apiKeyGet: () => invoke<ApiKeyState>("api_key_get"),
  apiKeySet: (key: string) => invoke<void>("api_key_set", { key }),
  apiKeyDelete: () => invoke<void>("api_key_delete"),
  modelsGet: (force = false) =>
    invoke<{ data: ModelInfo[]; fallback: boolean }>("models_get", { force }),
  logsGet: (limit = 200, afterSeq = 0) => invoke<LogEntry[]>("logs_get", { limit, afterSeq }),
  logsClear: () => invoke<void>("logs_clear"),
  logsExport: (path: string) => invoke<number>("logs_export", { path }),
  requestsGet: (limit = 20) => invoke<RequestInfo[]>("requests_get", { limit }),
  portCheck: (port: number) =>
    invoke<{ in_use: boolean; pid: number | null }>("port_check", { port }),
  portFree: (port: number) =>
    invoke<{ killed: number[]; message: string }>("port_free", { port }),
  autostartGet: () => invoke<boolean>("autostart_get"),
  autostartSet: (enabled: boolean) => invoke<void>("autostart_set", { enabled }),
};

export async function onLog(cb: (entry: LogEntry) => void) {
  return listen<LogEntry>("proxy://log", (e) => cb(e.payload));
}

export async function onStatus(cb: (status: ProxyStatus) => void) {
  return listen<ProxyStatus>("proxy://status", (e) => cb(e.payload));
}

export async function onRequest(cb: (info: RequestInfo) => void) {
  return listen<RequestInfo>("proxy://request", (e) => cb(e.payload));
}
