// Tauri IPC 封装：以类型化的方式调用后端命令并订阅后端推送事件。
// 实际传输由 lib/ipc.ts 适配：Tauri 窗口内走官方 IPC，浏览器直连开发页面时走调试桥接。

import { invoke, listen } from "@/lib/ipc";

import type { Language } from "@/lib/language";
import type { ThemeMode } from "@/lib/theme";

/** 应用配置，字段与后端 config.json 及 Rust 侧 Config 结构一致。 */
export interface Config {
  /** 本地代理监听端口。 */
  port: number;
  /** 监听地址（IP，如 127.0.0.1 / 0.0.0.0）。 */
  host: string;
  /** Command Code 上游 API 根地址（含协议前缀）。 */
  api_base: string;
  /** 项目 slug（保留字段，实际转发用会话派生的伪造 slug）。 */
  project_slug: string;
  /** 日志文件路径，空字符串表示不写文件。 */
  log_file: string;
  /** 日志级别：debug / info / warn / error。 */
  log_level: string;
  /** 是否从 Provider API 动态拉取模型列表（关闭时用内置列表）。 */
  use_provider_models: boolean;
  /** 模型列表缓存刷新间隔（秒）。 */
  model_refresh_interval_secs: number;
  /** 应用启动后是否自动开启代理服务。 */
  auto_start_proxy: boolean;
  /** 启动时是否显示主窗口。 */
  show_window_on_start: boolean;
  /** 是否开机自启动（操作系统级登录项）。 */
  autostart: boolean;
  /** 关闭窗口时隐藏到托盘而不是退出。 */
  close_to_tray: boolean;
  /** 是否启用 token 用量统计（关闭后不再记录新用量）。 */
  usage_enabled: boolean;
  /** 用量明细保留天数，0 表示永久保留。 */
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
  /** 流式响应上游空闲超时（秒），0 表示不限（默认）。 */
  stream_idle_timeout_secs: number;
  /** 非流式响应上游空闲超时（秒），0 表示不限（默认）。 */
  nonstream_idle_timeout_secs: number;
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
  /** 上游 Command Code 版本号的本地缓存（后端刷新时维护，前端不编辑）。 */
  cc_version_cache: string;
}

/** 代理运行状态：是否运行、监听地址、OpenAI/Anthropic 接入 URL、上游版本与运行时长。 */
export interface ProxyStatus {
  /** 代理是否正在运行。 */
  running: boolean;
  /** 实际监听端口。 */
  port: number;
  /** 实际监听地址。 */
  host: string;
  /** OpenAI 兼容入口 URL。 */
  url: string;
  /** Anthropic 兼容入口 URL。 */
  anthropic_url: string;
  /** 当前模拟的上游 CLI 版本号。 */
  cc_version: string;
  /** 本次运行已持续秒数。 */
  uptime_secs: number;
  /** 最近一次启动代理的失败原因（成功启动后为空；未运行时由状态栏提示）。 */
  error?: string;
}

/** 一条代理日志：seq 为自增序号，ts 为毫秒时间戳。 */
export interface LogEntry {
  /** 全局自增序号，前端增量拉取的游标。 */
  seq: number;
  /** Unix 毫秒时间戳。 */
  ts: number;
  /** 日志级别：info / warn / error。 */
  level: string;
  /** 日志正文。 */
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
  /** 请求 ID（同时用作下游响应体的 completion_id / message_id / response_id）。 */
  id: string;
  /** 入口路径（/v1/chat/completions 等）。 */
  path: string;
  /** 请求的模型名。 */
  model: string;
  /** 是否流式请求。 */
  stream: boolean;
  /** 请求状态：streaming / ok / error / timeout / disconnect。 */
  status: string;
  /** 请求开始时间（Unix 毫秒）。 */
  started_at: number;
  /** 端到端耗时（毫秒），请求结束时回填。 */
  elapsed_ms: number;
  /** 上游回报的输入 token 数。 */
  input_tokens: number;
  /** 上游回报的输出 token 数。 */
  output_tokens: number;
  /** 命中缓存的输入 token 数。 */
  cached_tokens: number;
  /** 最后收到的上游事件类型（用于诊断中断位置）。 */
  last_event: string;
}

/** 本地转发 Key 的存储状态：是否已生成及掩码后的展示文本。 */
export interface ApiKeyState {
  /** 是否已生成本地转发 Key。 */
  has_key: boolean;
  /** 掩码后的展示文本（如 sk-abc…xyz）。 */
  masked: string;
}

/** Command Code 账户列表条目：掩码 key、userId（唯一标识）、显示名、来源与下标。 */
export interface AccountEntry {
  /** 账户在列表中的下标（删除按下标时使用，优先用 userId 删除）。 */
  index: number;
  /** 掩码后的上游 key（仅展示用，不可作匹配键）。 */
  masked: string;
  /** 账户唯一标识（whoami 的 user.id）。 */
  userId: string;
  /** 账户显示名（可自定义）。 */
  userName: string;
  /** 来源：oauth（浏览器授权）/ manual（手动粘贴）。 */
  source: string; // "oauth" | "manual"
}

/** 浏览器授权登录的轮询结果。 */
export interface AuthLoginPoll {
  /** 登录状态：idle（未开始）/ pending（等待授权）/ success / denied（用户拒绝）/ failed。 */
  status: "idle" | "pending" | "success" | "denied" | "failed";
  /** 本次登录的 loopback 回调端口（pending 时有效）。 */
  port?: number;
  /** 授权成功后捕获的账户凭据。 */
  account?: { key: string; userId: string; userName: string };
  /** 失败原因码（成功为 undefined，码表见 errors.*）。 */
  error?: string;
}

/** 打开浏览器授权页的结果：实际使用的浏览器与是否真正进入隐私模式。 */
export interface OpenBrowserOutcome {
  /** 实际打开的浏览器（default 表示系统默认）。 */
  browser: "chrome" | "edge" | "firefox" | "safari" | "default";
  /** 是否真正进入隐私模式（请求隐私但浏览器不支持时会回退为 false）。 */
  private: boolean;
}

/** 用量分组行（按模型/按端点），含请求数、各 token 列与估算成本。 */
export interface UsageGroupRow {
  /** 分组键：模型名或端点路径。 */
  key: string;
  /** 请求数。 */
  requests: number;
  /** 输入 token（含缓存命中与写入）。 */
  prompt_tokens: number;
  /** 输出 token。 */
  completion_tokens: number;
  /** 命中缓存的输入 token 数（prompt 子集）。 */
  cached_tokens: number;
  /** 输入+输出合计 token。 */
  total_tokens: number;
  /** 估算成本（美元）。 */
  cost: number;
}

/** 最近请求用量明细行。 */
export interface UsageRecentRow {
  /** 明细行 id（后端主键，同毫秒突发不再撞键）。 */
  id: number;
  /** 请求开始时间（Unix 毫秒）。 */
  ts: number;
  /** 请求的模型名。 */
  model: string;
  /** 入口端点（/v1/chat/completions 等）。 */
  endpoint: string;
  /** 请求状态：ok / error / timeout / disconnect。 */
  status: string;
  /** 输入 token（含缓存命中与写入）。 */
  prompt_tokens: number;
  /** 输出 token。 */
  completion_tokens: number;
  /** 命中缓存的输入 token 数（prompt 子集）。 */
  cached_tokens: number;
  /** 写入缓存的输入 token 数（prompt 子集）。 */
  cache_write_tokens: number;
  /** 估算成本（美元）。 */
  cost: number;
  /** 端到端耗时（毫秒）。明细查询为 0（仅内存请求摘要携带）。 */
  elapsed_ms: number;
}

/** 最近 10 分钟的一个分钟桶（数组按时间从旧到新排列，index 0 最早）。 */
export interface UsageMinuteBucket {
  /** 该分钟内的请求数。 */
  requests: number;
  /** 该分钟内的输入 token。 */
  prompt_tokens: number;
  /** 该分钟内的输出 token。 */
  completion_tokens: number;
  /** 该分钟内的估算成本（美元）。 */
  cost: number;
}

/** token 用量汇总统计（stats_get 返回）。 */
export interface UsageStats {
  /** 总请求数。 */
  total_requests: number;
  /** 总输入 token。 */
  total_prompt_tokens: number;
  /** 总输出 token。 */
  total_completion_tokens: number;
  /** 总缓存命中 token。 */
  total_cached_tokens: number;
  /** 总估算成本（美元）。 */
  total_cost: number;
  /** 按模型分组。 */
  by_model: UsageGroupRow[];
  /** 按端点分组。 */
  by_endpoint: UsageGroupRow[];
  /** 最近 10 分钟（10 个分钟桶，旧→新）。 */
  last_10_minutes: UsageMinuteBucket[];
  /** 最近请求明细（新在前，上限 20 条）。 */
  recent_requests: UsageRecentRow[];
}

/** 趋势图数据点：label 为桶标签（HH:00 或 MM-DD）。 */
export interface UsageChartPoint {
  /** 桶标签：小时桶为 "HH:00"，天桶为 "MM-DD"。 */
  label: string;
  /** 该桶输入 token。 */
  prompt_tokens: number;
  /** 该桶输出 token。 */
  completion_tokens: number;
  /** 该桶估算成本（美元）。 */
  cost: number;
}

/** 用量统计时间范围，与后端 Period::parse 的取值一致。 */
export type UsagePeriod = "today" | "24h" | "7d" | "30d" | "60d" | "all";

/** 模型能力标记（文本输入 / 视觉 / 思考）。 */
export interface ModelCaps {
  /** 是否支持文本输入。 */
  text: boolean;
  /** 是否支持视觉输入（图片）。 */
  vision: boolean;
  /** 是否支持思考/推理输出。 */
  reasoning: boolean;
}

/** 单档费率（$/1M tokens）。 */
export interface ModelRates {
  /** 输入单价。 */
  input: number;
  /** 输出单价。 */
  output: number;
  /** 缓存命中单价。 */
  cacheRead: number;
  /** 缓存写入单价。 */
  cacheWrite: number;
}

/** 价格档位：maxContext 为该档输入 token 上限，null 表示最高档无上限。 */
export interface ModelTier {
  /** 该档覆盖的最大输入 token（null 表示无上限）。 */
  maxContext: number | null;
  /** 该档费率。 */
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
  /** 折扣百分比（rates 已是折后有效价，此字段仅展示）。 */
  discountPercent: number;
  /** 是否限时免费（为真时不产生成本）。 */
  free: boolean;
  /** 促销结束日期（YYYY-MM-DD），存在时仅作展示。 */
  expires?: string;
  /** 促销结束条件描述，存在时仅作展示。 */
  endsWhen?: string;
}

/** 模型价格条目（model_pricing 表）：促销、分档费率与闲/忙时，按模型 ID 与列表关联。 */
export interface ModelPricing {
  /** 模型 ID（价格表归一化键，匹配口径见后端 find_pricing）。 */
  id: string;
  /** 促销信息（无促销时缺省）。 */
  deal?: ModelDeal;
  /** 闲时/忙时费率（仅 DeepSeek 系列）。 */
  timeOfDay?: ModelTimeOfDay;
  /** 分档费率（至少一档）。 */
  tiers: ModelTier[];
}

/** 单个模型在当前套餐下的准入结果。 */
export interface ModelAccessInfo {
  /** 当前套餐下是否可用。 */
  allowed: boolean;
  /** 不可用时最低需要的套餐名（如 GOAT / Provider）。 */
  minimum_plan?: string | null;
  /** 不可用原因：留空，由前端按当前语言结合 minimum_plan 组装文案。 */
  reason?: string | null;
}

/** 当前 Command Code 账户的套餐上下文。 */
export interface PlanContext {
  /** 套餐 ID（如 individual-go）；无有效订阅时为 null。 */
  plan_id: string | null;
  /** 套餐展示名（如 Go / Pro / Max）；无套餐时为空字符串。 */
  plan_name: string;
  /** 已购买按量额度（美元）。 */
  purchased_credits: number;
  /** 赠送额度（美元）。 */
  free_credits: number;
  /** 上游拉取是否失败（失败时不做任何限制）。 */
  fetch_failed: boolean;
  /** 附加说明：内部原因码（如 plan_fetch_failed / no_account），空串表示无。 */
  note: string;
}

/** 套餐状态：套餐信息 + 各模型准入结果（键为模型 ID）。 */
export interface PlanStatus {
  /** 当前套餐上下文。 */
  plan: PlanContext;
  /** 各模型准入结果（键为模型 ID）。 */
  access: Record<string, ModelAccessInfo>;
}

/** 限额窗口（5 小时 / 周）：used 为已用量，cap 为上限，resetAt 为重置时间（毫秒）。 */
export interface LimitWindow {
  /** 已用量（上游为额度值或请求数）。 */
  used: number;
  /** 上限。 */
  cap: number;
  /** 窗口重置时间（Unix 毫秒），缺失为 null。 */
  reset_at: number | null;
}

/** 组织级消费限额行。 */
export interface OrgLimit {
  /** 限额项展示名。 */
  label: string;
  /** 已用百分比（0-100）。 */
  pct: number;
  /** 是否已达上限。 */
  reached: boolean;
}

/** 账户使用策略：轮询 / 优先消耗指定账户（含会话粘滞）。 */
export type AccountStrategy = "round_robin" | "priority";

/** 账户使用规则：策略与优先消耗的账户 userId（空表示自动取剩余额度最多者）。 */
export interface AccountRouting {
  /** 使用策略：round_robin（轮询）/ priority（优先消耗指定账户 + 会话粘滞）。 */
  strategy: AccountStrategy;
  /** 优先消耗的账户 userId（空表示自动选择）。 */
  preferred_account_id: string;
}

/** 单个账户的额度快照（来自上游 whoami/subscriptions/credits/summary）。 */
export interface AccountQuota {
  /** 账户唯一标识（后端新增，前端按此键配对，不再用掩码/下标）。 */
  user_id: string;
  /** 账户显示名。 */
  user_name: string;
  /** 掩码 Key（仅展示用，不可作匹配键）。 */
  masked_key: string;
  /** 套餐 ID；无订阅时为 null。 */
  plan_id: string | null;
  /** 套餐展示名；无订阅时为空字符串（文案由前端按语言渲染）。 */
  plan_name: string;
  /** 订阅状态（active / trialing / past_due …）。 */
  status: string | null;
  /** 月剩余额度（美元）。 */
  monthly_remaining: number;
  /** 购买剩余额度（美元）。 */
  purchased_remaining: number;
  /** 赠送剩余额度（美元）。 */
  free_remaining: number;
  /** 总剩余（美元）。 */
  total_remaining: number;
  /** 总池（美元）= 月额度 + 购买 + 赠送。 */
  total_pool: number;
  /** 本计费周期内上游统计的实际消耗（美元）。 */
  total_spent: number;
  /** 用量百分比（余额视角，0-100）。 */
  usage_percent: number;
  /** 是否有可展示的计费数据。 */
  has_billing: boolean;
  /** 距离周期重置的天数（向上取整）。 */
  days_left: number | null;
  /** 周期起点（Unix 毫秒）。 */
  period_start: number | null;
  /** 周期终点（Unix 毫秒）。 */
  period_end: number | null;
  /** 5 小时窗口限额。 */
  five_hour: LimitWindow | null;
  /** 周窗口限额。 */
  weekly: LimitWindow | null;
  /** 组织级消费限额。 */
  org_limits: OrgLimit[];
  /** 拉取失败原因码（成功为 null，码表见 quota.error.*）。 */
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
  // 本地转发 Key：读取（掩码）/ 返回完整 Key（复制用）/ 保存 / 随机生成 / 删除
  localKeyGet: () => invoke<ApiKeyState>("local_key_get"),
  localKeyExpose: () => invoke<string>("local_key_expose"),
  localKeySet: (key: string) => invoke<void>("local_key_set", { key }),
  localKeyGenerate: () => invoke<{ key: string; masked: string }>("local_key_generate"),
  localKeyDelete: () => invoke<void>("local_key_delete"),
  // Command Code 账户：列表 / 新增（whoami 验证补全，可选自定义显示名）/ 重命名 / 删除
  accountList: () => invoke<{ accounts: AccountEntry[] }>("account_list"),
  accountAdd: (key: string, userName?: string) =>
    invoke<void>("account_add", { key, userName }),
  accountRename: (userId: string, userName: string) =>
    invoke<void>("account_rename", { userId, userName }),
  accountRemove: (index: number) => invoke<void>("account_remove", { index }),
  // 按 userId 删除（幂等）：下标在列表并发变更时易错位，删除优先用此命令
  accountRemoveById: (userId: string) =>
    invoke<{ removed: boolean }>("account_remove_by_id", { userId }),
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
