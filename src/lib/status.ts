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
    default:
      return "stopped";
  }
}

/** 请求状态字符串 → 中文文案。 */
export function statusLabel(status: string): string {
  switch (status) {
    case "streaming":
      return "流式中";
    case "ok":
      return "成功";
    case "timeout":
      return "超时";
    case "error":
      return "错误";
    case "disconnect":
      return "断连";
    default:
      return status;
  }
}
