// Tauri IPC 封装：以类型化的方式调用后端命令并订阅后端推送事件。
// 实际传输由 lib/ipc.ts 适配：Tauri 窗口内走官方 IPC，浏览器直连开发页面时走调试桥接。

import { invoke, listen } from "@/lib/ipc";

import type { Language } from "@/lib/language";
import type { ThemeMode } from "@/lib/theme";

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
  /** Command Code 上游账户 key 列表（user_ 开头），请求按轮询切换使用。 */
  cc_accounts: string[];
  /** 本地转发鉴权 key（sk- 开头，仅本机服务鉴权用，不发给 Command Code 上游）。 */
  local_api_key: string;
  /** 无 system prompt 时发空格占位（阻止上游注入默认提示词）。 */
  empty_system_placeholder: boolean;
  /** ZDR 模式：向 Command Code 上游发送 x-cmd-zdr: 1 请求头。 */
  zdr: boolean;
  /** 请求体大小上限（MB），超限请求返回 413。 */
  max_body_mb: number;
  /** 下游写缓冲背压僵死看门狗（毫秒），0 表示禁用。 */
  client_drain_timeout_ms: number;
  /** 进程内在途请求上限，0 表示不限。 */
  max_inflight: number;
  /** 界面主题：system（跟随系统）/ dark / light。 */
  theme: ThemeMode;
  /** 界面语言：zh（简体中文）/ en（英文）。 */
  language: Language;
  /** 出站代理模式：none（不走代理，默认）/ system（跟随系统环境变量）/ custom（自定义代理）。 */
  proxy_mode: string;
  /** 自定义代理类型：socks5 / http（仅 custom 模式生效）。 */
  proxy_type: string;
  /** 自定义代理主机（仅 custom 模式生效）。 */
  proxy_host: string;
  /** 自定义代理端口（仅 custom 模式生效）。 */
  proxy_port: number;
  /** 自定义代理认证用户名（可选，仅 custom 模式生效）。 */
  proxy_username: string;
  /** 自定义代理认证密码（可选，仅 custom 模式生效）。 */
  proxy_password: string;
  /** 信封 mode（/alpha/generate 的 mode 字段；agent | learning | …）。 */
  cli_mode: string;
  /** lifecycle metadata 的 mode（interactive | non-interactive，独立于 cli_mode）。 */
  cli_session_mode: string;
  /** 指纹盐：改值让所有账户换一台设备（成批换身份用）。 */
  fingerprint_salt: string;
  /** 伪造的项目目录（与 x-project-slug 同源；留空用内置默认）。 */
  device_project_dir: string;
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
  /** 最近一次启动代理的失败原因（成功启动后为空；未运行时由状态栏提示）。 */
  error?: string;
}

/** 一条代理日志：seq 为自增序号，ts 为毫秒时间戳。 */
export interface LogEntry {
  seq: number;
  ts: number;
  level: string;
  msg: string;
}

/** 模型列表条目（models 表）：id 为调用时使用的模型名，其余为展示与归属信息。 */
export interface ModelInfo {
  id: string;
  name: string;
  /** 厂商显示名；缺失时前端可按 ID 前缀推断。 */
  provider?: string;
  /** 上下文长度（token）。 */
  contextLength?: number;
  /** 能力标记；未收录模型可能缺失。 */
  caps?: ModelCaps;
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

/** Command Code 账户列表条目：掩码 key、userId（唯一标识）、显示名、来源与下标。 */
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

/** 打开浏览器授权页的结果：实际使用的浏览器与是否真正进入隐私模式。 */
export interface OpenBrowserOutcome {
  browser: "chrome" | "edge" | "firefox" | "safari" | "default";
  private: boolean;
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
  /** 忙时窗口，UTC 小时区间 [start, end)，如 [[1,4],[6,10]]。 */
  peakRanges?: Array<[number, number]>;
  /** 忙时窗口是否仅限周一至周五。 */
  weekdaysOnly?: boolean;
  /** 后端自带的英文窗口描述，仅在前端无法本地化时兜底。 */
  windows: string;
}

/** 促销信息：discountPercent 为折扣百分比，free 表示限时免费。 */
export interface ModelDeal {
  discountPercent: number;
  free: boolean;
  expires?: string;
  endsWhen?: string;
}

/** 模型价格条目（model_pricing 表）：促销、分档费率与闲/忙时，按模型 ID 与列表关联。 */
export interface ModelPricing {
  id: string;
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

/** 当前 Command Code 账户的套餐上下文。 */
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

/** 账户使用策略：轮询 / 优先消耗指定账户（含会话粘滞）。 */
export type AccountStrategy = "round_robin" | "priority";

/** 账户使用规则：策略与优先消耗的账户 userId（空表示自动取剩余额度最多者）。 */
export interface AccountRouting {
  strategy: AccountStrategy;
  preferred_account_id: string;
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
  // Command Code 账户：列表 / 新增（whoami 验证补全，可选自定义显示名）/ 重命名 / 按下标删除
  accountList: () => invoke<{ accounts: AccountEntry[] }>("account_list"),
  accountAdd: (key: string, userName?: string) =>
    invoke<void>("account_add", { key, userName }),
  accountRename: (userId: string, userName: string) =>
    invoke<void>("account_rename", { userId, userName }),
  accountRemove: (index: number) => invoke<void>("account_remove", { index }),
  // 浏览器授权登录：启动（返回授权 URL）/ 轮询结果 / 取消 / 打开授权页（支持隐私模式）
  authLoginStart: () => invoke<{ url: string; port: number }>("auth_login_start"),
  authLoginPoll: () => invoke<AuthLoginPoll>("auth_login_poll"),
  authLoginCancel: () => invoke<void>("auth_login_cancel"),
  authLoginOpenBrowser: (url: string, privateMode: boolean) =>
    invoke<OpenBrowserOutcome>("auth_login_open_browser", { url, private: privateMode }),
  // 模型列表：force 为 true 时忽略缓存强制向上游拉取；fallback 表示是否使用兜底列表
  modelsGet: (force = false) =>
    invoke<{ data: ModelInfo[]; fallback: boolean }>("models_get", { force }),
  // 当前生效的模型计费表（能力、分档价格、折扣、免费与闲忙时；数据存于 SQLite）
  modelsCatalog: () => invoke<ModelPricing[]>("models_catalog"),
  // 覆盖写入模型信息（按 ID UPSERT 落库并刷新内存表），用于数据更新
  modelsCatalogUpdate: (models: ModelPricing[], source?: string) =>
    invoke<{ updated: number }>("models_catalog_update", { models, source }),
  // 当前账户套餐信息与各模型准入结果（force 时强制刷新上游缓存）
  planStatus: (force = false) => invoke<PlanStatus>("plan_status", { force }),
  // 全部账户的额度快照 / 指定账户的额度快照（账户详情）
  accountsQuota: () => invoke<AccountQuota[]>("accounts_quota"),
  accountQuota: (userId: string) => invoke<AccountQuota>("account_quota", { userId }),
  // 账户使用规则：轮询 / 优先消耗指定账户（含会话粘滞）
  accountRoutingGet: () => invoke<AccountRouting>("account_routing_get"),
  accountRoutingSet: (strategy: AccountStrategy, preferredAccountId: string) =>
    invoke<void>("account_routing_set", { strategy, preferredAccountId }),
  // 清除会话→账户绑定（手动切换账户后强制所有会话重选）
  accountBindingsClear: () => invoke<{ cleared: number }>("account_bindings_clear"),
  // 应用版本号：开发模式（tauri dev）返回 dev，生产构建返回发版版本
  appVersion: () => invoke<string>("app_version"),
  // 界面主题：读取 / 保存（system / dark / light）
  themeGet: () => invoke<{ theme: ThemeMode }>("theme_get"),
  themeSet: (theme: ThemeMode) => invoke<void>("theme_set", { theme }),
  // 界面语言：读取 / 保存（zh / en）
  languageGet: () => invoke<{ language: Language }>("language_get"),
  languageSet: (language: Language) => invoke<void>("language_set", { language }),
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
