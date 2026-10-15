export interface PocStatus {
  code: number;
  generation: string;
  frames: string;
  attaches: string;
  resizes: string;
  detaches: string;
  width: number;
  height: number;
  attached: boolean;
  foreground: boolean;
}
export const pocAvailable: () => boolean;
export const abiVersion: () => number;
export const setForeground: (foreground: boolean) => number;
export const pocStatus: () => PocStatus;

export const initializeApp: (sandbox: string) => number;
export const shutdownApp: () => number;
export const appSnapshot: () => string;
export const editForm: (json: string) => number;
export const editPassword: (password: string) => number;
export const appAction: (action: number, event: string, flags: number, index: number) => number;
export const appFocus: (foreground: boolean, focused: boolean, remote: boolean) => number;
export const appLocalDisplay: (json: string) => number;
// 仅版本化来源/操作意图；返回 0 为入队，不表示远端接收。
export const appImeSource: (json: string) => number;
export const appImeOperation: (json: string) => number;
export const appGeometry: (width: number, height: number, contentWidth: number, contentHeight: number, scale: number, panX: number, panY: number, density: number) => number;
export const appInput: (session: string, generation: string, revision: string, kind: number, flags: number, x: number, y: number, hid: number, modifiers: number, wheelX: number, wheelY: number) => number;
export const commitText: (session: string, generation: string, revision: string, utf8: string) => number;
export const huksProbe: () => number;

export interface ImeGraphemeNeighbors { isBoundary: boolean; previous: number; next: number; }
export const imeGraphemeNeighbors: (text: string, cursor: number) => ImeGraphemeNeighbors;
