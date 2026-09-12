import { useEffect, useState } from "react";
import {
  AlertTriangle,
  Download,
  Eye,
  EyeOff,
  ExternalLink,
  KeyRound,
  Loader2,
  Pencil,
  Plus,
  RefreshCw,
  Save,
  Trash2,
  UserRound,
  Wand2,
  XCircle,
} from "lucide-react";
import { toast } from "sonner";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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
import { api, type AccountEntry, type ApiKeyState, type Config } from "@/lib/api";

// 配置项默认值，字段与后端 config.json 一一对应
const DEFAULTS: Config = {
  port: 3050,
  host: "0.0.0.0",
  api_base: "https://api.commandcode.ai",
  project_slug: "cc-proxy",
  log_file: "",
  log_level: "info",
  use_provider_models: true,
  model_refresh_interval_ms: 300000,
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
  max_inflight: 0,
};

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

/** 配置视图：编辑服务、模型、偏好、日志与凭据设置，改动后自动保存。 */
export function ConfigView() {
  const [cfg, setCfg] = useState<Config>(DEFAULTS);
  const [loaded, setLoaded] = useState(false);
  // 本地转发 Key（sk-）与 CC 账户（user_）的凭据状态
  const [localKey, setLocalKey] = useState<ApiKeyState>({ has_key: false, masked: "" });
  const [localKeyInput, setLocalKeyInput] = useState("");
  const [showLocalKey, setShowLocalKey] = useState(false);
  const [accounts, setAccounts] = useState<AccountEntry[]>([]);
  const [accountInput, setAccountInput] = useState("");
  const [showAccountInput, setShowAccountInput] = useState(false);
  // 浏览器授权登录弹窗状态
  const [loginOpen, setLoginOpen] = useState(false);
  const [loginUrl, setLoginUrl] = useState("");
  const [loginStatus, setLoginStatus] = useState<"idle" | "pending" | "success" | "denied" | "failed">("idle");
  const [loginError, setLoginError] = useState("");
  // 账户改名状态（editingId 为正在改名的 userId）
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [portInUse, setPortInUse] = useState<{ in_use: boolean; pid: number | null }>({
    in_use: false,
    pid: null,
  });
  const [freeing, setFreeing] = useState(false);

  // 挂载时并行加载配置、本地 Key 与账户列表，loaded 用于区分“初始加载完成”
  useEffect(() => {
    Promise.all([api.configGet(), api.localKeyGet(), api.accountList()]).then(([c, k, a]) => {
      setCfg(c);
      setLocalKey(k);
      setAccounts(a.accounts);
      setLoaded(true);
    });
  }, []);

  // 端口改动后防抖 400ms 再检测占用，避免逐字符输入时频繁请求
  useEffect(() => {
    if (!loaded) return;
    const t = setTimeout(() => {
      api.portCheck(cfg.port).then(setPortInUse).catch(() => {});
    }, 400);
    return () => clearTimeout(t);
  }, [cfg.port, loaded]);

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
        toast.success("端口或地址已变更，重启代理后生效");
      }
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** 结束占用当前端口的进程，并重新检测占用状态。 */
  async function freePort() {
    setFreeing(true);
    try {
      const r = await api.portFree(cfg.port);
      toast.success(r.message);
      const check = await api.portCheck(cfg.port);
      setPortInUse(check);
    } catch (e) {
      toast.error(String(e));
    } finally {
      setFreeing(false);
    }
  }

  // 配置改动后自动保存（防抖 600ms），无需手动点保存
  useEffect(() => {
    if (!loaded) return;
    const t = setTimeout(() => {
      save();
    }, 600);
    return () => clearTimeout(t);
  }, [cfg, loaded]);

  /** 校验并保存本地转发 Key（必须以 sk- 开头）。 */
  async function saveLocalKey() {
    if (!localKeyInput.trim()) {
      toast.error("请输入本地转发 Key");
      return;
    }
    if (!localKeyInput.trim().startsWith("sk-")) {
      toast.error("本地转发 Key 必须以 sk- 开头");
      return;
    }
    try {
      await api.localKeySet(localKeyInput.trim());
      setLocalKey(await api.localKeyGet());
      setLocalKeyInput("");
      toast.success("本地转发 Key 已保存");
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** 随机生成一个新的本地转发 Key 并保存。 */
  async function generateLocalKey() {
    try {
      const r = await api.localKeyGenerate();
      setLocalKey({ has_key: true, masked: r.masked });
      setLocalKeyInput("");
      toast.success("已随机生成新的本地转发 Key");
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** 删除已保存的本地转发 Key 并刷新状态。 */
  async function deleteLocalKey() {
    try {
      await api.localKeyDelete();
      setLocalKey({ has_key: false, masked: "" });
      toast.success("本地转发 Key 已删除");
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** 校验并新增一个 CC 账户 Key（必须以 user_ 开头）。 */
  async function addAccount() {
    if (!accountInput.trim()) {
      toast.error("请输入 CC 账户 Key");
      return;
    }
    if (!accountInput.trim().startsWith("user_")) {
      toast.error("CC 账户 Key 必须以 user_ 开头");
      return;
    }
    try {
      await api.accountAdd(accountInput.trim());
      setAccounts((await api.accountList()).accounts);
      setAccountInput("");
      setShowAccountInput(false);
      toast.success("CC 账户已添加");
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** 按下标删除一个 CC 账户并刷新列表。 */
  async function removeAccount(index: number) {
    try {
      await api.accountRemove(index);
      setAccounts((await api.accountList()).accounts);
      toast.success("CC 账户已移除");
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** 打开授权登录弹窗：启动 loopback 服务器并获取授权 URL。 */
  async function openLoginDialog() {
    setLoginOpen(true);
    setLoginStatus("idle");
    setLoginError("");
    setLoginUrl("");
    try {
      const { url } = await api.authLoginStart();
      setLoginUrl(url);
      setLoginStatus("pending");
    } catch (e) {
      setLoginError(String(e));
      setLoginStatus("failed");
    }
  }

  /** 在系统浏览器中打开授权 URL。 */
  async function openAuthBrowser() {
    if (!loginUrl) return;
    try {
      await openUrl(loginUrl);
    } catch (e) {
      toast.error(`打开浏览器失败：${String(e)}`);
    }
  }

  /** 关闭登录弹窗并取消进行中的登录。 */
  async function closeLoginDialog() {
    setLoginOpen(false);
    try {
      await api.authLoginCancel();
    } catch {
      /* 忽略取消失败 */
    }
  }

  // 弹窗打开且状态为 pending 时轮询登录结果
  useEffect(() => {
    if (!loginOpen || loginStatus !== "pending") return;
    const timer = setInterval(async () => {
      try {
        const r = await api.authLoginPoll();
        if (r.status === "success") {
          setLoginStatus("success");
          clearInterval(timer);
          // 成功后自动关弹窗并刷新列表
          setTimeout(() => {
            setLoginOpen(false);
            setAccounts([]);
            api.accountList().then((a) => setAccounts(a.accounts)).catch(() => {});
            toast.success("CC 账户登录成功");
          }, 600);
        } else if (r.status === "denied") {
          setLoginStatus("denied");
          clearInterval(timer);
        } else if (r.status === "failed") {
          setLoginError(r.error ?? "登录失败");
          setLoginStatus("failed");
          clearInterval(timer);
        }
      } catch {
        /* 轮询失败则下一轮再试 */
      }
    }, 1000);
    return () => clearInterval(timer);
  }, [loginOpen, loginStatus]);

  /** 进入账户改名模式。 */
  function startRename(a: AccountEntry) {
    setRenameId(a.userId);
    setRenameValue(a.userName);
  }

  /** 保存账户自定义显示名。 */
  async function saveRename(userId: string) {
    const name = renameValue.trim();
    if (!name) {
      toast.error("显示名不能为空");
      return;
    }
    try {
      await api.accountRename(userId, name);
      setRenameId(null);
      setAccounts((await api.accountList()).accounts);
      toast.success("账户显示名已更新");
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** 弹出保存对话框，把最近日志导出到用户选择的文件。 */
  async function exportLogs() {
    try {
      const path = await saveDialog({
        defaultPath: "68proxy-logs.log",
        filters: [{ name: "日志文件", extensions: ["log", "txt"] }],
      });
      if (!path) return;
      const count = await api.logsExport(path);
      toast.success(`已导出 ${count} 条日志`);
    } catch (e) {
      toast.error(String(e));
    }
  }

  return (
    <div className="space-y-4 pb-8">
        <div className="grid grid-cols-2 gap-4">
          <Section title="服务" desc="代理监听地址与端口">
            <Field label="监听端口" hint="1-65535">
              <div className="space-y-1.5">
                <Input
                  type="number"
                  value={cfg.port}
                  onChange={(e) => update("port", Number(e.target.value))}
                />
                {portInUse.in_use && (
                  <div className="space-y-1.5">
                    <p className="flex items-center gap-1.5 text-xs text-destructive">
                      <AlertTriangle className="h-3.5 w-3.5" />
                      端口 {cfg.port} 已被占用
                      {portInUse.pid ? `（PID ${portInUse.pid}）` : ""}
                    </p>
                    {portInUse.pid && (
                      <Button
                        variant="destructive"
                        size="sm"
                        onClick={freePort}
                        disabled={freeing}
                      >
                        {freeing ? <RefreshCw className="animate-spin" /> : <XCircle />}
                        结束占用进程（PID {portInUse.pid}）
                      </Button>
                    )}
                  </div>
                )}
              </div>
            </Field>
            <Field label="监听地址" hint="0.0.0.0 允许局域网访问">
              <Input value={cfg.host} onChange={(e) => update("host", e.target.value)} />
            </Field>
          </Section>

          <Section title="模型" desc="模型列表的来源与刷新">
            <div className="flex items-center justify-between">
              <Label>动态拉取 Provider 模型</Label>
              <Switch
                checked={cfg.use_provider_models}
                onCheckedChange={(v) => update("use_provider_models", v)}
              />
            </div>
            <Field label="刷新间隔（毫秒）">
              <Input
                type="number"
                value={cfg.model_refresh_interval_ms}
                onChange={(e) => update("model_refresh_interval_ms", Number(e.target.value))}
              />
            </Field>
          </Section>

          <Section title="代理" desc="CC 上游调用行为">
            <div className="flex items-center justify-between">
              <Label>空 system 占位符</Label>
              <Switch
                checked={cfg.empty_system_placeholder}
                onCheckedChange={(v) => update("empty_system_placeholder", v)}
              />
            </div>
            <p className="text-xs text-muted-foreground">
              无 system prompt 时发送空格占位，阻止 CC 上游注入约 7.5K token 的默认提示词。
            </p>
            <div className="flex items-center justify-between pt-2">
              <Label>ZDR 模式</Label>
              <Switch checked={cfg.zdr} onCheckedChange={(v) => update("zdr", v)} />
            </div>
            <p className="text-xs text-muted-foreground">
              向 CC 上游发送 x-cmd-zdr: 1 请求头（生成与初始化预请求均生效）。
            </p>
          </Section>

          <Section title="偏好" desc="应用与代理的启动方式">
            <div className="flex items-center justify-between">
              <Label>启动程序时自动运行代理</Label>
              <Switch
                checked={cfg.auto_start_proxy}
                onCheckedChange={(v) => update("auto_start_proxy", v)}
              />
            </div>
            <div className="flex items-center justify-between">
              <Label>开机自动启动 68proxy</Label>
              <Switch checked={cfg.autostart} onCheckedChange={(v) => update("autostart", v)} />
            </div>
            <div className="flex items-center justify-between">
              <Label>关闭窗口时隐藏到托盘</Label>
              <Switch checked={cfg.close_to_tray} onCheckedChange={(v) => update("close_to_tray", v)} />
            </div>
            <div className="flex items-center justify-between">
              <Label>启动时显示窗口</Label>
              <Switch
                checked={cfg.show_window_on_start}
                onCheckedChange={(v) => update("show_window_on_start", v)}
              />
            </div>
            <div className="flex items-center justify-between">
              <Label>启用 token 用量统计</Label>
              <Switch
                checked={cfg.usage_enabled}
                onCheckedChange={(v) => update("usage_enabled", v)}
              />
            </div>
            <Field label="用量保留天数" hint="0 表示永久保留">
              <Input
                type="number"
                min={0}
                value={cfg.usage_retention_days}
                onChange={(e) => update("usage_retention_days", Number(e.target.value))}
              />
            </Field>
          </Section>

          <Section title="日志" desc="日志级别与导出">
            <Field label="日志级别">
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
            <Field label="导出最近日志" hint="选择保存位置，导出最近 1000 条">
              <Button variant="outline" size="sm" onClick={exportLogs}>
                <Download />
                导出日志…
              </Button>
            </Field>
          </Section>
        </div>

        <Section title="本地转发 Key" desc="客户端接入本地代理时统一填写的 sk- 开头 Key；该 Key 只用于本机服务鉴权，不会发送给 CC 上游">
          <div className="space-y-3">
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <KeyRound className="h-3.5 w-3.5" />
              {localKey.has_key ? `当前：${localKey.masked}` : "尚未生成本地转发 Key（客户端将无法接入）"}
            </div>
            <div className="flex gap-2">
              <div className="relative flex-1">
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
              <Button onClick={saveLocalKey}>保存</Button>
              <Button variant="outline" onClick={generateLocalKey}>
                <Wand2 />
                随机生成
              </Button>
              {localKey.has_key && (
                <Button variant="ghost" onClick={deleteLocalKey}>
                  <Trash2 />
                  删除
                </Button>
              )}
            </div>
          </div>
        </Section>

        <Section title="CC 账户" desc="user_ 开头的 Command Code 上游账户；可配置多个，请求按轮询自动切换。支持浏览器授权登录或手动粘贴 Key">
          <div className="space-y-3">
            {accounts.length === 0 ? (
              <p className="flex items-center gap-2 text-xs text-muted-foreground">
                <UserRound className="h-3.5 w-3.5" />
                尚未添加 CC 账户（需至少一个才能启动代理）
              </p>
            ) : (
              <ul className="space-y-2">
                {accounts.map((a) => (
                  <li key={a.index} className="flex items-center justify-between gap-2 rounded-md border px-3 py-2">
                    <div className="flex min-w-0 flex-col gap-0.5">
                      <div className="flex items-center gap-2">
                        {renameId === a.userId ? (
                          <div className="flex items-center gap-1.5">
                            <Input
                              value={renameValue}
                              onChange={(e) => setRenameValue(e.target.value)}
                              className="h-6 w-40 font-mono text-xs"
                              autoFocus
                              onKeyDown={(e) => {
                                if (e.key === "Enter") saveRename(a.userId);
                                if (e.key === "Escape") setRenameId(null);
                              }}
                            />
                            <Button size="sm" variant="ghost" className="h-6 px-1.5" onClick={() => saveRename(a.userId)}>
                              <Save className="h-3 w-3" />
                            </Button>
                          </div>
                        ) : (
                          <>
                            <span className="truncate text-sm font-medium text-foreground">
                              {a.userName || a.masked}
                            </span>
                            <Button size="sm" variant="ghost" className="h-5 px-1 text-muted-foreground" onClick={() => startRename(a)}>
                              <Pencil className="h-3 w-3" />
                            </Button>
                          </>
                        )}
                        <span
                          className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${
                            a.source === "oauth"
                              ? "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
                              : "bg-muted text-muted-foreground"
                          }`}
                        >
                          {a.source === "oauth" ? "OAuth" : "手动"}
                        </span>
                      </div>
                      <span className="truncate font-mono text-xs text-muted-foreground">{a.masked}</span>
                    </div>
                    <Button variant="ghost" size="sm" onClick={() => removeAccount(a.index)}>
                      <Trash2 />
                      移除
                    </Button>
                  </li>
                ))}
              </ul>
            )}
            <div className="flex flex-wrap items-center gap-2">
              <Button variant="outline" size="sm" onClick={openLoginDialog}>
                <ExternalLink />
                通过浏览器登录
              </Button>
              {showAccountInput ? (
                <div className="flex gap-2">
                  <Input
                    value={accountInput}
                    onChange={(e) => setAccountInput(e.target.value)}
                    placeholder="user_xxxxxxxxx"
                    className="font-mono"
                  />
                  <Button onClick={addAccount}>添加</Button>
                  <Button variant="ghost" onClick={() => { setAccountInput(""); setShowAccountInput(false); }}>
                    取消
                  </Button>
                </div>
              ) : (
                <Button variant="outline" size="sm" onClick={() => setShowAccountInput(true)}>
                  <Plus />
                  添加账户
                </Button>
              )}
            </div>
          </div>
        </Section>

        {/* 浏览器授权登录弹窗 */}
        <Dialog open={loginOpen} onOpenChange={(open) => { if (!open) closeLoginDialog(); }}>
          <DialogContent className="sm:max-w-md">
            <DialogHeader>
              <DialogTitle>登录 Command Code 账户</DialogTitle>
              <DialogDescription>
                {loginStatus === "pending"
                  ? "将在浏览器中打开授权页面，请完成登录后返回本窗口。"
                  : loginStatus === "success"
                    ? "授权成功，正在添加账户…"
                    : "通过浏览器授权登录 Command Code，无需手动粘贴 Key。"}
              </DialogDescription>
            </DialogHeader>
            <div className="flex flex-col items-center gap-4 py-4">
              {loginStatus === "pending" && (
                <>
                  <Loader2 className="h-8 w-8 animate-spin text-primary" />
                  <p className="text-center text-sm text-muted-foreground">
                    已启动授权回调服务器（127.0.0.1 随机端口），等待你在浏览器中完成授权…
                  </p>
                  <Button onClick={openAuthBrowser}>
                    <ExternalLink />
                    打开浏览器授权
                  </Button>
                </>
              )}
              {loginStatus === "success" && (
                <p className="text-center text-sm text-emerald-600 dark:text-emerald-400">
                  登录成功，账户已添加。
                </p>
              )}
              {loginStatus === "denied" && (
                <p className="text-center text-sm text-amber-600 dark:text-amber-400">
                  授权被拒绝，你可以关闭弹窗后重试。
                </p>
              )}
              {loginStatus === "failed" && (
                <p className="text-center text-sm text-destructive">授权失败：{loginError || "未知错误"}</p>
              )}
            </div>
            <DialogFooter>
              <Button variant="ghost" onClick={closeLoginDialog}>关闭</Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>

        <Separator />

        <div className="flex items-center justify-end gap-2 text-xs text-muted-foreground">
          <span className="flex items-center gap-1.5">
            <Save className="h-3.5 w-3.5" />
            配置改动后自动保存
          </span>
        </div>
    </div>
  );
}
