// 纯 Harmony shell 策略。时间由调用者注入系统单调 uptime，禁止使用墙钟。
export enum IslandPin { Hover, Press, Focus, Tooltip, Details, Error }
export enum IslandInput { Mouse, Touch, Keyboard }

export class SessionIslandPolicy {
  private page: number = 0;
  private session: string = '';
  private active: boolean = false;
  private revision: number = 0;
  private open: boolean = false;
  private deadline: number = -1;
  private input: IslandInput = IslandInput.Touch;
  private pins: Set<IslandPin> = new Set<IslandPin>();

  observe(page: number, session: string, active: boolean, now: number): void {
    const enteredRemote = page === 3 && this.page !== 3;
    const leftRemote = this.page === 3 && page !== 3;
    const boundary = enteredRemote || leftRemote || active !== this.active || (page === 0 && this.page !== 0) ||
      (page === 1 && (this.page === 0 || this.page === 5)) || (page === 3 && session !== this.session);
    this.page = page;
    this.session = session;
    this.active = active;
    if (boundary) {
      this.revision++;
      this.pins.clear();
      this.input = IslandInput.Touch;
      this.deadline = -1;
      this.open = page !== 0 && active;
    }
    if (!active || page === 0) { this.open = false; this.deadline = -1; return; }
    if (page !== 3) { this.open = true; this.deadline = -1; return; }
    if (boundary || enteredRemote) { this.open = true; this.schedule(now); }
  }

  epoch(): number { return this.revision; }
  shown(): boolean { return this.active && this.page !== 0; }
  expanded(): boolean { return this.shown() && this.open; }

  reveal(input: IslandInput, now: number): void {
    if (!this.shown()) { return; }
    this.input = input;
    this.open = true;
    this.schedule(now);
  }

  pin(pin: IslandPin, value: boolean, now: number): void {
    const previous = this.pins.has(pin);
    if (previous === value) { return; }
    if (value) { this.pins.add(pin); } else { this.pins.delete(pin); }
    if (value && pin === IslandPin.Error && this.shown()) { this.open = true; }
    this.schedule(now);
  }

  private schedule(now: number): void {
    this.deadline = this.expanded() && this.page === 3 && this.pins.size === 0 ?
      now + (this.input === IslandInput.Mouse ? 700 : 3000) : -1;
  }

  advance(now: number, epoch: number): void {
    if (epoch !== this.revision || this.deadline < 0 || now < this.deadline) { return; }
    this.open = false;
    this.deadline = -1;
  }

  collapse(): void {
    if (this.page !== 3 || this.pins.has(IslandPin.Press) || this.pins.has(IslandPin.Details) || this.pins.has(IslandPin.Error)) { return; }
    this.open = false;
    this.deadline = -1;
  }
}

// Local/Remote 所有权不由岛可见性决定；本地尾键须在真实远程交互后仍隔离到抬起。
export class ShellKeyboardOwnership {
  private local: boolean = true;
  private held: Set<number> = new Set<number>();
  private quarantined: Set<number> = new Set<number>();

  enterLocal(): void {
    this.local = true;
    this.held.forEach((code: number) => { this.quarantined.add(code); });
  }
  enterRemote(): void { this.local = false; }
  isLocal(): boolean { return this.local; }
  isHeld(code: number): boolean { return this.held.has(code); }
  systemModifierHeld(): boolean { return this.held.has(2076) || this.held.has(2077); }
  key(code: number, down: boolean): boolean {
    const isolated = this.quarantined.has(code);
    if (down) {
      this.held.add(code);
      if (this.local || this.quarantined.size > 0) { this.quarantined.add(code); }
    } else {
      this.held.delete(code);
      this.quarantined.delete(code);
    }
    return isolated;
  }
  canSend(): boolean { return !this.local && this.quarantined.size === 0; }
  reset(): void { this.local = true; this.held.clear(); this.quarantined.clear(); }
}

export enum SurfaceTouchPhase { Down, Move, Up, Cancel }
export enum SurfaceTouchDecision { Ignore, Restore, Release }
export class SurfaceTouchOwnership {
  private contacts: number = 0;
  observe(phase: SurfaceTouchPhase, contacts: number): SurfaceTouchDecision {
    const previous = this.contacts;
    this.contacts = Math.max(0, contacts);
    if (phase === SurfaceTouchPhase.Cancel) { this.contacts = 0; return SurfaceTouchDecision.Release; }
    if (contacts > 1 || contacts < 0 || (phase === SurfaceTouchPhase.Down && contacts !== 1)) { return SurfaceTouchDecision.Release; }
    // 多点退为单点的尾部仍属于旧手势；必须全部结束后重新单点按下。
    if (phase === SurfaceTouchPhase.Down && contacts === 1 && previous === 0) { return SurfaceTouchDecision.Restore; }
    return SurfaceTouchDecision.Ignore;
  }
  reset(): void { this.contacts = 0; }
}
export interface DialogCallbackTicket { serial: number; epoch: number; }
export class DialogCallbackScope {
  private serial: number = 0;
  private open: boolean = false;
  begin(epoch: number): DialogCallbackTicket { this.open = true; return { serial: ++this.serial, epoch: epoch }; }
  invalidate(): void { this.serial++; this.open = false; }
  accepts(ticket: DialogCallbackTicket, epoch: number, active: boolean): boolean {
    return this.open && active && ticket.serial === this.serial && ticket.epoch === epoch;
  }
  observeKey(ticket: DialogCallbackTicket, epoch: number, active: boolean, keyboard: ShellKeyboardOwnership, code: number, down: boolean): boolean {
    if (!this.accepts(ticket, epoch, active)) { return false; }
    keyboard.key(code, down); return true;
  }
}

export interface QueuedKeySource { session: string; generation: string; revision: string; }
export interface QueuedKeyCleanup extends QueuedKeySource { code: number; hid: number; }
interface QueuedKeyEntry extends QueuedKeySource { hid: number; isolated: boolean; released: boolean; }
export class QueuedRemoteKeys {
  private current?: QueuedKeySource;
  private entries: Map<number, QueuedKeyEntry> = new Map<number, QueuedKeyEntry>();
  private valid(value: string): boolean {
    return /^[1-9][0-9]*$/.test(value) && value.length <= 20 && (value.length < 20 || value <= '18446744073709551615');
  }
  private sameSource(a: QueuedKeySource, b: QueuedKeySource): boolean { return a.session === b.session && a.generation === b.generation; }
  setEpoch(session: string, generation: string, revision: string): void {
    if (!this.valid(session) || !this.valid(generation) || !this.valid(revision)) {
      this.current = undefined; this.isolate(); return;
    }
    const next: QueuedKeySource = { session: session, generation: generation, revision: revision };
    if (!this.current || !this.sameSource(this.current, next)) { this.isolate(); }
    this.current = next;
    // page0/新会话只变更current来源；可信旧Down仍保留到其真实物理Up。
  }
  allowsDown(code: number): boolean {
    const entry = this.entries.get(code);
    return !entry || (!!this.current && this.sameSource(entry, this.current) && !entry.isolated && !entry.released);
  }
  recordDown(code: number, hid: number, accepted: boolean): void {
    if (!accepted || !this.current || this.entries.has(code)) { return; }
    this.entries.set(code, { session: this.current.session, generation: this.current.generation, revision: this.current.revision,
      hid: hid, isolated: false, released: false });
  }
  isolate(): void { this.entries.forEach((entry: QueuedKeyEntry) => { entry.isolated = true; }); }
  tailUp(code: number): boolean {
    const entry = this.entries.get(code);
    if (!entry || !entry.isolated) { return false; }
    entry.released = true;
    return true;
  }
  normalUp(code: number, accepted: boolean): void {
    const entry = this.entries.get(code);
    if (!entry || !this.current || !this.sameSource(entry, this.current) || entry.isolated) { return; }
    if (accepted) { this.entries.delete(code); }
    else { entry.isolated = true; entry.released = true; }
  }
  pending(): QueuedKeyCleanup[] {
    const pending: QueuedKeyCleanup[] = [];
    this.entries.forEach((entry: QueuedKeyEntry, code: number) => {
      if (entry.isolated && entry.released) {
        pending.push({ code: code, hid: entry.hid, session: entry.session, generation: entry.generation, revision: entry.revision });
      }
    });
    return pending;
  }
  completeCleanup(cleanup: QueuedKeyCleanup, accepted: boolean): void {
    const entry = this.entries.get(cleanup.code);
    if (accepted && entry && entry.released && this.sameSource(entry, cleanup) && entry.revision === cleanup.revision && entry.hid === cleanup.hid) {
      this.entries.delete(cleanup.code);
    }
  }
  hasPending(): boolean { return this.pending().length > 0; }
}

export function compactDetailsLines(lines: string[]): string {
  return lines.filter((line: string): boolean => line.trim().length > 0).join('\n');
}
