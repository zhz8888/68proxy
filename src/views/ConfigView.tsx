import { useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  Check,
  Copy,
  Download,
  Eye,
  EyeOff,
  KeyRound,
  Monitor,
  Moon,
  RefreshCw,
  Save,
  Sun,
  Trash2,
  Wand2,
  XCircle,
  type LucideIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { applyLanguage, translate, type Language } from "@/i18n";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { api, type ApiKeyState, type Config } from "@/lib/api";
import { errText, msgText } from "@/lib/messages";
import { copyText } from "@/lib/format";
import { pickSavePath } from "@/lib/platform";
import { applyTheme, type ThemeMode } from "@/lib/theme";

// 配置项默认值，字段与后端 config.json 一一对应
const DEFAULTS: Config = {
  port: 3050,
  host: "127.0.0.1",
  api_base: "https://api.commandcode.ai",
  project_slug: "cc-proxy",
  log_file: "",
  log_level: "info",
  use_provider_models: true,
  model_refresh_interval_secs: 300,
  auto_start_proxy: false,
  show_window_on_start: true,
  autostart: false,
  close_to_tray: true,
  usage_enabled: true,
  usage_retention_days: 0,
  cc_accounts: [],
  local_api_key: "",
  empty_system_placeholder: true,
  zdr: false,
  max_body_mb: 10,
  client_drain_timeout_ms: 0,
  stream_idle_timeout_secs: 0,
  nonstream_idle_timeout_secs: 0,
  max_inflight: 32,
  theme: "system",
  language: "zh",
  proxy_mode: "none",
  proxy_type: "socks5",
  proxy_host: "",
  proxy_port: 0,
  proxy_username: "",
  proxy_password: "",
  cli_mode: "agent",
  cli_session_mode: "interactive",
  fingerprint_salt: "",
  device_project_dir: "",
};

/** 主题选项：值与 i18n key、图标（文案在组件内按当前语言取）。 */
const THEME_OPTIONS: Array<{ value: ThemeMode; labelKey: string; icon: LucideIcon }> = [
  { value: "system", labelKey: "theme.system", icon: Monitor },
  { value: "dark", labelKey: "theme.dark", icon: Moon },
  { value: "light", labelKey: "theme.light", icon: Sun },
];

/** 语言选项：值与 i18n key（语言名按各自母语书写，跟随界面语言切换）。 */
const LANGUAGE_OPTIONS: Array<{ value: Language; labelKey: string }> = [
  { value: "zh", labelKey: "theme.langZh" },
  { value: "en", labelKey: "theme.langEn" },
];

/** 出站代理模式选项：值与 i18n key（复用主题三段式 Tabs 范式）。 */
const PROXY_MODE_OPTIONS: Array<{ value: string; labelKey: string }> = [
  { value: "none", labelKey: "config.proxyModeNone" },
  { value: "system", labelKey: "config.proxyModeSystem" },
  { value: "custom", labelKey: "config.proxyModeCustom" },
];

/** 出站代理类型选项：SOCKS5 / HTTP。 */
const PROXY_TYPE_OPTIONS: Array<{ value: string; labelKey: string }> = [
  { value: "socks5", labelKey: "config.proxyTypeSocks5" },
  { value: "http", labelKey: "config.proxyTypeHttp" },
];

/** 配置页通用区块卡片：标题 + 可选描述 + 内容。 */
function Section({
  title,
  desc,
  children,
  className,
}: {
  title: string;
  desc?: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <Card className={className}>
      <CardHeader className="pb-3">
        <CardTitle className="text-sm">{title}</CardTitle>
        {desc && <CardDescription>{desc}</CardDescription>}
      </CardHeader>
      <CardContent className="space-y-4 pt-0">{children}</CardContent>
    </Card>
  );
}

/** 单个配置字段：标签 + 可选提示文案 + 控件。 */
function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <div className="flex items-baseline justify-between">
        <Label>{label}</Label>
        {hint && <span className="text-xs text-muted-foreground">{hint}</span>}
      </div>
      {children}
    </div>
  );
}

/** 配置视图：编辑服务、模型、代理行为、程序、外观、Token 统计、日志与本地转发 Key，改动后自动保存。
 *  Command Code 上游账户由独立的账户视图管理（见 AccountsView）。 */
export function ConfigView() {
  const { t } = useTranslation();
  const [cfg, setCfg] = useState<Config>(DEFAULTS);
  const [loaded, setLoaded] = useState(false);
  // 加载失败提示（加载失败时禁用自动保存，避免用默认值覆盖后端配置）
  const [loadError, setLoadError] = useState("");
  // 端口以字符串保存，允许输入过程中的空值/非法值，仅在合法时同步到 cfg.port
  const [portInput, setPortInput] = useState(String(DEFAULTS.port));
  // 代理端口以字符串保存（同端口输入模式），仅在合法时同步到 cfg.proxy_port
  const [proxyPortInput, setProxyPortInput] = useState(
    DEFAULTS.proxy_port > 0 ? String(DEFAULTS.proxy_port) : "",
  );
  // 模型刷新间隔以字符串保存（同端口输入模式）：清空/非法值期间不同步到配置，
  // 否则 Number("") 得 0 会让模型缓存判断恒为未命中、每次取模型都打上游
  const [refreshInput, setRefreshInput] = useState(
    String(DEFAULTS.model_refresh_interval_secs),
  );
  // 用量保留天数以字符串保存（同端口输入模式）：清空输入期间不同步到配置，
  // 否则 Number("") 得 0 会被解释为「永久保留」并立即落库
  const [retentionInput, setRetentionInput] = useState(
    String(DEFAULTS.usage_retention_days),
  );
  // 上游空闲超时以字符串保存（同端口输入模式，两处）：0 是合法值（表示不限），
  // 但清空输入时 Number("") 也得 0，会被误当成「不限」静默落库，须靠字符串中间态挡住
  const [streamIdleInput, setStreamIdleInput] = useState(
    String(DEFAULTS.stream_idle_timeout_secs),
  );
  const [nonstreamIdleInput, setNonstreamIdleInput] = useState(
    String(DEFAULTS.nonstream_idle_timeout_secs),
  );
  // 是否已完成一次成功的加载：用于跳过一次「加载后立即自动保存」
  const skipNextAutosave = useRef(true);
  // 本地转发 Key（sk-）的凭据状态
  const [localKey, setLocalKey] = useState<ApiKeyState>({ has_key: false, masked: "" });
  const [localKeyInput, setLocalKeyInput] = useState("");
  const [showLocalKey, setShowLocalKey] = useState(false);
  // 是否刚复制过完整 Key（短暂显示「已复制」图标）
  const [copiedKey, setCopiedKey] = useState(false);
  const [portInUse, setPortInUse] = useState<{ in_use: boolean; pid: number | null }>({
    in_use: false,
    pid: null,
  });
  const [freeing, setFreeing] = useState(false);

  // 挂载时并行加载配置与本地 Key，loaded 用于区分“初始加载完成”
  useEffect(() => {
    Promise.all([api.configGet(), api.localKeyGet()])
      .then(([c, k]) => {
        // 语言字段做兜底（旧后端/旧配置可能缺失），保证下拉框始终有合法选中值
        setCfg({ ...c, language: c.language === "en" ? "en" : "zh" });
        setPortInput(String(c.port));
        setProxyPortInput(c.proxy_port > 0 ? String(c.proxy_port) : "");
        setRefreshInput(String(c.model_refresh_interval_secs));
        setRetentionInput(String(c.usage_retention_days));
        setStreamIdleInput(String(c.stream_idle_timeout_secs));
        setNonstreamIdleInput(String(c.nonstream_idle_timeout_secs));
        setLocalKey(k);
        setLoaded(true);
      })
      .catch((e) => {
        // 加载失败不得静默：先前的实现会让 loaded 永远为 false，
        // 页面显示假默认值且所有编辑都不落库
        // 保存原始错误串，渲染时再翻译，使切换语言后已显示的提示随之更新
        setLoadError(String(e));
      });
  }, []);

  // 端口改动后防抖 400ms 再检测占用，避免逐字符输入时频繁请求
  useEffect(() => {
    if (!loaded) return;
    const t2 = setTimeout(() => {
      api.portCheck(cfg.port).then(setPortInUse).catch(() => {});
    }, 400);
    return () => clearTimeout(t2);
  }, [cfg.port, loaded]);

  /** 更新端口输入：仅在 1-65535 时同步到配置，空值/非法值期间不触发保存与检测。 */
  function updatePort(raw: string) {
    setPortInput(raw);
    const n = Number(raw);
    if (raw.trim() !== "" && Number.isInteger(n) && n >= 1 && n <= 65535) {
      update("port", n);
    }
  }

  /** 更新代理端口输入：仅在 1-65535 时同步到配置，空值/非法值期间不触发保存。 */
  function updateProxyPort(raw: string) {
    setProxyPortInput(raw);
    const n = Number(raw);
    if (raw.trim() !== "" && Number.isInteger(n) && n >= 1 && n <= 65535) {
      update("proxy_port", n);
    }
  }

  /** 更新模型刷新间隔输入：仅在正整数时同步到配置，空值/非法值期间不触发保存。 */
  function updateRefreshInterval(raw: string) {
    setRefreshInput(raw);
    const n = Number(raw);
    if (raw.trim() !== "" && Number.isInteger(n) && n >= 1) {
      update("model_refresh_interval_secs", n);
    }
  }

  /** 更新用量保留天数输入：仅在非负整数时同步到配置。
   *  清空输入时 Number("") === 0 会被后端解释为「永久保留」，必须靠字符串中间态挡住。 */
  function updateRetentionDays(raw: string) {
    setRetentionInput(raw);
    const n = Number(raw);
    if (raw.trim() !== "" && Number.isInteger(n) && n >= 0) {
      update("usage_retention_days", n);
    }
  }

  /** 更新流式空闲超时输入：仅在非负整数时同步到配置（0 = 不限）。
   *  清空输入时 Number("") === 0 会被当成「不限」，靠字符串中间态挡住静默落库。 */
  function updateStreamIdle(raw: string) {
    setStreamIdleInput(raw);
    const n = Number(raw);
    if (raw.trim() !== "" && Number.isInteger(n) && n >= 0) {
      update("stream_idle_timeout_secs", n);
    }
  }

  /** 更新非流式空闲超时输入：语义同 updateStreamIdle。 */
  function updateNonstreamIdle(raw: string) {
    setNonstreamIdleInput(raw);
    const n = Number(raw);
    if (raw.trim() !== "" && Number.isInteger(n) && n >= 0) {
      update("nonstream_idle_timeout_secs", n);
    }
  }

  /** 更新单个配置字段（只改本地状态，实际保存由防抖 effect 完成）。 */
  function update<K extends keyof Config>(key: K, value: Config[K]) {
    setCfg((c) => ({ ...c, [key]: value }));
  }

  /** 保存配置到后端，并同步开机自启开关；端口/地址变更时提示需重启。 */
  async function save() {
    try {
      const res = await api.configSave(cfg);
      await api.autostartSet(cfg.autostart);
      if (res.needs_restart) {
        toast.success(t("config.needRestart"));
      }
    } catch (e) {
      toast.error(errText(e));
    }
  }

  /** 结束占用当前端口的进程，并重新检测占用状态。 */
  async function freePort() {
    setFreeing(true);
    try {
      const r = await api.portFree(cfg.port);
      toast.success(msgText(r.message));
      const check = await api.portCheck(cfg.port);
      setPortInUse(check);
    } catch (e) {
      toast.error(errText(e));
    } finally {
      setFreeing(false);
    }
  }

  // 配置改动后自动保存（防抖 600ms），无需手动点保存
  useEffect(() => {
    if (!loaded || loadError) return;
    // 跳过「加载完成后」的第一次触发：否则每次进入页面都会无条件回写一次配置
    if (skipNextAutosave.current) {
      skipNextAutosave.current = false;
      return;
    }
    const timer = setTimeout(() => {
      save();
    }, 600);
    return () => clearTimeout(timer);
  }, [cfg, loaded, loadError]);

  /** 校验并保存本地转发 Key（必须以 sk- 开头）。 */
  async function saveLocalKey() {
    if (!localKeyInput.trim()) {
      toast.error(t("config.keyRequired"));
      return;
    }
    if (!localKeyInput.trim().startsWith("sk-")) {
      toast.error(t("config.keyPrefix"));
      return;
    }
    try {
      await api.localKeySet(localKeyInput.trim());
      setLocalKey(await api.localKeyGet());
      setLocalKeyInput("");
      toast.success(t("config.keySaved"));
    } catch (e) {
      toast.error(errText(e));
    }
  }

  /** 随机生成一个新的本地转发 Key 并保存。 */
  async function generateLocalKey() {
    try {
      const r = await api.localKeyGenerate();
      setLocalKey({ has_key: true, masked: r.masked });
      setLocalKeyInput("");
      toast.success(t("config.keyGenerated"));
    } catch (e) {
      toast.error(errText(e));
    }
  }

  /** 复制完整本地转发 Key 到剪贴板（Key 仅在用户主动复制时由后端返回）。 */
  async function copyLocalKey() {
    try {
      const key = await api.localKeyExpose();
      if (!key) {
        toast.error(t("config.localKeyMissing"));
        return;
      }
      if (await copyText(key)) {
        setCopiedKey(true);
        setTimeout(() => setCopiedKey(false), 1400);
      }
    } catch (e) {
      toast.error(errText(e));
    }
  }

  /** 删除已保存的本地转发 Key 并刷新状态。 */
  async function deleteLocalKey() {
    try {
      await api.localKeyDelete();
      setLocalKey({ has_key: false, masked: "" });
      toast.success(t("config.keyDeleted"));
    } catch (e) {
      toast.error(errText(e));
    }
  }

  /** 弹出保存对话框，把最近日志导出到用户选择的文件。 */
  async function exportLogs() {
    try {
      const path = await pickSavePath("68Proxy-logs.log", ["log", "txt"]);
      if (!path) return;
      const count = await api.logsExport(path);
      toast.success(t("config.exported", { p0: count }));
    } catch (e) {
      toast.error(errText(e));
    }
  }

  return (
    <div className="space-y-4 pb-8">
        {loadError && (
          <div className="flex items-center gap-2 rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
            <AlertTriangle className="h-4 w-4 shrink-0" />
            {t("config.loadFailed", { p0: errText(loadError) })}
          </div>
        )}
        <div className="grid grid-cols-2 gap-4">
          <Section title={t("config.serviceTitle")} desc={t("config.serviceDesc")}>
            <Field label={t("config.listenPort")} hint={t("config.listenPortHint")}>
              <div className="space-y-1.5">
                <Input
                  type="number"
                  value={portInput}
                  onChange={(e) => updatePort(e.target.value)}
                />
                {portInUse.in_use && (
                  <div className="space-y-1.5">
                    <p className="flex items-center gap-1.5 text-xs text-destructive">
                      <AlertTriangle className="h-3.5 w-3.5" />
                      {t("config.portInUse", { p0: cfg.port })}
                      {portInUse.pid ? t("config.portInUsePid", { p0: portInUse.pid }) : ""}
                    </p>
                    {portInUse.pid && (
                      <Button
                        variant="destructive"
                        size="sm"
                        onClick={freePort}
                        disabled={freeing}
                      >
                        {freeing ? <RefreshCw className="animate-spin" /> : <XCircle />}
                        {t("config.killOccupier", { p0: portInUse.pid })}
                      </Button>
                    )}
                  </div>
                )}
              </div>
            </Field>
            <Field label={t("config.listenHost")} hint={t("config.listenHostHint")}>
              <Input value={cfg.host} onChange={(e) => update("host", e.target.value)} />
            </Field>
          </Section>

          <Section title={t("config.modelsTitle")} desc={t("config.modelsDesc")}>
            <div className="flex items-center justify-between">
              <Label>{t("config.dynamicModels")}</Label>
              <Switch
                checked={cfg.use_provider_models}
                onCheckedChange={(v) => update("use_provider_models", v)}
              />
            </div>
            <Field label={t("config.refreshInterval")}>
              <Input
                type="number"
                min={1}
                value={refreshInput}
                onChange={(e) => updateRefreshInterval(e.target.value)}
              />
            </Field>
          </Section>

          <Section title={t("config.proxyTitle")} desc={t("config.proxyDesc")}>
            <div className="flex items-center justify-between">
              <Label>{t("config.emptySystemPlaceholder")}</Label>
              <Switch
                checked={cfg.empty_system_placeholder}
                onCheckedChange={(v) => update("empty_system_placeholder", v)}
              />
            </div>
            <p className="text-xs text-muted-foreground">{t("config.emptySystemPlaceholderHint")}</p>
            <div className="flex items-center justify-between pt-2">
              <Label>{t("config.zdr")}</Label>
              <Switch checked={cfg.zdr} onCheckedChange={(v) => update("zdr", v)} />
            </div>
            <p className="text-xs text-muted-foreground">{t("config.zdrHint")}</p>
            <Field
              label={t("config.streamIdleTimeout")}
              hint={t("config.idleTimeoutHint")}
            >
              <Input
                type="number"
                min={0}
                value={streamIdleInput}
                onChange={(e) => updateStreamIdle(e.target.value)}
              />
            </Field>
            <Field
              label={t("config.nonstreamIdleTimeout")}
              hint={t("config.idleTimeoutHint")}
            >
              <Input
                type="number"
                min={0}
                value={nonstreamIdleInput}
                onChange={(e) => updateNonstreamIdle(e.target.value)}
              />
            </Field>
          </Section>

          <Section title={t("config.outboundProxyTitle")} desc={t("config.outboundProxyDesc")}>
            <Field label={t("config.proxyModeLabel")}>
              {/* 三段式模式切换：不走代理 / 跟随系统 / 自定义 */}
              <Tabs
                value={cfg.proxy_mode}
                onValueChange={(v) => update("proxy_mode", v)}
              >
                <TabsList className="h-11 w-full">
                  {PROXY_MODE_OPTIONS.map((opt) => (
                    <TabsTrigger key={opt.value} value={opt.value} className="h-9 flex-1">
                      {translate(opt.labelKey)}
                    </TabsTrigger>
                  ))}
                </TabsList>
              </Tabs>
              {/* 模式说明独占一行，避免长文案与标签同行被挤压折叠 */}
              <p className="text-xs text-muted-foreground">{t("config.proxyModeHint")}</p>
            </Field>
            {cfg.proxy_mode === "custom" && (
              <>
                <Field label={t("config.proxyTypeLabel")}>
                  <Select value={cfg.proxy_type} onValueChange={(v) => update("proxy_type", v)}>
                    <SelectTrigger>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {PROXY_TYPE_OPTIONS.map((opt) => (
                        <SelectItem key={opt.value} value={opt.value}>
                          {translate(opt.labelKey)}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </Field>
                <div className="grid grid-cols-2 gap-3">
                  <Field label={t("config.proxyHostLabel")}>
                    <Input
                      value={cfg.proxy_host}
                      onChange={(e) => update("proxy_host", e.target.value)}
                      placeholder="127.0.0.1"
                      className="font-mono"
                    />
                  </Field>
                  <Field label={t("config.proxyPortLabel")}>
                    <Input
                      type="number"
                      min={1}
                      max={65535}
                      value={proxyPortInput}
                      onChange={(e) => updateProxyPort(e.target.value)}
                      placeholder="1080"
                      className="font-mono"
                    />
                  </Field>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <Field label={t("config.proxyUsernameLabel")}>
                    <Input
                      value={cfg.proxy_username}
                      onChange={(e) => update("proxy_username", e.target.value)}
                      autoComplete="off"
                    />
                  </Field>
                  <Field label={t("config.proxyPasswordLabel")}>
                    <Input
                      type="password"
                      value={cfg.proxy_password}
                      onChange={(e) => update("proxy_password", e.target.value)}
                      autoComplete="new-password"
                    />
                  </Field>
                </div>
              </>
            )}
          </Section>

          <Section title={t("config.programTitle")} desc={t("config.programDesc")}>
            <div className="flex items-center justify-between">
              <Label>{t("config.autoStartProxy")}</Label>
              <Switch
                checked={cfg.auto_start_proxy}
                onCheckedChange={(v) => update("auto_start_proxy", v)}
              />
            </div>
            <div className="flex items-center justify-between">
              <Label>{t("config.autostart")}</Label>
              <Switch checked={cfg.autostart} onCheckedChange={(v) => update("autostart", v)} />
            </div>
            <div className="flex items-center justify-between">
              <Label>{t("config.closeToTray")}</Label>
              <Switch checked={cfg.close_to_tray} onCheckedChange={(v) => update("close_to_tray", v)} />
            </div>
            <div className="flex items-center justify-between">
              <Label>{t("config.showWindowOnStart")}</Label>
              <Switch
                checked={cfg.show_window_on_start}
                onCheckedChange={(v) => update("show_window_on_start", v)}
              />
            </div>
          </Section>

          <Section title={t("theme.sectionTitle")} desc={t("theme.sectionDesc")}>
            <Field label={t("theme.appearanceMode")}>
              {/* 三段式滑块：点击任一段即时切换主题（高度与页面其他表单控件一致） */}
              <Tabs
                value={cfg.theme}
                onValueChange={(v) => {
                  const mode = v as ThemeMode;
                  // 即时生效（切换 <html> 的 dark 类）并经专用命令持久化——
                  // config_save 会保留 theme 原值，故此处必须显式落库
                  applyTheme(mode);
                  update("theme", mode);
                  api.themeSet(mode).catch((e) => toast.error(errText(e)));
                }}
              >
                <TabsList className="h-11 w-full">
                  {THEME_OPTIONS.map((opt) => {
                    const Icon = opt.icon;
                    return (
                      <TabsTrigger key={opt.value} value={opt.value} className="h-9 flex-1 gap-1.5">
                        <Icon className="h-3.5 w-3.5" />
                        {translate(opt.labelKey)}
                      </TabsTrigger>
                    );
                  })}
                </TabsList>
              </Tabs>
            </Field>
            <Field label={t("theme.language")}>
              {/* 语言下拉框：切换即时生效并经专用命令持久化（config_save 会保留 language 原值） */}
              <Select
                value={cfg.language}
                onValueChange={(v) => {
                  const lang = v as Language;
                  applyLanguage(lang);
                  update("language", lang);
                  api.languageSet(lang).catch((e) => toast.error(errText(e)));
                }}
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {LANGUAGE_OPTIONS.map((opt) => (
                    <SelectItem key={opt.value} value={opt.value}>
                      {translate(opt.labelKey)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          </Section>

          <Section title={t("config.statsTitle")} desc={t("config.statsDesc")}>
            <div className="flex items-center justify-between">
              <Label>{t("config.usageEnabled")}</Label>
              <Switch
                checked={cfg.usage_enabled}
                onCheckedChange={(v) => update("usage_enabled", v)}
              />
            </div>
            <Field label={t("config.retentionDays")} hint={t("config.retentionDaysHint")}>
              <Input
                type="number"
                min={0}
                value={retentionInput}
                onChange={(e) => updateRetentionDays(e.target.value)}
              />
            </Field>
          </Section>

          <Section title={t("config.logsTitle")} desc={t("config.logsDesc")}>
            <Field label={t("config.logLevel")}>
              <Select value={cfg.log_level} onValueChange={(v) => update("log_level", v)}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="debug">debug</SelectItem>
                  <SelectItem value="info">info</SelectItem>
                  <SelectItem value="warn">warn</SelectItem>
                  <SelectItem value="error">error</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            <Field label={t("config.exportRecent")} hint={t("config.exportRecentHint")}>
              <Button variant="outline" size="sm" onClick={exportLogs}>
                <Download />
                {t("config.exportLogs")}
              </Button>
            </Field>
          </Section>

          <Section title={t("config.localKeyTitle")} desc={t("config.localKeyDesc")}>
            {/* 当前 Key 状态 */}
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <KeyRound className="h-3.5 w-3.5" />
              {localKey.has_key
                ? t("config.localKeyCurrent", { p0: localKey.masked })
                : t("config.localKeyMissing")}
              {localKey.has_key && (
                <button
                  type="button"
                  onClick={copyLocalKey}
                  className="text-muted-foreground hover:text-foreground"
                  title={t("common.copy")}
                >
                  {copiedKey ? (
                    <Check className="h-3.5 w-3.5 text-success" />
                  ) : (
                    <Copy className="h-3.5 w-3.5" />
                  )}
                </button>
              )}
            </div>
            {/* 输入框独占一行，按钮统一置于卡片底部 */}
            <div className="relative">
              <Input
                type={showLocalKey ? "text" : "password"}
                value={localKeyInput}
                onChange={(e) => setLocalKeyInput(e.target.value)}
                placeholder="sk-xxxxxxxx…"
                className="pr-9 font-mono"
              />
              <button
                type="button"
                onClick={() => setShowLocalKey((v) => !v)}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
              >
                {showLocalKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
              </button>
            </div>
            <div className="flex flex-wrap gap-2 pt-1">
              <Button onClick={saveLocalKey}>{t("common.save")}</Button>
              <Button variant="outline" onClick={generateLocalKey}>
                <Wand2 />
                {t("config.generate")}
              </Button>
              {localKey.has_key && (
                <Button variant="destructive-ghost" onClick={deleteLocalKey}>
                  <Trash2 />
                  {t("common.delete")}
                </Button>
              )}
            </div>
          </Section>
        </div>

        <Separator />

        <div className="flex items-center justify-end gap-2 text-xs text-muted-foreground">
          <span className="flex items-center gap-1.5">
            <Save className="h-3.5 w-3.5" />
            {t("config.autoSaved")}
          </span>
        </div>
    </div>
  );
}
