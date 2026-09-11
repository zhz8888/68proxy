// Tauri IPC 封装：以类型化的方式调用后端命令并订阅后端推送事件
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** 应用配置，字段与后端 config.json 及 Rust 侧 Config 结构一致。 */
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

/** 代理运行状态：是否运行、监听地址、OpenAI/Anthropic 接入 URL、上游版本与运行时长。 */
export interface ProxyStatus {
  running: boolean;
  port: number;
  host: string;
  url: string;
  anthropic_url: string;
  cc_version: string;
  uptime_secs: number;
}

/** 一条代理日志：seq 为自增序号，ts 为毫秒时间戳。 */
export interface LogEntry {
  seq: number;
  ts: number;
  level: string;
  msg: string;
}

/** 模型条目：id 为调用时使用的模型名，name 为展示名。 */
export interface ModelInfo {
  id: string;
  name: string;
}

/** 一次中继请求的摘要信息（路径、模型、状态、耗时与 token 用量等）。 */
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

/** API Key 的存储状态：是否已保存 Key 及掩码后的展示文本。 */
export interface ApiKeyState {
  has_key: boolean;
  masked: string;
}

/** 后端 Tauri 命令的类型化封装，前端所有 IPC 调用统一经由此对象。 */
export const api = {
  // 代理生命周期：启动 / 停止 / 重启 / 查询状态，均返回最新 ProxyStatus
  proxyStart: () => invoke<ProxyStatus>("proxy_start"),
  proxyStop: () => invoke<ProxyStatus>("proxy_stop"),
  proxyRestart: () => invoke<ProxyStatus>("proxy_restart"),
  proxyStatus: () => invoke<ProxyStatus>("proxy_status"),
  // 配置读写：保存时返回是否需要重启代理生效
  configGet: () => invoke<Config>("config_get"),
  configSave: (config: Config) => invoke<{ needs_restart: boolean }>("config_save", { config }),
  // API Key 管理：读取（掩码）/ 保存 / 删除
  apiKeyGet: () => invoke<ApiKeyState>("api_key_get"),
  apiKeySet: (key: string) => invoke<void>("api_key_set", { key }),
  apiKeyDelete: () => invoke<void>("api_key_delete"),
  // 模型列表：force 为 true 时忽略缓存强制向上游拉取；fallback 表示是否使用兜底列表
  modelsGet: (force = false) =>
    invoke<{ data: ModelInfo[]; fallback: boolean }>("models_get", { force }),
  // 日志：按序号增量拉取 / 清空 / 导出到文件（返回条数）
  logsGet: (limit = 200, afterSeq = 0) => invoke<LogEntry[]>("logs_get", { limit, afterSeq }),
  logsClear: () => invoke<void>("logs_clear"),
  logsExport: (path: string) => invoke<number>("logs_export", { path }),
  // 最近中继请求记录
  requestsGet: (limit = 20) => invoke<RequestInfo[]>("requests_get", { limit }),
  // 端口占用检查与释放（仅 Windows 支持结束占用进程）
  portCheck: (port: number) =>
    invoke<{ in_use: boolean; pid: number | null }>("port_check", { port }),
  portFree: (port: number) =>
    invoke<{ killed: number[]; message: string }>("port_free", { port }),
  // 开机自启开关
  autostartGet: () => invoke<boolean>("autostart_get"),
  autostartSet: (enabled: boolean) => invoke<void>("autostart_set", { enabled }),
};

/** 订阅后端日志推送事件，返回取消订阅函数。@param cb 每产生一条日志时回调 */
export async function onLog(cb: (entry: LogEntry) => void) {
  return listen<LogEntry>("proxy://log", (e) => cb(e.payload));
}

/** 订阅代理状态变更事件，返回取消订阅函数。@param cb 状态变化时回调最新状态 */
export async function onStatus(cb: (status: ProxyStatus) => void) {
  return listen<ProxyStatus>("proxy://status", (e) => cb(e.payload));
}

/** 订阅中继请求摘要事件，返回取消订阅函数。@param cb 每次请求更新时回调 */
export async function onRequest(cb: (info: RequestInfo) => void) {
  return listen<RequestInfo>("proxy://request", (e) => cb(e.payload));
}
