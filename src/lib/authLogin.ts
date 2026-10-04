import { api, type AuthLoginPoll } from "@/lib/api";

/**
 * 浏览器授权登录的轮询状态机（模块级单例）。
 *
 * 账户的落库由后端 `auth_login_poll` 命令完成，因此只要轮询在跑，授权结果就不会丢。
 * 若把轮询放在账户页的 `useEffect` 里，用户切换视图卸载该页后就再也没有 poll，
 * 浏览器里明明授权成功、账户却永远不会入库，loopback 服务器也会一直空等。
 * 故把轮询提到模块级：与 React 生命周期解耦，只有一个定时器，视图只做订阅。
 */

/** 轮询结果（含未开始时的 idle）。 */
export type LoginSnapshot = AuthLoginPoll;

type Listener = (snap: LoginSnapshot) => void;

/** 轮询间隔（毫秒）。 */
const POLL_INTERVAL_MS = 1000;

/** 当前登录快照（模块级单例，视图只读订阅）。 */
let snapshot: LoginSnapshot = { status: "idle" };
/** 后台轮询定时器句柄（null 表示未在轮询）。 */
let timer: ReturnType<typeof setInterval> | null = null;
/** 登录状态订阅者集合（视图挂载时注册、卸载时移除）。 */
const listeners = new Set<Listener>();
/**
 * 轮询世代号：每次开始/取消自增。
 *
 * `pollOnce` 是异步写，await 返回后必须校验「本次轮询是否仍然有效」，否则：
 * - 取消登录后在途的 pollOnce 会把 `{status:"pending"}` 写回，轮询却已停 → 弹窗永久卡死；
 * - 重新发起登录时旧轮询的结果会覆盖新一轮的快照。
 */
let pollGen = 0;

/** 通知所有订阅者当前快照。 */
function emit() {
  for (const fn of listeners) fn(snapshot);
}

/** 停止轮询（不清空快照）。 */
function stopPolling() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
}

/** 单次轮询：终态时停止定时器；网络失败保留定时器等待下一轮。
 *
 *  世代失效（期间发起了新登录或被取消）时直接丢弃结果，不写快照也不 emit。 */
async function pollOnce() {
  const gen = pollGen;
  try {
    const r = await api.authLoginPoll();
    if (gen !== pollGen) return;
    const terminal = r.status === "success" || r.status === "denied" || r.status === "failed";
    snapshot = r;
    if (terminal) stopPolling();
    // 先按终态快照通知订阅者（视图据此展示成功/拒绝/失败并刷新列表），再把
    // 模块快照归一为 idle：否则 success 会永久留在快照里，再次进入账户页时
    // 按快照重建弹窗并重放成功弹窗与 toast（须手动点「关闭」才会复位）。
    // 归一后不再 emit，避免刚处理完终态又被 idle 覆盖掉 UI 状态。
    emit();
    if (terminal) snapshot = { status: "idle" };
  } catch {
    /* 轮询失败则下一轮再试 */
  }
}

/** 订阅登录状态变化，返回取消订阅函数（不停止后台轮询）。 */
export function subscribeLogin(fn: Listener): () => void {
  listeners.add(fn);
  fn(snapshot);
  return () => {
    listeners.delete(fn);
  };
}

/** 读取当前登录快照。 */
export function loginSnapshot(): LoginSnapshot {
  return snapshot;
}

/** 开始轮询登录结果（重复调用不会产生第二个定时器）。 */
export function startLoginPolling() {
  // 世代递增：使上一轮遗留的在途 pollOnce 结果失效
  pollGen += 1;
  snapshot = { status: "pending" };
  emit();
  stopPolling();
  timer = setInterval(() => {
    void pollOnce();
  }, POLL_INTERVAL_MS);
}

/** 取消登录：停止轮询、通知后端关闭 loopback 会话，并把快照重置为 idle。 */
export async function cancelLoginPolling() {
  // 世代递增：让取消时仍在 await 上的 pollOnce 无法把 pending 写回
  pollGen += 1;
  stopPolling();
  snapshot = { status: "idle" };
  emit();
  try {
    await api.authLoginCancel();
  } catch {
    /* 忽略取消失败 */
  }
}
