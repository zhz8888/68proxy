import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ExternalLink,
  Loader2,
  Pencil,
  Plus,
  RefreshCw,
  Route,
  Save,
  Trash2,
  UserRound,
} from "lucide-react";
import { toast } from "sonner";

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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { MeterBar, QuotaDetail, usageColor } from "@/components/QuotaDetail";
import {
  api,
  type AccountEntry,
  type AccountQuota,
  type AccountRouting,
  type LimitWindow,
} from "@/lib/api";
import { openExternal } from "@/lib/platform";
import { formatCost } from "@/lib/format";
import { errText } from "@/lib/messages";
import { cn } from "@/lib/utils";
import { translate } from "@/i18n";

/** 下拉框中「自动选择」选项的哨兵值（Radix Select 不允许空字符串作为 value）。 */
const AUTO_ACCOUNT = "__auto__";

/** 套餐徽标配色：不同档位用不同色调区分（未收录的套餐回退中性灰）。 */
function planBadgeClass(planName: string): string {
  switch (planName) {
    case "Go":
      return "bg-teal-500/15 text-teal-600 dark:text-teal-400";
    case "GOAT":
      return "bg-amber-500/15 text-amber-600 dark:text-amber-400";
    case "Pro":
      return "bg-sky-500/15 text-sky-600 dark:text-sky-400";
    case "Max":
      return "bg-violet-500/15 text-violet-600 dark:text-violet-400";
    case "Ultra":
      return "bg-rose-500/15 text-rose-600 dark:text-rose-400";
    case "Provider":
      return "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400";
    case "Teams Pro":
      return "bg-cyan-500/15 text-cyan-600 dark:text-cyan-400";
    default:
      return "bg-muted text-muted-foreground";
  }
}

/** 限额窗口 → 已用百分比；窗口缺失（未开通/不限）时返回 null 以便渲染「—」。 */
function windowPct(win: LimitWindow | null): number | null {
  if (!win || win.cap <= 0) return null;
  return Math.min((win.used / win.cap) * 100, 100);
}

/** 账户行内的精简用量格：标签 + 迷你进度条 + 百分比（无数据显示「—」）。 */
function UsageCell({ label, pct, title }: { label: string; pct: number | null; title?: string }) {
  return (
    <div className="min-w-0 flex-1" title={title}>
      <div className="flex items-baseline justify-between gap-1 text-[10px]">
        <span className="truncate text-muted-foreground">{label}</span>
        <span className={cn("shrink-0 font-mono", pct != null ? usageColor(pct) : "text-muted-foreground/50")}>
          {pct != null ? `${pct.toFixed(0)}%` : "—"}
        </span>
      </div>
      <MeterBar pct={pct ?? 0} className="mt-0.5 h-1" />
    </div>
  );
}

/**
 * 取账户对应的额度快照。
 *
 * 后端 `accounts_quota` 按账户列表同一顺序并发返回，故优先按顺序对应；
 * 掩码相等时直接采信（防止两侧列表短暂不一致时错位）。
 */
function quotaFor(quotas: AccountQuota[], a: AccountEntry, index: number): AccountQuota | undefined {
  return quotas.find((q) => q.masked_key === a.masked) ?? quotas[index];
}

/**
 * Command Code 账户视图：管理作为上游凭据的 user_ 账户。
 *
 * 支持浏览器授权登录与手动粘贴 Key 两种添加方式，可改名、移除并查看单个账户的完整额度；
 * 多账户请求按轮询自动切换（由后端调度）。
 */
export function AccountsView() {
  const { t } = useTranslation();
  const [accounts, setAccounts] = useState<AccountEntry[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState("");
  const [accountInput, setAccountInput] = useState("");
  const [showAccountInput, setShowAccountInput] = useState(false);
  // 各账户额度快照（套餐类型与 5 小时/周/月用量），与账户列表同序返回
  const [quotas, setQuotas] = useState<AccountQuota[]>([]);
  const [quotaLoading, setQuotaLoading] = useState(false);
  // 浏览器授权登录弹窗状态
  const [loginOpen, setLoginOpen] = useState(false);
  const [loginUrl, setLoginUrl] = useState("");
  const [loginStatus, setLoginStatus] = useState<"idle" | "pending" | "success" | "denied" | "failed">("idle");
  const [loginError, setLoginError] = useState("");
  // 账户改名状态（renameId 为正在改名的 userId）
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  // 账户详情弹窗（展示该账户完整额度）
  const [detailAccount, setDetailAccount] = useState<AccountEntry | null>(null);
  const [detailQuota, setDetailQuota] = useState<AccountQuota | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState("");
  // 账户使用规则（策略 + 优先消耗账户）
  const [routing, setRouting] = useState<AccountRouting>({
    strategy: "round_robin",
    preferred_account_id: "",
  });
  const [routingSaving, setRoutingSaving] = useState(false);

  /** 保存账户使用规则（策略或优先账户变更时调用）。 */
  async function saveRouting(next: AccountRouting) {
    setRouting(next);
    setRoutingSaving(true);
    try {
      await api.accountRoutingSet(next.strategy, next.preferred_account_id);
      toast.success(t("accounts.routingSaved"));
    } catch (e) {
      toast.error(errText(e));
    } finally {
      setRoutingSaving(false);
    }
  }

  /** 重新读取账户列表（新增/移除/登录成功后刷新）。 */
  async function reloadAccounts() {
    setAccounts((await api.accountList()).accounts);
  }

  /**
   * 拉取全部账户的额度快照（套餐类型 + 各窗口用量）。
   *
   * 额度随上游用量变化，故每次进页面 / 列表变更后重拉；失败只清空额度，不影响账户管理。
   */
  async function reloadQuotas() {
    setQuotaLoading(true);
    try {
      setQuotas(await api.accountsQuota());
    } catch {
      setQuotas([]);
    } finally {
      setQuotaLoading(false);
    }
  }

  // 挂载时加载账户列表与额度快照
  useEffect(() => {
    api
      .accountList()
      .then((a) => {
        setAccounts(a.accounts);
        setLoaded(true);
      })
      // 保存原始错误串，渲染时再翻译（切换语言后已显示的提示随之更新）
      .catch((e) => setLoadError(String(e)));
    reloadQuotas();
    api.accountRoutingGet().then(setRouting).catch(() => {});
  }, []);

  /** 校验并新增一个 CC 账户 Key（必须以 user_ 开头）。 */
  async function addAccount() {
    if (!accountInput.trim()) {
      toast.error(t("accounts.keyRequired"));
      return;
    }
    if (!accountInput.trim().startsWith("user_")) {
      toast.error(t("accounts.keyPrefix"));
      return;
    }
    try {
      await api.accountAdd(accountInput.trim());
      await reloadAccounts();
      setAccountInput("");
      setShowAccountInput(false);
      toast.success(t("accounts.added"));
      reloadQuotas();
    } catch (e) {
      toast.error(errText(e));
    }
  }

  /** 按下标删除一个 CC 账户并刷新列表。 */
  async function removeAccount(index: number) {
    try {
      await api.accountRemove(index);
      await reloadAccounts();
      toast.success(t("accounts.removed"));
      reloadQuotas();
    } catch (e) {
      toast.error(errText(e));
    }
  }

  /** 打开账户详情弹窗并加载该账户的完整额度。 */
  async function openAccountDetail(a: AccountEntry) {
    setDetailAccount(a);
    setDetailQuota(null);
    setDetailError("");
    setDetailLoading(true);
    try {
      setDetailQuota(await api.accountQuota(a.userId));
    } catch (e) {
      setDetailError(String(e));
    } finally {
      setDetailLoading(false);
    }
  }

  /** 进入账户改名模式。 */
  function startRename(a: AccountEntry) {
    setRenameId(a.userId);
    setRenameValue(a.userName);
  }

  /** 保存账户自定义显示名。 */
  async function saveRename(userId: string) {
    const name = renameValue.trim();
    if (!name) {
      toast.error(t("accounts.nameEmpty"));
      return;
    }
    try {
      await api.accountRename(userId, name);
      setRenameId(null);
      await reloadAccounts();
      toast.success(t("accounts.renamed"));
    } catch (e) {
      toast.error(errText(e));
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
      await openExternal(loginUrl);
    } catch (e) {
      toast.error(t("accounts.openBrowserFailed", { p0: errText(e) }));
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
            reloadQuotas();
            toast.success(t("accounts.loginSuccess"));
          }, 600);
        } else if (r.status === "denied") {
          setLoginStatus("denied");
          clearInterval(timer);
        } else if (r.status === "failed") {
          // 统一存为 err: 前缀的消息码，渲染时用 errText 翻译；无码时回退通用失败文案
          setLoginError(`err:${r.error || "auth_callback_params_missing"}`);
          setLoginStatus("failed");
          clearInterval(timer);
        }
      } catch {
        /* 轮询失败则下一轮再试 */
      }
    }, 1000);
    return () => clearInterval(timer);
  }, [loginOpen, loginStatus]);

  return (
    <div className="space-y-4 pb-8">
      {loadError && (
        <div className="rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
          {t("accounts.loadFailed", { p0: errText(loadError) })}
        </div>
      )}

      {/* 使用规则：决定请求如何在多账户间分配 */}
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-sm">
            <Route className="h-4 w-4" />
            {t("accounts.routingTitle")}
          </CardTitle>
          <CardDescription>{t("accounts.routingDesc")}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex items-center justify-between gap-3">
            <div className="space-y-0.5">
              <Label className="text-sm">{t("accounts.priorityAccount")}</Label>
              <p className="text-xs text-muted-foreground">{t("accounts.priorityHint")}</p>
            </div>
            <Switch
              checked={routing.strategy === "priority"}
              disabled={routingSaving}
              onCheckedChange={(v) =>
                saveRouting({ ...routing, strategy: v ? "priority" : "round_robin" })
              }
            />
          </div>

          {routing.strategy === "priority" && (
            <>
              <div className="space-y-1.5">
                <Label>{t("accounts.designatedAccount")}</Label>
                <Select
                  value={routing.preferred_account_id || AUTO_ACCOUNT}
                  onValueChange={(v) =>
                    saveRouting({ ...routing, preferred_account_id: v === AUTO_ACCOUNT ? "" : v })
                  }
                  disabled={routingSaving}
                >
                  <SelectTrigger>
                    <SelectValue placeholder={t("accounts.autoSelect")} />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={AUTO_ACCOUNT}>{t("accounts.autoOption")}</SelectItem>
                    {accounts.map((a) => (
                      <SelectItem key={a.userId} value={a.userId}>
                        {a.userName || a.masked}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <p className="rounded-md border border-border/70 bg-secondary/20 px-3 py-2 text-xs leading-relaxed text-muted-foreground">
                {t("accounts.stickyHint")}
              </p>
            </>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <div className="flex items-start justify-between gap-2">
            <div className="space-y-1.5">
              <CardTitle className="flex items-center gap-2 text-sm">
                <UserRound className="h-4 w-4" />
                {t("accounts.title")}
              </CardTitle>
              <CardDescription>{t("accounts.desc")}</CardDescription>
            </div>
            <Button
              variant="ghost"
              size="sm"
              className="h-7 shrink-0 px-2 text-muted-foreground"
              onClick={reloadQuotas}
              disabled={quotaLoading}
              title={t("accounts.refreshUsage")}
            >
              <RefreshCw className={cn("h-3.5 w-3.5", quotaLoading && "animate-spin")} />
              {t("accounts.refreshUsage")}
            </Button>
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          {loaded && accounts.length === 0 ? (
            <p className="flex items-center gap-2 text-xs text-muted-foreground">
              <UserRound className="h-3.5 w-3.5" />
              {t("accounts.empty")}
            </p>
          ) : (
            <ul className="space-y-2">
              {accounts.map((a, i) => {
                const q = quotaFor(quotas, a, i);
                const planName = q?.plan_name ? q.plan_name : null;
                return (
                  <li key={a.index} className="space-y-2 rounded-md border px-3 py-2">
                    <div className="flex items-center justify-between gap-2">
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
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-6 px-1.5"
                                onClick={() => saveRename(a.userId)}
                              >
                                <Save className="h-3 w-3" />
                              </Button>
                            </div>
                          ) : (
                            <>
                              <span className="truncate text-sm font-medium text-foreground">
                                {a.userName || a.masked}
                              </span>
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-5 px-1 text-muted-foreground"
                                onClick={() => startRename(a)}
                              >
                                <Pencil className="h-3 w-3" />
                              </Button>
                            </>
                          )}
                          {/* 订阅类型徽标（Go / GOAT / Pro / Max …），额度未取到时占位「—」 */}
                          <span
                            className={cn(
                              "shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium",
                              planName ? planBadgeClass(planName) : "bg-muted text-muted-foreground",
                            )}
                          >
                            {planName ?? (q ? t("plan.noSubscription") : "—")}
                          </span>
                          <span
                            className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${
                              a.source === "oauth"
                                ? "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
                                : "bg-muted text-muted-foreground"
                            }`}
                          >
                            {a.source === "oauth"
                              ? t("accounts.source.oauth")
                              : t("accounts.source.manual")}
                          </span>
                        </div>
                        <span className="truncate font-mono text-xs text-muted-foreground">{a.masked}</span>
                      </div>
                      <div className="flex shrink-0 items-center gap-1">
                        <Button variant="ghost" size="sm" onClick={() => openAccountDetail(a)}>
                          {t("accounts.quotaDetail")}
                        </Button>
                        <Button variant="ghost" size="sm" onClick={() => removeAccount(a.index)}>
                          <Trash2 />
                          {t("accounts.remove")}
                        </Button>
                      </div>
                    </div>

                    {/* 用量概览：5 小时 / 周窗口限额 + 月配额用量 */}
                    <div className="flex items-center gap-3 border-t pt-2">
                      {q?.error ? (
                        <span className="text-[10px] text-destructive">
                          {t("quota.fetchFailedPrefix", {
                            p0: translate(`quota.error.${q.error}`, { defaultValue: q.error }),
                          })}
                        </span>
                      ) : q && q.has_billing ? (
                        <>
                          <UsageCell
                            label={t("accounts.window.fiveHour")}
                            pct={windowPct(q.five_hour)}
                            title={
                              q.five_hour
                                ? t("accounts.window.usedOfCap", { p0: q.five_hour.used, p1: q.five_hour.cap })
                                : t("accounts.window.fiveHourDisabled")
                            }
                          />
                          <UsageCell
                            label={t("accounts.window.weekly")}
                            pct={windowPct(q.weekly)}
                            title={
                              q.weekly
                                ? t("accounts.window.usedOfCap", { p0: q.weekly.used, p1: q.weekly.cap })
                                : t("accounts.window.weeklyDisabled")
                            }
                          />
                          <UsageCell
                            label={t("accounts.window.monthly")}
                            pct={q.usage_percent}
                            title={t("accounts.window.remainingOfPool", {
                              p0: formatCost(q.total_remaining),
                              p1: formatCost(q.total_pool),
                            })}
                          />
                        </>
                      ) : (
                        <span className="text-[10px] text-muted-foreground">
                          {quotaLoading ? t("accounts.loadingUsage") : t("accounts.noBillingData")}
                        </span>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <Button variant="outline" size="sm" onClick={openLoginDialog}>
              <ExternalLink />
              {t("accounts.loginViaBrowser")}
            </Button>
            {showAccountInput ? (
              <div className="flex gap-2">
                <Input
                  value={accountInput}
                  onChange={(e) => setAccountInput(e.target.value)}
                  placeholder="user_xxxxxxxxx"
                  className="font-mono"
                />
                <Button onClick={addAccount}>{t("accounts.add")}</Button>
                <Button
                  variant="ghost"
                  onClick={() => {
                    setAccountInput("");
                    setShowAccountInput(false);
                  }}
                >
                  {t("common.cancel")}
                </Button>
              </div>
            ) : (
              <Button variant="outline" size="sm" onClick={() => setShowAccountInput(true)}>
                <Plus />
                {t("accounts.addAccount")}
              </Button>
            )}
          </div>
        </CardContent>
      </Card>

      {/* 账户额度详情弹窗 */}
      <Dialog
        open={detailAccount !== null}
        onOpenChange={(open) => {
          if (!open) setDetailAccount(null);
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t("accounts.detailTitle")}</DialogTitle>
            <DialogDescription>{detailAccount?.userName || detailAccount?.masked}</DialogDescription>
          </DialogHeader>
          <div className="py-2">
            {detailLoading ? (
              <p className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
                <Loader2 className="h-4 w-4 animate-spin" />
                {t("accounts.loadingDetail")}
              </p>
            ) : detailError ? (
              <p className="py-4 text-sm text-destructive">{errText(detailError)}</p>
            ) : detailQuota ? (
              <QuotaDetail quota={detailQuota} />
            ) : null}
          </div>
          <DialogFooter>
            <Button variant="secondary" onClick={() => setDetailAccount(null)}>
              {t("common.close")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 浏览器授权登录弹窗 */}
      <Dialog
        open={loginOpen}
        onOpenChange={(open) => {
          if (!open) closeLoginDialog();
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t("accounts.loginTitle")}</DialogTitle>
            <DialogDescription>
              {loginStatus === "pending"
                ? t("accounts.loginPendingDesc")
                : loginStatus === "success"
                  ? t("accounts.loginSuccessDesc")
                  : t("accounts.loginIdleDesc")}
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col items-center gap-4 py-4">
            {loginStatus === "pending" && (
              <>
                <Loader2 className="h-8 w-8 animate-spin text-primary" />
                <p className="text-center text-sm text-muted-foreground">
                  {t("accounts.loginWaiting")}
                </p>
                <Button onClick={openAuthBrowser}>
                  <ExternalLink />
                  {t("accounts.openBrowserAuth")}
                </Button>
              </>
            )}
            {loginStatus === "success" && (
              <p className="text-center text-sm text-emerald-600 dark:text-emerald-400">
                {t("accounts.loginSuccessMessage")}
              </p>
            )}
            {loginStatus === "denied" && (
              <p className="text-center text-sm text-amber-600 dark:text-amber-400">
                {t("accounts.loginDenied")}
              </p>
            )}
            {loginStatus === "failed" && (
              <p className="text-center text-sm text-destructive">
                {t("accounts.loginFailedPrefix", { p0: errText(loginError) || t("common.unknown") })}
              </p>
            )}
          </div>
          <DialogFooter>
            <Button variant="ghost" onClick={closeLoginDialog}>
              {t("common.close")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
