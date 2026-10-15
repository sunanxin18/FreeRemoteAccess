// 原生 display/window 字段已经是 px；此处不调用 vp2px，也不交换真实屏幕宽高。
export interface DisplayRect { left: number; top: number; width: number; height: number; }
// ArkTS 使用名义类型；原生 SDK Rect 逐字段复制，不使用结构赋值或类型强转。
export function makeDisplayRect(left: number, top: number, width: number, height: number): DisplayRect {
  const rect: DisplayRect = { left: left, top: top, width: width, height: height };
  return rect;
}
export function validDisplayWorkArea(rect: DisplayRect, width: number, height: number): boolean {
  return Number.isSafeInteger(rect.left) && Number.isSafeInteger(rect.top) &&
    Number.isSafeInteger(rect.width) && Number.isSafeInteger(rect.height) &&
    rect.left >= 0 && rect.top >= 0 && rect.left <= 2147483647 && rect.top <= 2147483647 &&
    rect.width > 0 && rect.height > 0 && rect.width <= 4294967295 && rect.height <= 4294967295 &&
    Number.isSafeInteger(rect.left + rect.width) && Number.isSafeInteger(rect.top + rect.height) &&
    rect.left + rect.width <= width && rect.top + rect.height <= height;
}
export interface LocalDisplayGeometry {
  displayId: number;
  width: number;
  height: number;
  density: number;
  rotation: number;
  window: DisplayRect;
  drawable: DisplayRect;
  workArea: DisplayRect | null;
  fullscreen: boolean;
}
export interface DisplayTicket { stage: number; revision: number; windowId: number; }
export interface LocalDisplayMessage {
  abiVersion: number;
  stage: number;
  revision: number;
  windowId: number;
  phone: boolean;
  geometry: LocalDisplayGeometry | null;
  reason: string | null;
}
export class LocalDisplayPolicy {
  private stage: number = 1;
  private revision: number = 0;
  private windowId: number = 0;
  stageId(): number { return this.stage; }
  attach(): void { this.stage++; this.windowId = 0; this.revision++; }
  detach(): void { this.stage++; this.windowId = 0; this.revision++; }
  begin(windowId: number): DisplayTicket {
    this.windowId = windowId; this.revision++;
    return { stage: this.stage, revision: this.revision, windowId: windowId };
  }
  isCurrent(ticket: DisplayTicket): boolean {
    return ticket.stage === this.stage && ticket.revision === this.revision && ticket.windowId === this.windowId;
  }
  message(ticket: DisplayTicket, phone: boolean, geometry: LocalDisplayGeometry | null, reason: string | null): LocalDisplayMessage {
    return { abiVersion: 1, stage: ticket.stage, revision: ticket.revision, windowId: ticket.windowId, phone: phone, geometry: geometry, reason: reason };
  }
}
