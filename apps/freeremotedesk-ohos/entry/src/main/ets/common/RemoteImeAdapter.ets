import { inputMethod } from '@kit.IMEKit';
import { UIContext } from '@kit.ArkUI';
import { BusinessError } from '@kit.BasicServicesKit';
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
// 固定数值阶段：1请求 2门 3控制器 4订阅 5附着 6来源 7Owner 8selection 9ready 10关闭 11清理 12显示键盘。
// outcome：0开始 1等待 2完成 3同步抛错 4异步拒绝 5非零 6超时 7失效 8进程封闭 9主动关闭 10上限。
export interface ImeDiagnostic {
  attempt: number; stage: number; outcome: number; code: number; subscriptionIndex: number;
  gateMask: number; progressMask: number; ownerState: number; ticketEqual: boolean;
}
const IME_NO_CODE: number = -2147483648;
let attemptSerial: number = 0;
function imeErrorCode(error: BusinessError): number {
  try {
    const code = error?.code;
    return Number.isSafeInteger(code) && code >= -2147483647 && code <= 2147483647 ? code : IME_NO_CODE;
  } catch { return IME_NO_CODE; }
}
interface ImeSourceEnvelope { abiVersion: number; state: string; ticket: ImeTicket; }
interface ImeOperationEnvelope { abiVersion: number; ticket: ImeTicket; serial: number; kind: string; text?: string; key?: string; }
// 模块寿命超过组件/连接，SDK singleton 的所有 attach/detach 串行；不按旧 resolve 重贴来源。
let editorSerial: number = 0;
let sdkWork: Promise<void> = Promise.resolve();
let sdkBlocked: string = '';
interface ImeSdkOperation {
  readonly owner: RemoteImeAdapter; readonly attempt: number; readonly stage: number;
}
let sdkOperation: ImeSdkOperation | undefined = undefined;
function sameTicket(a?: ImeTicket | null, b?: ImeTicket | null): boolean {
  return !!a && !!b && a.windowStage === b.windowStage && a.windowId === b.windowId && a.editorSerial === b.editorSerial && a.session === b.session && a.generation === b.generation;
}
class ImeListeners {
  private registrations: number = 0;
  private registrationCode: number = IME_NO_CODE;
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
  on(c: inputMethod.InputMethodController): boolean {
    try {
      this.registrations = 1; c.on('insertText', this.insert); this.registrations = 2; c.on('setPreviewText', this.preview); this.registrations = 3; c.on('finishTextPreview', this.finish);
      this.registrations = 4; c.on('deleteLeft', this.leftDelete); this.registrations = 5; c.on('deleteRight', this.rightDelete); this.registrations = 6; c.on('moveCursor', this.move);
      this.registrations = 7; c.on('selectByRange', this.select); this.registrations = 8; c.on('selectByMovement', this.selectMove); this.registrations = 9; c.on('sendFunctionKey', this.functionKey);
      this.registrations = 10; c.on('handleExtendAction', this.extend); this.registrations = 11; c.on('sendKeyboardStatus', this.status);
      this.registrations = 12; c.on('getLeftTextOfCursor', this.left); this.registrations = 13; c.on('getRightTextOfCursor', this.right); this.registrations = 14; c.on('getTextIndexAtCursor', this.index);
      return true;
    } catch (error) { this.registrationCode = imeErrorCode(error as BusinessError); return false; }
  }
  subscriptionIndex(): number { return this.registrations; }
  subscriptionCode(): number { return this.registrationCode; }
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
  ownerConfirmed: boolean = false;
  showStarted: boolean = false;
  showCompleted: boolean = false;
  selectionStarted: boolean = false;
  selectionPending: boolean = false;
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
  readonly showKeyboard: boolean;
  constructor(ticket: ImeTicket, controller: inputMethod.InputMethodController, deadline: number, showKeyboard: boolean,
    grapheme: GraphemeProvider, output: (operation: ImeProjection) => boolean, unavailable: () => void, changed: () => void) {
    this.ticket = ticket; this.controller = controller; this.deadline = deadline; this.showKeyboard = showKeyboard;
    this.policy = new RemoteImePolicy(grapheme, output, unavailable); this.listeners = new ImeListeners(this.policy, changed);
  }
}
export class RemoteImeAdapter {
  private attempt: number = 0;
  private diagnosticCount: number = 0;
  private diagnosticClosed: boolean = true;
  private waiting: boolean = false;
  private gateMask: number = 0;
  private ownerState: number = 0;
  private ticketEqual: boolean = false;
  private diagnostic: (event: ImeDiagnostic) => void;
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
    grapheme: GraphemeProvider, notice: (message: string) => void, diagnostic: (event: ImeDiagnostic) => void = () => {}) {
    this.controller = controller; this.bridge = bridge; this.grapheme = grapheme; this.notice = notice; this.diagnostic = diagnostic;
  }
  private record(stage: number, outcome: number, code: number = IME_NO_CODE, subscriptionIndex: number = 0,
    terminal: boolean = false): void {
    if (this.diagnosticClosed || (!terminal && this.diagnosticCount >= 24)) { return; }
    this.diagnosticCount++;
    const a = this.attachment;
    const event: ImeDiagnostic = { attempt: this.attempt, stage: stage, outcome: outcome, code: code, subscriptionIndex: subscriptionIndex,
      gateMask: this.gateMask, ownerState: this.ownerState, ticketEqual: this.ticketEqual,
      progressMask: (a?.sdkAttached ? 1 : 0) | (a?.published ? 2 : 0) | (a?.ready ? 4 : 0) |
        (a?.attachAttempted ? 8 : 0) | (a?.attaching ? 16 : 0) | (sdkOperation ? 32 : 0) |
        (sdkBlocked.length > 0 ? 64 : 0) | (a?.selectionPending ? 128 : 0) };
    if (terminal) { this.diagnosticClosed = true; }
    // 诊断是旁路，callback/getter/日志故障不能影响输入状态或SDK串行链。
    try { this.diagnostic(event); } catch { /* 隔离内部诊断故障。 */ }
  }
  mode(): boolean { return this.wanted; }
  loading(): boolean { return this.wanted && !this.attachment?.ready; }
  queuedSerial(): number { return this.attachment?.queued ?? 0; }
  acceptedSerial(): number { return this.attachment?.accepted ?? 0; }
  dispatchedSerial(): number { return this.attachment?.dispatched ?? 0; }
  request(context: UIContext, showKeyboard: boolean, now: number): void {
    this.close(5);
    attemptSerial = attemptSerial < 2147483647 ? attemptSerial + 1 : 1;
    this.attempt = attemptSerial; this.diagnosticCount = 0; this.diagnosticClosed = false;
    this.waiting = false; this.gateMask = 0; this.ownerState = 0; this.ticketEqual = false;
    this.record(1, 0);
    if (sdkBlocked.length > 0) { this.record(1, 8, IME_NO_CODE, 0, true); this.notice(sdkBlocked); return; }
    this.context = context; this.showKeyboard = showKeyboard; this.wanted = true; this.deadline = now + 2000;
  }
  private source(ticket: ImeTicket, state: string): number {
    const envelope: ImeSourceEnvelope = { abiVersion: 1, state: state, ticket: ticket };
    return this.bridge.source(JSON.stringify(envelope));
  }
  close(reason: number = 0): void {
    const cleanupAttempt = this.attempt;
    this.record(10, 9, reason, 0, true);
    this.wanted = false; this.context = undefined;
    const a = this.attachment; this.attachment = undefined;
    if (!a) { return; }
    // 先同步屏障，再 C 来源撤销，再排队 SDK cleanup。旧闭包只看到失效 policy。
    a.policy.invalidate(); a.pending = []; a.bytes = 0;
    if (a.published) {
      try { if (this.source(a.ticket, 'detached') !== 0) { this.notice('系统输入法关闭失败，请重新连接'); } }
      catch { this.notice('系统输入法关闭失败，请重新连接'); }
    }
    sdkWork = sdkWork.then(async (): Promise<void> => {
      if (!a.listeners.off(a.controller)) { sdkBlocked = '系统输入法清理失败，请重新打开应用'; this.notice(sdkBlocked); }
      if (a.attachAttempted) {
        try { await this.waitForSdk(a.controller.detach(), 11, cleanupAttempt); } catch { if (a.sdkAttached) { sdkBlocked = '系统输入法清理失败，请重新打开应用'; this.notice(sdkBlocked); } }
      }
    });
  }
  private timeoutSdk(operation?: ImeSdkOperation): void {
    sdkBlocked = '系统输入法未响应，请重新打开应用';
    const own = operation?.owner === this && operation.attempt === this.attempt;
    // 新请求只受旧 singleton 操作封闭，不能冒称自己的 attach/cleanup 超时。
    this.record(own && operation ? operation.stage : 1, own ? 6 : 8, IME_NO_CODE, 0, true);
    this.close(); this.notice(sdkBlocked);
  }
  private async waitForSdk(operation: Promise<void>, stage: number, attempt: number): Promise<void> {
    // kind/归属在发起或cleanup入队时固定；watchdog不race原Promise或解开串行链。
    const pending: ImeSdkOperation = { owner: this, attempt: attempt, stage: stage };
    sdkOperation = pending;
    const timer = setTimeout(() => { this.timeoutSdk(pending); }, 2000);
    try { await operation; }
    finally { clearTimeout(timer); if (sdkOperation === pending) { sdkOperation = undefined; } }
  }
  private fail(stage: number = 2, outcome: number = 7, code: number = IME_NO_CODE, index: number = 0): void {
    this.record(stage, outcome, code, index, true); this.close(); this.notice('系统输入法暂时不可用，请重新点击键盘');
  }
  private changed(a: ImeAttachment): void {
    if (this.attachment !== a || !a.sdkAttached || !a.published) { return; }
    const initial = !a.selectionStarted;
    a.selectionStarted = true;
    if (initial) { a.selectionPending = true; this.record(8, 1); }
    try {
      const selection = a.policy.selection();
      a.controller.changeSelection(a.policy.context(), selection.start, selection.end).then(() => {
        if (this.attachment === a && initial) { a.selectionPending = false; this.record(8, 2); this.prepareReady(a); }
      }).catch((error: BusinessError) => {
        if (this.attachment === a) { this.fail(8, 4, imeErrorCode(error)); }
      });
    } catch (error) { this.fail(8, 3, imeErrorCode(error as BusinessError)); }
  }
  private prepareReady(a: ImeAttachment): void {
    if (this.attachment !== a || a.ready || !a.published || !a.ownerConfirmed ||
      !a.selectionStarted || a.selectionPending) { return; }
    if (a.showKeyboard && !a.showCompleted) {
      if (a.showStarted) { return; }
      a.showStarted = true;
      const showAttempt = this.attempt;
      sdkWork = sdkWork.then(async (): Promise<void> => {
        if (this.attachment !== a) { return; }
        if (sdkBlocked.length > 0) { this.record(1, 8, IME_NO_CODE, 0, true); this.close(); this.notice(sdkBlocked); return; }
        this.record(12, 0);
        let showing: Promise<void>;
        try { showing = a.controller.showTextInput(); }
        catch (error) { this.fail(12, 3, imeErrorCode(error as BusinessError)); return; }
        this.record(12, 1);
        try { await this.waitForSdk(showing, 12, showAttempt); }
        catch (error) {
          if (this.attachment === a) { this.fail(12, 4, imeErrorCode(error as BusinessError)); }
          return;
        }
        if (this.attachment !== a) { return; }
        a.showCompleted = true; this.record(12, 2); this.prepareReady(a);
      });
      return;
    }
    a.ready = true; this.record(9, 2);
    const pending = a.pending; a.pending = []; a.bytes = 0;
    for (const operation of pending) { if (!this.project(a, operation)) { pending.length = 0; return; } }
    pending.length = 0;
  }
  private project(a: ImeAttachment, operation: ImeProjection): boolean {
    if (this.attachment !== a) { return false; }
    const bytes = operation.text === undefined ? 0 : imeTextBytes(operation.text);
    if (!a.ready) {
      if (a.pending.length >= 32 || bytes < 0 || a.bytes + bytes > 16384) { this.fail(6, 10); return false; }
      a.pending.push(operation); a.bytes += bytes; return true;
    }
    const envelope: ImeOperationEnvelope = { abiVersion: 1, ticket: a.ticket, serial: operation.serial, kind: operation.kind };
    if (operation.text !== undefined) { envelope.text = operation.text; }
    if (operation.key !== undefined) { envelope.key = operation.key; }
    let code = -1;
    try { code = this.bridge.operation(JSON.stringify(envelope)); } catch { code = -1; }
    if (code !== 0) { this.fail(6, 5, code); return false; }
    a.queued = operation.serial; return true;
  }
  observe(snapshot: ImeSnapshot, focused: boolean, foreground: boolean, now: number): void {
    if (!this.wanted) { return; }
    if (sdkBlocked.length > 0) { this.record(1, 8, IME_NO_CODE, 0, true); this.close(); this.notice(sdkBlocked); return; }
    const s = snapshot.ime; const display = snapshot.localDisplayEvidence; const a = this.attachment;
    const valid = foreground && snapshot.page === 3 && s?.installed && s.protocolTextInput && display &&
      Number.isSafeInteger(display.stage) && display.stage > 0 && Number.isSafeInteger(display.windowId) && display.windowId > 0 &&
      !!snapshot.session && !!snapshot.generation;
    this.gateMask = (foreground ? 1 : 0) | (snapshot.page === 3 ? 2 : 0) | (s?.installed ? 4 : 0) |
      (s?.protocolTextInput ? 8 : 0) | (display && Number.isSafeInteger(display.stage) && display.stage > 0 &&
        Number.isSafeInteger(display.windowId) && display.windowId > 0 ? 16 : 0) |
      (snapshot.session ? 32 : 0) | (snapshot.generation ? 64 : 0) | (focused ? 128 : 0) |
      (s?.sourceAvailable ? 256 : 0) | (this.context ? 512 : 0) |
      (a && a.ticket.windowStage === display?.stage ? 1024 : 0) | (a && a.ticket.windowId === display?.windowId ? 2048 : 0) |
      (a && a.ticket.session === snapshot.session ? 4096 : 0) | (a && a.ticket.generation === snapshot.generation ? 8192 : 0);
    this.ownerState = s?.state === 'attached' ? 1 : s?.state === 'detached' ? 0 : 2;
    this.ticketEqual = sameTicket(s?.ticket, a?.ticket);
    // 请求焦点阶段允许等待真实 onFocus；已经开始的来源不接受任何失焦。
    if (!valid || (a && (!focused || !s?.sourceAvailable || a.ticket.windowStage !== display?.stage || a.ticket.windowId !== display?.windowId ||
      a.ticket.session !== snapshot.session || a.ticket.generation !== snapshot.generation))) { this.fail(); return; }
    if (now >= this.deadline && (!a || !a.ready)) {
      if (a?.attaching || sdkOperation) { this.timeoutSdk(sdkOperation); }
      else { this.fail(a?.published ? 7 : 2, 6); }
      return;
    }
    if (!a) {
      if (!focused || !s?.sourceAvailable || !display || !this.context) { if (!this.waiting) { this.waiting = true; this.record(2, 1); } return; }
      if (editorSerial >= Number.MAX_SAFE_INTEGER || sdkBlocked.length > 0) { this.fail(2, 10); return; }
      const ticket: ImeTicket = { windowStage: display.stage, windowId: display.windowId, editorSerial: ++editorSerial, session: snapshot.session ?? '', generation: snapshot.generation ?? '' };
      let c: inputMethod.InputMethodController;
      this.record(3, 0);
      try { c = this.controller(); } catch (error) { this.fail(3, 3, imeErrorCode(error as BusinessError)); return; }
      const next: ImeAttachment = new ImeAttachment(ticket, c, this.deadline, this.showKeyboard, this.grapheme,
        (operation: ImeProjection): boolean => this.project(next, operation), () => this.notice('该上下文编辑不可用，请重新输入'), () => this.changed(next));
      this.attachment = next; const context = this.context; const attachAttempt = this.attempt;
      sdkWork = sdkWork.then(async (): Promise<void> => {
        if (this.attachment !== next) { return; }
        if (sdkBlocked.length > 0) { this.record(1, 8, IME_NO_CODE, 0, true); this.close(); this.notice(sdkBlocked); return; }
        const config: inputMethod.TextConfig = { inputAttribute: { textInputType: inputMethod.TextInputType.TEXT,
          enterKeyType: inputMethod.EnterKeyType.NEWLINE, consumeKeyEvents: false }, windowId: ticket.windowId };
        const options: inputMethod.AttachOptions = { showKeyboard: false };
        next.attaching = true; next.attachAttempted = true;
        this.record(5, 0);
        let attached: Promise<void>;
        try { attached = c.attachWithUIContext(context, config, options); }
        catch (error) { next.attaching = false; this.fail(5, 3, imeErrorCode(error as BusinessError)); return; }
        this.record(5, 1);
        try { await this.waitForSdk(attached, 5, attachAttempt); }
        catch (error) {
          next.attaching = false;
          if (this.attachment === next) { this.fail(5, 4, imeErrorCode(error as BusinessError)); }
          return;
        }
        next.attaching = false; next.sdkAttached = true;
        if (this.attachment !== next) { return; }
        this.record(5, 2);
        this.record(4, 0);
        if (!next.listeners.on(c)) { this.fail(4, 3, next.listeners.subscriptionCode(), next.listeners.subscriptionIndex()); return; }
        if (this.attachment !== next) { return; }
        let sourceCode: number;
        try { sourceCode = this.source(ticket, 'attached'); }
        catch (error) { this.fail(6, 3, imeErrorCode(error as BusinessError)); return; }
        if (sourceCode !== 0) { this.fail(6, 5, sourceCode); return; }
        next.published = true; this.record(6, 2); this.record(7, 1); this.changed(next);
      });
      return;
    }
    if (a.published && s?.state === 'attached' && sameTicket(s.ticket, a.ticket)) {
      if (!a.ownerConfirmed) { a.ownerConfirmed = true; this.record(7, 2); }
      this.prepareReady(a);
    } else if (a.ownerConfirmed) { this.fail(7, 7); return; }
    if (sameTicket(s?.ackTicket, a.ticket)) {
      const accepted = s?.lastAcceptedSerial ?? 0; const dispatched = s?.lastDispatchedSerial ?? 0;
      if (!Number.isSafeInteger(accepted) || !Number.isSafeInteger(dispatched) || accepted < a.accepted || dispatched < a.dispatched || dispatched > accepted || accepted > a.queued) { this.fail(7, 10); return; }
      a.accepted = accepted; a.dispatched = dispatched;
    }
  }
}
