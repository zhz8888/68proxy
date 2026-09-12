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
  model_refresh_interval_secs: number;
  auto_start_proxy: boolean;
  show_window_on_start: boolean;
  autostart: boolean;
  close_to_tray: boolean;
  usage_enabled: boolean;
  usage_retention_days: number;
  /** CC 上游账户 key 列表（user_ 开头），请求按轮询切换使用。 */
  cc_accounts: string[];
  /** 本地转发鉴权 key（sk- 开头，仅本机服务鉴权用，不发给 CC 上游）。 */
  local_api_key: string;
  /** 无 system prompt 时发空格占位（阻止上游注入默认提示词）。 */
  empty_system_placeholder: boolean;
  /** ZDR 模式：向 CC 上游发送 x-cmd-zdr: 1 请求头。 */
  zdr: boolean;
  /** 请求体大小上限（MB），超限请求返回 413。 */
  max_body_mb: number;
  /** 下游写缓冲背压僵死看门狗（毫秒），0 表示禁用。 */
  client_drain_timeout_ms: number;
  /** 进程内在途请求上限，0 表示不限。 */
  max_inflight: number;
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

/** 本地转发 Key 的存储状态：是否已生成及掩码后的展示文本。 */
export interface ApiKeyState {
  has_key: boolean;
  masked: string;
}

/** CC 账户列表条目：掩码 key、userId（唯一标识）、显示名、来源与下标。 */
export interface AccountEntry {
  index: number;
  masked: string;
  userId: string;
  userName: string;
  source: string; // "oauth" | "manual"
}

/** 浏览器授权登录的轮询结果。 */
export interface AuthLoginPoll {
  status: "idle" | "pending" | "success" | "denied" | "failed";
  port?: number;
  account?: { key: string; userId: string; userName: string };
  error?: string;
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

/** 最近 10 分钟的一个分钟桶（数组按时间从旧到新排列，index 0 最早）。 */
export interface UsageMinuteBucket {
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  cost: number;
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
  last_10_minutes: UsageMinuteBucket[];
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

/** 模型能力标记（文本输入 / 视觉 / 思考）。 */
export interface ModelCaps {
  text: boolean;
  vision: boolean;
  reasoning: boolean;
}

/** 单档费率（$/1M tokens）。 */
export interface ModelRates {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}

/** 价格档位：maxContext 为该档输入 token 上限，null 表示最高档无上限。 */
export interface ModelTier {
  maxContext: number | null;
  rates: ModelRates;
}

/** 闲时/忙时费率信息（仅 DeepSeek 系列）。 */
export interface ModelTimeOfDay {
  peak: ModelRates;
  windows: string;
}

/** 促销信息：discountPercent 为折扣百分比，free 表示限时免费。 */
export interface ModelDeal {
  discountPercent: number;
  free: boolean;
  expires?: string;
  endsWhen?: string;
}

/** 内置计费表中的单个模型（含能力、分档价格、折扣与闲忙时）。 */
export interface ModelPricing {
  id: string;
  name: string;
  category: string;
  provider?: string;
  contextWindow?: number;
  caps: ModelCaps;
  deprecated?: boolean;
  deal?: ModelDeal;
  timeOfDay?: ModelTimeOfDay;
  tiers: ModelTier[];
}

/** 单个模型在当前套餐下的准入结果。 */
export interface ModelAccessInfo {
  allowed: boolean;
  minimum_plan?: string | null;
  reason?: string | null;
}

/** 当前 CC 账户的套餐上下文。 */
export interface PlanContext {
  plan_id: string | null;
  plan_name: string;
  purchased_credits: number;
  free_credits: number;
  fetch_failed: boolean;
  note: string;
}

/** 套餐状态：套餐信息 + 各模型准入结果（键为模型 ID）。 */
export interface PlanStatus {
  plan: PlanContext;
  access: Record<string, ModelAccessInfo>;
}

/** 限额窗口（5 小时 / 周）：used 为已用量，cap 为上限，resetAt 为重置时间（毫秒）。 */
export interface LimitWindow {
  used: number;
  cap: number;
  reset_at: number | null;
}

/** 组织级消费限额行。 */
export interface OrgLimit {
  label: string;
  pct: number;
  reached: boolean;
}

/** 单个账户的额度快照（来自上游 whoami/subscriptions/credits/summary）。 */
export interface AccountQuota {
  user_name: string;
  masked_key: string;
  plan_id: string | null;
  plan_name: string;
  status: string | null;
  monthly_remaining: number;
  purchased_remaining: number;
  free_remaining: number;
  total_remaining: number;
  total_pool: number;
  total_spent: number;
  usage_percent: number;
  has_billing: boolean;
  days_left: number | null;
  period_start: number | null;
  period_end: number | null;
  five_hour: LimitWindow | null;
  weekly: LimitWindow | null;
  org_limits: OrgLimit[];
  error: string | null;
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
  // 本地转发 Key：读取（掩码）/ 保存 / 随机生成 / 删除
  localKeyGet: () => invoke<ApiKeyState>("local_key_get"),
  localKeySet: (key: string) => invoke<void>("local_key_set", { key }),
  localKeyGenerate: () => invoke<{ key: string; masked: string }>("local_key_generate"),
  localKeyDelete: () => invoke<void>("local_key_delete"),
  // CC 账户：列表 / 新增（whoami 验证补全，可选自定义显示名）/ 重命名 / 按下标删除
  accountList: () => invoke<{ accounts: AccountEntry[] }>("account_list"),
  accountAdd: (key: string, userName?: string) =>
    invoke<void>("account_add", { key, userName }),
  accountRename: (userId: string, userName: string) =>
    invoke<void>("account_rename", { userId, userName }),
  accountRemove: (index: number) => invoke<void>("account_remove", { index }),
  // 浏览器授权登录：启动（返回授权 URL）/ 轮询结果 / 取消
  authLoginStart: () => invoke<{ url: string; port: number }>("auth_login_start"),
  authLoginPoll: () => invoke<AuthLoginPoll>("auth_login_poll"),
  authLoginCancel: () => invoke<void>("auth_login_cancel"),
  // 模型列表：force 为 true 时忽略缓存强制向上游拉取；fallback 表示是否使用兜底列表
  modelsGet: (force = false) =>
    invoke<{ data: ModelInfo[]; fallback: boolean }>("models_get", { force }),
  // 内置模型计费表（能力、分档价格、折扣、免费与闲忙时）
  modelsCatalog: () => invoke<ModelPricing[]>("models_catalog"),
  // 当前账户套餐信息与各模型准入结果（force 时强制刷新上游缓存）
  planStatus: (force = false) => invoke<PlanStatus>("plan_status", { force }),
  // 全部账户的额度快照 / 指定账户的额度快照（账户详情）
  accountsQuota: () => invoke<AccountQuota[]>("accounts_quota"),
  accountQuota: (userId: string) => invoke<AccountQuota>("account_quota", { userId }),
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
