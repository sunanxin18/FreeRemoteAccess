import { inputMethod } from '@kit.IMEKit';
import { UIContext } from '@kit.ArkUI';
import { GraphemeProvider, ImeRange, ImeProjection, RemoteImePolicy, imeTextBytes } from './RemoteImePolicy';
export interface ImeTicket {
  readonly windowStage: number; readonly windowId: number; readonly editorSerial: number; readonly session: string; readonly generation: string;
}
export interface ImeOwnerState {
  installed: boolean; protocolTextInput: boolean; sourceAvailable: boolean; state: string;
  ticket?: ImeTicket | null; ackTicket?: ImeTicket | null; lastAcceptedSerial: number; lastDispatchedSerial: number;
}
export interface ImeDisplaySource { stage: number; windowId: number; }
export interface ImeSnapshot { page: number; session?: string; generation?: string; localDisplayEvidence?: ImeDisplaySource; ime?: ImeOwnerState; }
export interface ImeBridge { source: (json: string) => number; operation: (json: string) => number; }
interface ImeSourceEnvelope { abiVersion: number; state: string; ticket: ImeTicket; }
interface ImeOperationEnvelope { abiVersion: number; ticket: ImeTicket; serial: number; kind: string; text?: string; key?: string; }
// 模块寿命超过组件/连接，SDK singleton 的所有 attach/detach 串行；不按旧 resolve 重贴来源。
let editorSerial: number = 0;
let sdkWork: Promise<void> = Promise.resolve();
let sdkBlocked: string = '';
let sdkBusy: boolean = false;
function sameTicket(a?: ImeTicket | null, b?: ImeTicket | null): boolean {
  return !!a && !!b && a.windowStage === b.windowStage && a.windowId === b.windowId && a.editorSerial === b.editorSerial && a.session === b.session && a.generation === b.generation;
}
class ImeListeners {
  private registrations: number = 0;
  insert = (text: string): void => { this.policy.insert(text); this.changed(); };
  preview = (text: string, range: inputMethod.Range): void => { const local: ImeRange = { start: range.start, end: range.end }; this.policy.preview(text, local); this.changed(); };
  finish = (): void => { this.policy.finish(); this.changed(); };
  leftDelete = (length: number): void => { this.policy.delete(length, true); this.changed(); };
  rightDelete = (length: number): void => { this.policy.delete(length, false); this.changed(); };
  move = (direction: inputMethod.Direction): void => { this.policy.move(direction, false); this.changed(); };
  select = (range: inputMethod.Range): void => { const local: ImeRange = { start: range.start, end: range.end }; this.policy.select(local); this.changed(); };
  selectMove = (movement: inputMethod.Movement): void => { this.policy.move(movement.direction, true); this.changed(); };
  functionKey = (key: inputMethod.FunctionKey): void => { this.policy.functionKey(key.enterKeyType); this.changed(); };
  extend = (action: inputMethod.ExtendAction): void => { this.policy.extend(action); this.changed(); };
  status = (_status: inputMethod.KeyboardStatus): void => { /* HIDE 不等同 blur，不撤销有效 F。 */ };
  left = (length: number): string => this.policy.left(length);
  right = (length: number): string => this.policy.right(length);
  index = (): number => this.policy.index();
  private policy: RemoteImePolicy;
  private changed: () => void;
  constructor(policy: RemoteImePolicy, changed: () => void) { this.policy = policy; this.changed = changed; }
  on(c: inputMethod.InputMethodController): void {
    try {
      this.registrations = 1; c.on('insertText', this.insert); this.registrations = 2; c.on('setPreviewText', this.preview); this.registrations = 3; c.on('finishTextPreview', this.finish);
      this.registrations = 4; c.on('deleteLeft', this.leftDelete); this.registrations = 5; c.on('deleteRight', this.rightDelete); this.registrations = 6; c.on('moveCursor', this.move);
      this.registrations = 7; c.on('selectByRange', this.select); this.registrations = 8; c.on('selectByMovement', this.selectMove); this.registrations = 9; c.on('sendFunctionKey', this.functionKey);
      this.registrations = 10; c.on('handleExtendAction', this.extend); this.registrations = 11; c.on('sendKeyboardStatus', this.status);
      this.registrations = 12; c.on('getLeftTextOfCursor', this.left); this.registrations = 13; c.on('getRightTextOfCursor', this.right); this.registrations = 14; c.on('getTextIndexAtCursor', this.index);
    } catch { throw new Error('ime_subscription_unavailable'); }
  }
  off(c: inputMethod.InputMethodController): boolean {
    // 即使某个 off 抛错也尝试移除其余本实例回调；从不 off(event) 清别人的监听。
    let ok = true;
    if (this.registrations >= 1) { try { c.off('insertText', this.insert); } catch { ok = false; } }
    if (this.registrations >= 2) { try { c.off('setPreviewText', this.preview); } catch { ok = false; } }
    if (this.registrations >= 3) { try { c.off('finishTextPreview', this.finish); } catch { ok = false; } }
    if (this.registrations >= 4) { try { c.off('deleteLeft', this.leftDelete); } catch { ok = false; } }
    if (this.registrations >= 5) { try { c.off('deleteRight', this.rightDelete); } catch { ok = false; } }
    if (this.registrations >= 6) { try { c.off('moveCursor', this.move); } catch { ok = false; } }
    if (this.registrations >= 7) { try { c.off('selectByRange', this.select); } catch { ok = false; } }
    if (this.registrations >= 8) { try { c.off('selectByMovement', this.selectMove); } catch { ok = false; } }
    if (this.registrations >= 9) { try { c.off('sendFunctionKey', this.functionKey); } catch { ok = false; } }
    if (this.registrations >= 10) { try { c.off('handleExtendAction', this.extend); } catch { ok = false; } }
    if (this.registrations >= 11) { try { c.off('sendKeyboardStatus', this.status); } catch { ok = false; } }
    if (this.registrations >= 12) { try { c.off('getLeftTextOfCursor', this.left); } catch { ok = false; } }
    if (this.registrations >= 13) { try { c.off('getRightTextOfCursor', this.right); } catch { ok = false; } }
    if (this.registrations >= 14) { try { c.off('getTextIndexAtCursor', this.index); } catch { ok = false; } }
    this.registrations = 0; return ok;
  }
}
class ImeAttachment {
  pending: ImeProjection[] = [];
  bytes: number = 0;
  published: boolean = false;
  ready: boolean = false;
  sdkAttached: boolean = false;
  attaching: boolean = false;
  attachAttempted: boolean = false;
  queued: number = 0;
  accepted: number = 0;
  dispatched: number = 0;
  policy: RemoteImePolicy;
  listeners: ImeListeners;
  readonly ticket: ImeTicket;
  readonly controller: inputMethod.InputMethodController;
  readonly deadline: number;
  constructor(ticket: ImeTicket, controller: inputMethod.InputMethodController, deadline: number,
    grapheme: GraphemeProvider, output: (operation: ImeProjection) => boolean, unavailable: () => void, changed: () => void) {
    this.ticket = ticket; this.controller = controller; this.deadline = deadline;
    this.policy = new RemoteImePolicy(grapheme, output, unavailable); this.listeners = new ImeListeners(this.policy, changed);
  }
}
export class RemoteImeAdapter {
  private wanted: boolean = false;
  private context?: UIContext;
  private showKeyboard: boolean = false;
  private deadline: number = 0;
  private attachment?: ImeAttachment;
  private controller: () => inputMethod.InputMethodController;
  private bridge: ImeBridge;
  private grapheme: GraphemeProvider;
  private notice: (message: string) => void;
  constructor(controller: () => inputMethod.InputMethodController, bridge: ImeBridge,
    grapheme: GraphemeProvider, notice: (message: string) => void) {
    this.controller = controller; this.bridge = bridge; this.grapheme = grapheme; this.notice = notice;
  }
  mode(): boolean { return this.wanted; }
  loading(): boolean { return this.wanted && !this.attachment?.ready; }
  queuedSerial(): number { return this.attachment?.queued ?? 0; }
  acceptedSerial(): number { return this.attachment?.accepted ?? 0; }
  dispatchedSerial(): number { return this.attachment?.dispatched ?? 0; }
  request(context: UIContext, showKeyboard: boolean, now: number): void {
    this.close();
    if (sdkBlocked.length > 0) { this.notice(sdkBlocked); return; }
    this.context = context; this.showKeyboard = showKeyboard; this.wanted = true; this.deadline = now + 2000;
  }
  private source(ticket: ImeTicket, state: string): number {
    const envelope: ImeSourceEnvelope = { abiVersion: 1, state: state, ticket: ticket };
    try { return this.bridge.source(JSON.stringify(envelope)); } catch { return -1; }
  }
  close(): void {
    this.wanted = false; this.context = undefined;
    const a = this.attachment; this.attachment = undefined;
    if (!a) { return; }
    // 先同步屏障，再 C 来源撤销，再排队 SDK cleanup。旧闭包只看到失效 policy。
    a.policy.invalidate(); a.pending = []; a.bytes = 0;
    if (a.published && this.source(a.ticket, 'detached') !== 0) { this.notice('系统输入法关闭失败，请重新连接'); }
    sdkWork = sdkWork.then(async (): Promise<void> => {
      if (!a.listeners.off(a.controller)) { sdkBlocked = '系统输入法清理失败，请重新打开应用'; this.notice(sdkBlocked); }
      if (a.attachAttempted) {
        try { await this.waitForSdk(a.controller.detach()); } catch { if (a.sdkAttached) { sdkBlocked = '系统输入法清理失败，请重新打开应用'; this.notice(sdkBlocked); } }
      }
    });
  }
  private async waitForSdk(operation: Promise<void>): Promise<void> {
    // watchdog只撤销产品来源/封闭后续attach；不race SDK Promise或解开串行链。
    sdkBusy = true;
    const timer = setTimeout(() => {
      sdkBlocked = '系统输入法未响应，请重新打开应用';
      this.close(); this.notice(sdkBlocked);
    }, 2000);
    try { await operation; }
    finally { clearTimeout(timer); sdkBusy = false; }
  }
  private fail(): void { this.close(); this.notice('系统输入法暂时不可用，请重新点击键盘'); }
  private changed(a: ImeAttachment): void {
    if (this.attachment !== a || !a.sdkAttached) { return; }
    try { const selection = a.policy.selection(); a.controller.changeSelection(a.policy.context(), selection.start, selection.end).catch(() => { if (this.attachment === a) { this.fail(); } }); }
    catch { this.fail(); }
  }
  private project(a: ImeAttachment, operation: ImeProjection): boolean {
    if (this.attachment !== a) { return false; }
    const bytes = operation.text === undefined ? 0 : imeTextBytes(operation.text);
    if (!a.ready) {
      if (a.pending.length >= 32 || bytes < 0 || a.bytes + bytes > 16384) { this.fail(); return false; }
      a.pending.push(operation); a.bytes += bytes; return true;
    }
    const envelope: ImeOperationEnvelope = { abiVersion: 1, ticket: a.ticket, serial: operation.serial, kind: operation.kind };
    if (operation.text !== undefined) { envelope.text = operation.text; }
    if (operation.key !== undefined) { envelope.key = operation.key; }
    let code = -1;
    try { code = this.bridge.operation(JSON.stringify(envelope)); } catch { code = -1; }
    if (code !== 0) { this.fail(); return false; }
    a.queued = operation.serial; return true;
  }
  observe(snapshot: ImeSnapshot, focused: boolean, foreground: boolean, now: number): void {
    if (!this.wanted) { return; }
    if (sdkBlocked.length > 0) { this.close(); this.notice(sdkBlocked); return; }
    const s = snapshot.ime; const display = snapshot.localDisplayEvidence; const a = this.attachment;
    const valid = foreground && snapshot.page === 3 && s?.installed && s.protocolTextInput && display &&
      Number.isSafeInteger(display.stage) && display.stage > 0 && Number.isSafeInteger(display.windowId) && display.windowId > 0 &&
      !!snapshot.session && !!snapshot.generation;
    // 请求焦点阶段允许等待真实 onFocus；已经开始的来源不接受任何失焦。
    if (!valid || (a && (!focused || !s?.sourceAvailable || a.ticket.windowStage !== display?.stage || a.ticket.windowId !== display?.windowId ||
      a.ticket.session !== snapshot.session || a.ticket.generation !== snapshot.generation))) { this.fail(); return; }
    if (now >= this.deadline && (!a || !a.ready)) {
      if (a?.attaching || sdkBusy) { sdkBlocked = '系统输入法未响应，请重新打开应用'; this.close(); this.notice(sdkBlocked); }
      else { this.fail(); }
      return;
    }
    if (!a) {
      if (!focused || !s?.sourceAvailable || !display || !this.context) { return; }
      if (editorSerial >= Number.MAX_SAFE_INTEGER || sdkBlocked.length > 0) { this.fail(); return; }
      const ticket: ImeTicket = { windowStage: display.stage, windowId: display.windowId, editorSerial: ++editorSerial, session: snapshot.session ?? '', generation: snapshot.generation ?? '' };
      let c: inputMethod.InputMethodController;
      try { c = this.controller(); } catch { this.fail(); return; }
      const next: ImeAttachment = new ImeAttachment(ticket, c, this.deadline, this.grapheme,
        (operation: ImeProjection): boolean => this.project(next, operation), () => this.notice('该上下文编辑不可用，请重新输入'), () => this.changed(next));
      this.attachment = next; const context = this.context; const show = this.showKeyboard;
      sdkWork = sdkWork.then(async (): Promise<void> => {
        if (this.attachment !== next) { return; }
        if (sdkBlocked.length > 0) { this.close(); this.notice(sdkBlocked); return; }
        try {
          next.listeners.on(c);
          const config: inputMethod.TextConfig = { inputAttribute: { textInputType: inputMethod.TextInputType.TEXT,
            enterKeyType: inputMethod.EnterKeyType.NEWLINE, consumeKeyEvents: false }, windowId: ticket.windowId };
          const options: inputMethod.AttachOptions = { showKeyboard: show };
          next.attaching = true; next.attachAttempted = true;
          await this.waitForSdk(c.attachWithUIContext(context, config, options));
          next.attaching = false;
          next.sdkAttached = true;
          if (this.attachment !== next) { return; }
          if (this.source(ticket, 'attached') !== 0) { this.fail(); return; }
          next.published = true; this.changed(next);
        } catch { next.attaching = false; if (this.attachment === next) { this.fail(); } }
      });
      return;
    }
    if (a.published && s?.state === 'attached' && sameTicket(s.ticket, a.ticket)) {
      if (!a.ready) {
        a.ready = true; const pending = a.pending; a.pending = []; a.bytes = 0;
        for (const operation of pending) { if (!this.project(a, operation)) { pending.length = 0; return; } }
        pending.length = 0;
      }
    } else if (a.ready) { this.fail(); return; }
    if (sameTicket(s?.ackTicket, a.ticket)) {
      const accepted = s?.lastAcceptedSerial ?? 0; const dispatched = s?.lastDispatchedSerial ?? 0;
      if (!Number.isSafeInteger(accepted) || !Number.isSafeInteger(dispatched) || accepted < a.accepted || dispatched < a.dispatched || dispatched > accepted || accepted > a.queued) { this.fail(); return; }
      a.accepted = accepted; a.dispatched = dispatched;
    }
  }
}
