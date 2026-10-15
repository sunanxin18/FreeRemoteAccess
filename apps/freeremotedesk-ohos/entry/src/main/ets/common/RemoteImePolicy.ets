// 只掌握当前组合的 UTF-16 scratch；不声称掌握远端全文或选区。
export interface ImeRange { start: number; end: number; }
export interface GraphemeNeighbors { isBoundary: boolean; previous: number; next: number; }
export interface ImeProjection { serial: number; kind: string; text?: string; key?: string; }
export type GraphemeProvider = (text: string, cursor: number) => GraphemeNeighbors;
export function imeTextBytes(text: string): number {
  let bytes = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c >= 0xD800 && c <= 0xDBFF) {
      if (++i >= text.length || text.charCodeAt(i) < 0xDC00 || text.charCodeAt(i) > 0xDFFF) { return -1; }
      bytes += 4;
    } else if (c >= 0xDC00 && c <= 0xDFFF) { return -1; }
    else { bytes += c < 0x80 ? 1 : c < 0x800 ? 2 : 3; }
  }
  return bytes;
}
export function imeScalarBoundary(text: string, position: number): boolean {
  return Number.isSafeInteger(position) && position >= 0 && position <= text.length && !(position > 0 && position < text.length &&
    text.charCodeAt(position - 1) >= 0xD800 && text.charCodeAt(position - 1) <= 0xDBFF && text.charCodeAt(position) >= 0xDC00 && text.charCodeAt(position) <= 0xDFFF);
}
// 两个 ArkUI handler 调用同一个决定：IME 模式下回调独占全部编辑/控制生产。
export function imeOwnsKeyProducer(systemIme: boolean): boolean { return systemIme; }
export class RemoteImePolicy {
  private text: string = '';
  private anchor: number = 0;
  private caret: number = 0;
  private preedit: boolean = false;
  private valid: boolean = true;
  private serial: number = 0;
  private grapheme: GraphemeProvider;
  private output: (operation: ImeProjection) => boolean;
  private unavailable: () => void;
  constructor(grapheme: GraphemeProvider, output: (operation: ImeProjection) => boolean, unavailable: () => void) {
    this.grapheme = grapheme; this.output = output; this.unavailable = unavailable;
  }
  context(): string { return this.text; }
  selection(): ImeRange { return { start: Math.min(this.anchor, this.caret), end: Math.max(this.anchor, this.caret) }; }
  index(): number { return this.caret; }
  private clear(): void { this.text = ''; this.anchor = 0; this.caret = 0; this.preedit = false; }
  invalidate(): void { this.valid = false; this.clear(); }
  private refuse(): void { this.clear(); this.unavailable(); }
  private bounds(range: ImeRange): boolean {
    return range.start <= range.end && imeScalarBoundary(this.text, range.start) && imeScalarBoundary(this.text, range.end);
  }
  private checked(text: string): boolean { const n = imeTextBytes(text); return text.length <= 4096 && n >= 0 && n <= 16384; }
  private replace(value: string, start: number, end: number): boolean {
    const next = this.text.slice(0, start) + value + this.text.slice(end);
    if (!this.checked(next)) { this.refuse(); return false; }
    this.text = next; this.caret = start + value.length; this.anchor = this.caret; return true;
  }
  preview(value: string, range: ImeRange): void {
    if (!this.valid) { return; }
    if (!this.checked(value)) { this.refuse(); return; }
    const sentinel = range.start === -1 && range.end === -1;
    const selected = this.anchor !== this.caret;
    if ((!sentinel && (!this.bounds(range) || selected)) || (!this.preedit && !sentinel && (range.start !== 0 || range.end !== 0))) { this.refuse(); return; }
    const start = sentinel ? selected ? Math.min(this.anchor, this.caret) : 0 : range.start;
    const end = sentinel ? selected ? Math.max(this.anchor, this.caret) : this.text.length : range.end;
    if (this.replace(value, start, end)) { this.preedit = true; }
  }
  private emit(kind: string, text?: string, key?: string): void {
    if (this.serial >= Number.MAX_SAFE_INTEGER) { this.invalidate(); this.unavailable(); return; }
    const operation: ImeProjection = { serial: ++this.serial, kind: kind };
    if (text !== undefined) { operation.text = text; }
    if (key !== undefined) { operation.key = key; }
    // 同步接收入 staging/native queue 后释放 scratch；失败停止当前来源，永不自动重放。
    if (!this.output(operation)) { this.invalidate(); }
  }
  insert(value: string): void {
    if (!this.valid) { return; }
    if (!this.checked(value)) { this.refuse(); return; }
    if (this.preedit) {
      const selected = this.anchor !== this.caret;
      if (!this.replace(value, selected ? Math.min(this.anchor, this.caret) : 0, selected ? Math.max(this.anchor, this.caret) : this.text.length)) { return; }
      this.finish();
    } else { this.clear(); if (value.length > 0) { this.emit('text', value); } }
  }
  finish(): void {
    if (!this.valid || !this.preedit) { return; }
    const value = this.text; this.clear(); if (value.length > 0) { this.emit('text', value); }
  }
  select(range: ImeRange): void {
    if (!this.valid) { return; }
    if (!this.preedit || !this.bounds(range)) { this.refuse(); return; }
    this.anchor = range.start; this.caret = range.end;
  }
  private neighbor(cursor: number, left: boolean): number {
    const n = this.grapheme(this.text, cursor);
    if (!n.isBoundary || !imeScalarBoundary(this.text, n.previous) || !imeScalarBoundary(this.text, n.next) || n.previous > cursor || n.next < cursor ||
      (left && cursor > 0 && n.previous === cursor) || (!left && cursor < this.text.length && n.next === cursor)) { throw new Error('ime_context_unavailable'); }
    return left ? n.previous : n.next;
  }
  delete(length: number, left: boolean): void {
    if (!this.valid) { return; }
    if (!Number.isSafeInteger(length) || length < 1 || length > 4096) { this.refuse(); return; }
    if (!this.preedit) { if (length === 1) { this.emit('keyPair', undefined, left ? 'Backspace' : 'Delete'); } else { this.refuse(); } return; }
    if (this.anchor !== this.caret) { this.replace('', Math.min(this.anchor, this.caret), Math.max(this.anchor, this.caret)); return; }
    let end = this.caret;
    try {
      for (let i = 0; i < length; i++) {
        if ((left && end === 0) || (!left && end === this.text.length)) { this.refuse(); return; }
        end = this.neighbor(end, left);
      }
      this.replace('', Math.min(end, this.caret), Math.max(end, this.caret));
    } catch { this.refuse(); }
  }
  move(direction: number, selecting: boolean): void {
    if (!this.valid) { return; }
    if (!Number.isInteger(direction) || direction < 1 || direction > 4) { this.refuse(); return; }
    if (!this.preedit) {
      if (selecting) { this.refuse(); return; }
      this.emit('keyPair', undefined, direction === 1 ? 'ArrowUp' : direction === 2 ? 'ArrowDown' : direction === 3 ? 'ArrowLeft' : 'ArrowRight'); return;
    }
    if (direction < 3) { this.refuse(); return; }
    try {
      const left = direction === 3;
      const next = !selecting && this.anchor !== this.caret ? (left ? Math.min(this.anchor, this.caret) : Math.max(this.anchor, this.caret)) : this.neighbor(this.caret, left);
      this.caret = next; if (!selecting) { this.anchor = next; }
    } catch { this.refuse(); }
  }
  extend(action: number): void {
    if (!this.valid) { return; }
    if (action === 0 && this.preedit) { this.anchor = 0; this.caret = this.text.length; } else { this.refuse(); }
  }
  functionKey(enterType: number): void {
    if (!this.valid) { return; }
    if (!Number.isInteger(enterType) || enterType < 0 || enterType > 8) { this.refuse(); return; }
    if (enterType === 1) { return; }
    this.finish(); if (this.valid) { this.emit('keyPair', undefined, 'Enter'); }
  }
  left(length: number): string {
    if (!Number.isSafeInteger(length) || length < 0) { return ''; }
    const end = Math.min(this.anchor, this.caret); let start = Math.max(0, end - Math.min(length, 4096));
    if (!imeScalarBoundary(this.text, start)) { start++; } return this.text.slice(start, end);
  }
  right(length: number): string {
    if (!Number.isSafeInteger(length) || length < 0) { return ''; }
    const start = Math.max(this.anchor, this.caret); let end = Math.min(this.text.length, start + Math.min(length, 4096));
    if (!imeScalarBoundary(this.text, end)) { end--; } return this.text.slice(start, end);
  }
}
