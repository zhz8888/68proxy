import type { LampState } from "@/components/StatusLamp";

/** 请求状态字符串 → 指示灯状态的映射（后端下发的 status 为原始字符串，前端仅做视觉翻译）。 */
export function lampForStatus(status: string): LampState {
  switch (status) {
    case "streaming":
      return "streaming";
    case "ok":
      return "ok";
    case "timeout":
      return "timeout";
    case "error":
      return "error";
    // 上游断连与错误同属异常收尾，用错误灯而非「未运行」灰灯，避免语义误导
    case "disconnect":
      return "error";
    default:
      return "stopped";
  }
}

/** 请求状态字符串 → i18n key（调用方用 translate(statusLabel(status)) 得到当前语言文案）。 */
export function statusLabel(status: string): string {
  switch (status) {
    case "streaming":
      return "status.streaming";
    case "ok":
      return "status.ok";
    case "timeout":
      return "status.timeout";
    case "error":
      return "status.error";
    case "disconnect":
      return "status.disconnect";
    // 未知状态原样返回，避免翻译层把真实状态名吞掉
    default:
      return status;
  }
}
