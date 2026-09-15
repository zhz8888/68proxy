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

let snapshot: LoginSnapshot = { status: "idle" };
let timer: ReturnType<typeof setInterval> | null = null;
const listeners = new Set<Listener>();

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

/** 单次轮询：终态时停止定时器；网络失败保留定时器等待下一轮。 */
async function pollOnce() {
  try {
    const r = await api.authLoginPoll();
    snapshot = r;
    if (r.status === "success" || r.status === "denied" || r.status === "failed") {
      stopPolling();
    }
    emit();
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
  snapshot = { status: "pending" };
  emit();
  stopPolling();
  timer = setInterval(() => {
    void pollOnce();
  }, POLL_INTERVAL_MS);
}

/** 取消登录：停止轮询、通知后端关闭 loopback 会话，并把快照重置为 idle。 */
export async function cancelLoginPolling() {
  stopPolling();
  snapshot = { status: "idle" };
  emit();
  try {
    await api.authLoginCancel();
  } catch {
    /* 忽略取消失败 */
  }
}
