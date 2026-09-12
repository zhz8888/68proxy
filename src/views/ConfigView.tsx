import { useEffect, useState } from "react";
import {
  AlertTriangle,
  Download,
  Eye,
  EyeOff,
  KeyRound,
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

        <Section title="CC 账户" desc="user_ 开头的 Command Code 上游 Key；可配置多个，请求按轮询自动切换，分散单账户限流/配额压力">
          <div className="space-y-3">
            {accounts.length === 0 ? (
              <p className="flex items-center gap-2 text-xs text-muted-foreground">
                <UserRound className="h-3.5 w-3.5" />
                尚未添加 CC 账户（需至少一个才能启动代理）
              </p>
            ) : (
              <ul className="space-y-2">
                {accounts.map((a) => (
                  <li key={a.index} className="flex items-center justify-between rounded-md border px-3 py-2">
                    <span className="font-mono text-xs text-muted-foreground">{a.masked}</span>
                    <Button variant="ghost" size="sm" onClick={() => removeAccount(a.index)}>
                      <Trash2 />
                      移除
                    </Button>
                  </li>
                ))}
              </ul>
            )}
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
        </Section>

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
