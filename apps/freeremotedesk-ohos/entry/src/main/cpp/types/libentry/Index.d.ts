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
