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
  usage_enabled: boolean;
  usage_retention_days: number;
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

/** 用量分组行（按模型/按端点），含请求数、各 token 列与估算成本。 */
export interface UsageGroupRow {
  key: string;
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  total_tokens: number;
  cost: number;
}

/** 最近请求用量明细行。 */
export interface UsageRecentRow {
  ts: number;
  model: string;
  endpoint: string;
  status: string;
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  cache_write_tokens: number;
  cost: number;
  elapsed_ms: number;
}

/** token 用量汇总统计（stats_get 返回）。 */
export interface UsageStats {
  total_requests: number;
  total_prompt_tokens: number;
  total_completion_tokens: number;
  total_cached_tokens: number;
  total_cost: number;
  by_model: UsageGroupRow[];
  by_endpoint: UsageGroupRow[];
  last_10_minutes: { requests: number; prompt_tokens: number; completion_tokens: number; cost: number }[];
  recent_requests: UsageRecentRow[];
}

/** 趋势图数据点：label 为桶标签（HH:00 或 MM-DD）。 */
export interface UsageChartPoint {
  label: string;
  prompt_tokens: number;
  completion_tokens: number;
  cost: number;
}

/** 用量统计时间范围，与后端 Period::parse 的取值一致。 */
export type UsagePeriod = "today" | "24h" | "7d" | "30d" | "60d" | "all";

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
  // token 用量统计：汇总 / 趋势图 / 最近明细 / 清空
  statsGet: (period: UsagePeriod = "all") =>
    invoke<UsageStats>("stats_get", { period }),
  statsChart: (period: UsagePeriod = "all") =>
    invoke<UsageChartPoint[]>("stats_chart", { period }),
  statsRecent: (limit = 20) => invoke<UsageRecentRow[]>("stats_recent", { limit }),
  statsClearAll: () => invoke<{ cleared: number }>("stats_clear_all"),
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

/** 订阅 token 用量更新事件，返回取消订阅函数。@param cb 用量变化时回调（节流后） */
export async function onStats(cb: (payload: { updated: number }) => void) {
  return listen<{ updated: number }>("proxy://stats", (e) => cb(e.payload));
}
