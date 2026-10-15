import native from 'libentry.so';
import { KeyboardAvoidMode, LengthMetrics } from '@kit.ArkUI';
import { deviceInfo, systemDateTime } from '@kit.BasicServicesKit';
import { usbHid } from './InputMapping';
import { PHONE_SESSION_ORIENTATION_EVENT } from '../common/PhoneOrientationPolicy';
import { IslandInput, IslandPin, SessionIslandPolicy, ShellKeyboardOwnership, SurfaceTouchOwnership, SurfaceTouchPhase, SurfaceTouchDecision, DialogCallbackScope, DialogCallbackTicket, QueuedRemoteKeys, compactDetailsLines } from '../common/SessionIslandPolicy';
import { SessionDetailsDialog } from '../components/SessionDetailsDialog';

interface ProfileSummary { address: string; username: string; port: number; protocol: string; target: number; modeLabel: string; targetLabel: string; }
interface ProtocolOption { id: string; label: string; shortLabel: string; available: boolean; targets: number[]; }
interface FormErrors { address?: string; port?: string; username?: string; password?: string; protocol?: string; target?: string; profile?: string; }
interface CertificateSummary { fingerprint: string; subject: string; issuer: string; }
interface AppSnapshot {
  initialized: boolean; page: number; available: boolean; canConnect?: boolean;
  target?: number; address?: string; port?: number; username?: string; protocol?: string;
  remember?: boolean; credentialLoaded?: boolean; loading?: string; profiles?: ProfileSummary[];
  selectedIndex?: number; errors?: FormErrors; notice?: string;
  protocolOptions?: ProtocolOption[]; modeNotice?: string;
  session?: string; generation?: string; geometryRevision?: string; certificate?: CertificateSummary;
  persistenceWarning?: string; statusCode?: string; statusMessage?: string;
}
interface FormEdit { target: number; address: string; port: number; username: string; protocol: string; remember: boolean; }
@Entry
@Component
struct Index {
  @StorageLink('frd_window_active') @Watch('shellActivityChanged') private shellActive: boolean = false;
  @StorageLink('frd_phone_fullscreen') private phoneFullscreen: boolean = false;
  @StorageLink('frd_island_safe_right') private islandSafeRight: number = 0;
  @StorageLink('frd_island_safe_bottom') private islandSafeBottom: number = 0;
  @StorageLink('frd_cutout_left') private cutoutLeft: number = 0;
  @StorageLink('frd_cutout_right') private cutoutRight: number = 0;
  @StorageLink('frd_cutout_top') private cutoutTop: number = 0;
  @StorageLink('frd_cutout_bottom') private cutoutBottom: number = 0;
  @State private model: AppSnapshot = { initialized: false, page: 0, available: false };
  @State private target: number = 1;
  @State private address: string = '';
  @State private port: string = '3389';
  @State private username: string = '';
  @State private protocol: string = 'auto';
  @State private remember: boolean = false;
  @State private compact: boolean = false;
  @State private windowWidth: number = 500;
  @State private windowHeight: number = 800;
  @State private passwordEpoch: number = 0;
  @State private bridgeNotice: string = '';
  @State private islandExpanded: boolean = false;
  @State private islandShown: boolean = false;
  @State private tooltipText: string = '';
  @State private iconsReady: boolean = false;
  @State private actionBusy: boolean = false;
  private actionPage: number = 0;
  private island: SessionIslandPolicy = new SessionIslandPolicy();
  private keyboard: ShellKeyboardOwnership = new ShellKeyboardOwnership();
  private detailsController?: CustomDialogController;
  private detailsOpen: boolean = false;
  private detailsScope: DialogCallbackScope = new DialogCallbackScope();
  private surfaceTouch: SurfaceTouchOwnership = new SurfaceTouchOwnership();
  private queuedKeys: QueuedRemoteKeys = new QueuedRemoteKeys();
  private focusTicket: number = 0;
  private focusedAction: number = -1;
  private hoveredAction: number = -1;
  private islandHovered: boolean = false;
  private pointerInput: boolean = false;
  private islandInput: IslandInput = IslandInput.Touch;
  private controller: XComponentController = new XComponentController();
  private timer: number = -1;
  private submitEvent: number = 0;
  private submitBlocked: boolean = false;
  private composing: boolean = false;
  private compositionBatch: boolean = false;
  private enterHeld: boolean = false;
  private enterSubmitted: boolean = false;
  private selectPending: boolean = false;
  private selectedIndex: number = -1;
  private surfaceWidth: number = 0;
  private surfaceHeight: number = 0;
  private zoomScale: number = 1;
  private pinchStart: number = 1;
  private panX: number = 0;
  private panY: number = 0;
  private panStartX: number = 0;
  private panStartY: number = 0;
  private remoteFocused: boolean = false;
  private check(code: number): void {
    if (code !== 0) { this.bridgeNotice = code === -11 ? '操作队列暂满，请稍后重试' : '当前操作不可用，请重试'; }
  }
  private edit(): void {
    const draft: FormEdit = { target: this.target, address: this.address, port: Number(this.port), username: this.username, protocol: this.protocol, remember: this.remember };
    if (!Number.isInteger(draft.port) || draft.port < 0 || draft.port > 65535) { draft.port = 0; }
    this.check(native.editForm(JSON.stringify(draft)));
  }
  private submit(): void {
    if (this.submitBlocked || this.composing || this.compositionBatch || this.model.page !== 0 || !this.model.canConnect) { return; }
    this.submitBlocked = true;
    this.check(native.appAction(1, `${++this.submitEvent}`, 0, 0));
    // 同一事件批次只能产生一个submission；Rust再次检查重复id和busy。
    setTimeout(() => { this.submitBlocked = false; }, 0);
  }
  private error(code?: string): string {
    switch (code) {
      case 'address_required': return '请输入主机地址';
      case 'port_required': return '端口必须为 1–65535';
      case 'username_required': return '请输入用户名';
      case 'password_required': return '请输入密码';
      case 'target_system_required': return '请选择目标系统';
      case 'unregistered_protocol': return '所选连接模式当前不可用';
      case 'unsupported_target_protocol': return '连接模式与目标系统不匹配';
      case 'saved_credential_unavailable': return '保存的密码不可用，请重新输入';
      case 'credential_load_failed': return '安全存储读取失败，请输入密码或稍后重试';
      default: return code ? '此选项当前不可用，请检查输入' : '';
    }
  }
  private geometry(): void {
    if (this.surfaceWidth > 0 && this.surfaceHeight > 0) {
      this.check(native.appGeometry(this.surfaceWidth, this.surfaceHeight, this.surfaceWidth, this.surfaceHeight, this.zoomScale, this.panX, this.panY, this.getUIContext().vp2px(1)));
    }
  }
  private now(): number {
    try { return systemDateTime.getUptime(systemDateTime.TimeType.STARTUP, false); }
    catch { this.bridgeNotice = '系统计时不可用，请使用会话控制'; return 0; }
  }
  private focus(value: boolean): void {
    if (!value) { this.release(); return; }
    if (this.queuedKeys.hasPending()) { this.keyboard.enterLocal(); this.remoteFocused = false; return; }
    this.remoteFocused = this.shellActive && !this.detailsOpen;
    const code = native.appFocus(this.shellActive, this.remoteFocused, this.remoteFocused);
    this.check(code); if (code !== 0) { this.remoteFocused = false; this.keyboard.enterLocal(); }
  }
  private release(): boolean {
    this.remoteFocused = false; this.queuedKeys.isolate();
    const code = native.appFocus(this.shellActive, false, false);
    this.check(code);
    return code === 0;
  }
  private localOwnership(): boolean {
    this.keyboard.enterLocal();
    return this.release();
  }
  private observeSurfaceTouch(event: TouchEvent): void {
    const phase = event.type === TouchType.Down ? SurfaceTouchPhase.Down : event.type === TouchType.Up ? SurfaceTouchPhase.Up :
      event.type === TouchType.Move ? SurfaceTouchPhase.Move : SurfaceTouchPhase.Cancel;
    // Up的touches可能仍包含抬起点；剔除changedTouches后得到剩余有效接触点。
    const released: Set<number> = new Set<number>();
    if (phase === SurfaceTouchPhase.Up) { event.changedTouches.forEach((touch: TouchObject) => { released.add(touch.id); }); }
    const contacts = event.touches.filter((touch: TouchObject): boolean => !released.has(touch.id)).length;
    const decision = this.surfaceTouch.observe(phase, contacts);
    if (decision === SurfaceTouchDecision.Release) { this.localOwnership(); }
    else if (decision === SurfaceTouchDecision.Restore) { this.surfaceInteraction(); }
  }
  private syncQueuedKeyEpoch(): void {
    this.queuedKeys.setEpoch(this.model.page === 0 ? '' : this.model.session ?? '', this.model.generation ?? '', this.model.geometryRevision ?? '');
  }
  private flushPendingKeyUps(): boolean {
    this.syncQueuedKeyEpoch();
    if (!this.queuedKeys.hasPending()) { return true; }
    for (const pending of this.queuedKeys.pending()) {
      // 可信Down原来源的真实尾Up：page0/新会话不借当前身份，native只清匹配隔离。
      const code = native.appInput(pending.session, pending.generation, pending.revision, 2, 0, 0, 0, pending.hid, 0, 127, 127);
      this.queuedKeys.completeCleanup(pending, code === 0);
      if (code !== 0) { this.check(code); this.localOwnership(); return false; }
    }
    return true;
  }
  private surfaceInteraction(): void {
    if (!this.shellActive || this.detailsOpen || this.model.page !== 3 || !this.flushPendingKeyUps()) { return; }
    // 仅同步 shell 所有权；真实 pointer 仍由 native XComponent 唯一发送。
    this.keyboard.enterRemote();
    this.focus(true);
  }
  private syncIsland(): void {
    this.syncQueuedKeyEpoch();
    if (this.model.page !== this.actionPage) { this.actionBusy = false; this.actionPage = this.model.page; }
    const before = this.island.epoch();
    this.island.observe(this.model.page, this.model.session ?? '', this.shellActive, this.now());
    if (before !== this.island.epoch()) {
      this.focusTicket++; this.closeDetails();
      this.tooltipText = ''; this.focusedAction = -1; this.hoveredAction = -1; this.islandHovered = false;
      this.pointerInput = false; this.keyboard.reset(); this.surfaceTouch.reset(); this.remoteFocused = false;
    }
    this.island.pin(IslandPin.Error, this.bridgeNotice.length > 0, this.now());
    this.island.advance(this.now(), this.island.epoch());
    this.refreshIsland();
  }
  private shellActivityChanged(): void {
    if (!this.shellActive) { this.closeDetails(); this.keyboard.reset(); this.surfaceTouch.reset(); this.release(); }
    this.syncIsland();
  }
  private refreshIsland(): void {
    const expanded = this.island.expanded();
    if (expanded !== this.islandExpanded) {
      // 控件分支替换时不保留已销毁入口的 hover/focus；不等待系统补发 leave。
      this.focusedAction = -1; this.hoveredAction = -1; this.islandHovered = false; this.tooltipText = '';
      this.island.pin(IslandPin.Focus, false, this.now()); this.island.pin(IslandPin.Hover, false, this.now());
      this.island.pin(IslandPin.Tooltip, false, this.now());
    }
    this.islandShown = this.island.shown(); this.islandExpanded = expanded;
  }
  private focusIslandAfterLayout(): void {
    const ticket = ++this.focusTicket; const epoch = this.island.epoch();
    setTimeout(() => {
      if (ticket !== this.focusTicket || epoch !== this.island.epoch() || !this.shellActive || !this.keyboard.isLocal() || !this.islandShown) { return; }
      try { this.getUIContext().getFocusController().requestFocus(this.islandExpanded ? 'frdIslandDetails' : 'frdIslandReveal'); }
      catch { this.bridgeNotice = '无法聚焦会话控制，请点击右下角'; }
    }, 0);
  }
  private tooltip(): void {
    const action = this.focusedAction >= 0 ? this.focusedAction : this.hoveredAction;
    this.tooltipText = action >= 0 ? this.actionLabel(action) : '';
    this.island.pin(IslandPin.Tooltip, this.tooltipText.length > 0, this.now());
  }
  private actionLabel(action: number): string {
    if (action === 0) { return '连接状态与服务器身份详情'; }
    if (action === 1) { return this.model.page === 3 ? '断开远程连接' : this.model.page === 5 ? '返回连接表单' : this.model.page === 4 ? '正在断开，不能重复提交' : '取消连接'; }
    return action === 2 ? '收起会话控制' : '展开会话控制';
  }
  private islandPress(input: IslandInput, pressed: boolean): void {
    this.pointerInput = true; this.islandInput = input;
    if (pressed) { this.localOwnership(); }
    this.island.pin(IslandPin.Press, pressed, this.now());
    if (!pressed && this.island.expanded()) { this.island.reveal(input, this.now()); }
    this.refreshIsland();
  }
  private islandFocus(action: number, focused: boolean): void {
    if (focused) {
      this.localOwnership();
      if (!this.pointerInput) { this.focusedAction = action; }
    } else if (this.focusedAction === action) { this.focusedAction = -1; }
    this.island.pin(IslandPin.Focus, this.focusedAction >= 0, this.now());
    this.tooltip(); this.refreshIsland();
  }
  private islandHover(action: number, hovered: boolean): void {
    if (hovered) { this.hoveredAction = action; }
    else if (this.hoveredAction === action) { this.hoveredAction = -1; }
    this.island.pin(IslandPin.Hover, this.islandHovered || this.hoveredAction >= 0, this.now());
    this.tooltip(); this.refreshIsland();
  }
  private revealIsland(input: IslandInput): void {
    if (!this.localOwnership()) { this.island.reveal(input, this.now()); this.refreshIsland(); return; }
    this.island.reveal(input, this.now()); this.refreshIsland();
  }
  private collapseIsland(): void {
    this.focusedAction = -1; this.hoveredAction = -1; this.islandHovered = false; this.tooltipText = '';
    this.island.pin(IslandPin.Focus, false, this.now()); this.island.pin(IslandPin.Hover, false, this.now());
    this.island.pin(IslandPin.Tooltip, false, this.now());
    this.island.collapse(); this.refreshIsland();
  }
  private islandAction(action: number): void {
    if (!this.localOwnership()) { this.island.reveal(this.islandInput, this.now()); this.refreshIsland(); return; }
    if (action === 3) { this.revealIsland(this.islandInput); }
    else if (action === 2) { this.collapseIsland(); }
    else if (action === 0) { this.details(); }
    else if (!this.actionBusy && (this.model.page === 1 || this.model.page === 2 || this.model.page === 3 || this.model.page === 5)) {
      this.actionBusy = true; this.actionPage = this.model.page;
      const code = native.appAction(this.model.page === 5 ? 3 : 2, '0', 0, 0);
      this.check(code); if (code !== 0) { this.actionBusy = false; }
    }
  }
  private closeDetails(): void {
    this.detailsScope.invalidate();
    const controller = this.detailsController; this.detailsController = undefined; this.detailsOpen = false;
    try { controller?.close(); } catch { this.bridgeNotice = '连接详情关闭失败，请重试'; }
    this.island.pin(IslandPin.Details, false, this.now());
  }
  private observeDetailsKey(ticket: DialogCallbackTicket, event: KeyEvent): void {
    if (!this.detailsOpen) { return; }
    const accepted = this.detailsScope.observeKey(ticket, this.island.epoch(), this.shellActive, this.keyboard, event.keyCode, event.type === KeyType.Down);
    if (accepted && event.type === KeyType.Up) {
      this.syncQueuedKeyEpoch();
      if (this.queuedKeys.tailUp(event.keyCode)) { this.flushPendingKeyUps(); }
    }
  }
  private detailsClosed(ticket: DialogCallbackTicket): void {
    if (!this.detailsOpen || !this.detailsScope.accepts(ticket, this.island.epoch(), this.shellActive)) { return; }
    this.detailsScope.invalidate();
    this.detailsOpen = false; this.detailsController = undefined;
    this.island.pin(IslandPin.Details, false, this.now()); this.refreshIsland();
    // 关闭详情仍属于本地域；只有显式返回/真实 surface 交互才恢复远程输入。
  }
  private returnToSurface(): void {
    this.closeDetails(); this.collapseIsland();
    if (!this.shellActive || this.model.page !== 3) { return; }
    try {
      // 先产生真实blur屏障，再按当前control/geometry入队尾Up，最后恢复远程focus。
      this.keyboard.enterLocal();
      this.getUIContext().getFocusController().clearFocus();
      if (!this.flushPendingKeyUps()) { return; }
      this.keyboard.enterRemote();
      this.getUIContext().getFocusController().requestFocus('frdRemoteSurfaceFocus');
    } catch { this.keyboard.enterLocal(); this.bridgeNotice = '无法恢复远程画面焦点，请点击画面'; }
  }
  private pointer(x: number, y: number, buttons: number, wx: number = 0, wy: number = 0): void {
    if (!this.model.session || !this.model.generation || !this.model.geometryRevision) { return; }
    this.check(native.appInput(this.model.session, this.model.generation, this.model.geometryRevision, 1, buttons,
      this.getUIContext().vp2px(x), this.getUIContext().vp2px(y), 0, 0, wx + 127, wy + 127));
  }
  private dispatchKey(event: KeyEvent): boolean {
    const down = event.type === KeyType.Down;
    const repeated = this.keyboard.isHeld(event.keyCode);
    const isolated = this.keyboard.key(event.keyCode, down);
    const hid = usbHid(event.keyCode);
    this.syncQueuedKeyEpoch();
    const cleanupTail = !down && this.queuedKeys.tailUp(event.keyCode);
    if (cleanupTail) { this.flushPendingKeyUps(); }
    if (this.model.page === 0) { return false; }
    this.pointerInput = false;
    const shortcut = down && event.keyCode === 2081 && event.getModifierKeyState?.(['Ctrl']) &&
      event.getModifierKeyState?.(['Alt']) && !event.getModifierKeyState?.(['Shift']) && event.metaKey === 0;
    if (shortcut && repeated) { return true; }
    if (shortcut) {
      this.islandInput = IslandInput.Keyboard; this.revealIsland(IslandInput.Keyboard);
      this.focusIslandAfterLayout();
      return true;
    }
    if (this.keyboard.isLocal()) {
      if (down && event.keyCode === 2070) { this.returnToSurface(); return true; }
      return false;
    }
    if (hid === 227 || hid === 231) { this.release(); return true; }
    if (cleanupTail) { return true; }
    if (down && !this.queuedKeys.allowsDown(event.keyCode)) {
      // 同码旧来源未完成真实Up清理时，新Down不能覆盖其来源账本。只在显式事件重试。
      if (!this.flushPendingKeyUps() || !this.queuedKeys.allowsDown(event.keyCode)) { return true; }
    }
    // 无对应本轮down的孤立keyup（如后台丢失按下/旧会话尾键）不进入远程。
    if ((!down && !repeated) || isolated || !this.keyboard.canSend() || !this.shellActive || !this.remoteFocused || this.detailsOpen ||
      !this.model.session || !this.model.generation || !this.model.geometryRevision || this.model.page !== 3) { return true; }
    if (hid === 0) { return true; }
    if (this.keyboard.systemModifierHeld() || event.metaKey !== 0) { return true; }
    if (event.getModifierKeyState?.(['Ctrl']) && event.getModifierKeyState?.(['Alt']) && event.keyCode === 2071) { return true; }
    const modifiers = (event.getModifierKeyState?.(['Shift']) ? 1 : 0) | (event.getModifierKeyState?.(['Ctrl']) ? 2 : 0) |
      (event.getModifierKeyState?.(['Alt']) ? 4 : 0);
    const code = native.appInput(this.model.session, this.model.generation, this.model.geometryRevision, 2,
      down ? 1 : 0, 0, 0, hid, modifiers, 127, 127);
    if (down) { this.queuedKeys.recordDown(event.keyCode, hid, code === 0); }
    else { this.queuedKeys.normalUp(event.keyCode, code === 0); }
    this.check(code); if (code !== 0) { this.localOwnership(); }
    return true;
  }
  aboutToAppear(): void {
    this.getUIContext().setKeyboardAvoidMode(KeyboardAvoidMode.RESIZE);
    try {
      this.getUIContext().getFont().registerFont({ familyName: 'frd-material-symbols-rounded', familySrc: $rawfile('ui-icons/material-symbols-rounded-24-400.ttf') });
      this.iconsReady = true;
    } catch { this.bridgeNotice = '界面图标加载失败，会话控制使用文字'; }
    this.syncIsland();
    this.edit();
    this.timer = setInterval(() => {
      try {
        this.model = JSON.parse(native.appSnapshot()) as AppSnapshot;
        this.syncIsland();
        if (deviceInfo.deviceType === 'phone' && this.model.initialized) {
          // 只传公开生命周期；方向与窗口生命周期由 Ability owner 去重处理。
          this.getUIContext().getHostContext()?.eventHub.emit(PHONE_SESSION_ORIENTATION_EVENT, this.model.page, this.model.session ?? '');
        }
        if (this.selectPending && this.model.selectedIndex !== undefined && this.model.selectedIndex === this.selectedIndex) {
          this.target = this.model.target ?? 1; this.address = this.model.address ?? ''; this.port = `${this.model.port ?? 3389}`;
          this.username = this.model.username ?? ''; this.protocol = this.model.protocol ?? 'auto'; this.remember = this.model.remember ?? false;
          this.passwordEpoch++; this.selectPending = false;
        }
      } catch { this.bridgeNotice = '应用状态读取失败'; }
    }, 100);
  }
  aboutToDisappear(): void {
    clearInterval(this.timer); this.focusTicket++; this.closeDetails(); this.keyboard.reset(); this.surfaceTouch.reset(); this.release();
    this.island.observe(0, '', false, this.now()); this.refreshIsland();
  }
  onBackPress(): boolean {
    if (this.detailsOpen) { this.closeDetails(); this.refreshIsland(); return true; }
    if (this.islandExpanded && this.model.page === 3) { this.returnToSurface(); return true; }
    if (this.model.page === 1 || this.model.page === 2) { this.islandAction(1); return true; }
    if (this.model.page === 5) { this.islandAction(1); return true; }
    return false;
  }
  @Builder
  passwordInput() {
    TextInput({ placeholder: this.model.credentialLoaded ? '已从安全存储载入；输入可替换' : '请输入密码' })
      .type(InputType.Password).fontSize(16).constraintSize({ minHeight: 48 }).width('100%').enterKeyType(EnterKeyType.Go).accessibilityText('密码')
      .showError(this.error(this.model.errors?.password)).enabled(this.model.page === 0)
      .onChange((value: string, preview?: PreviewText) => {
        const composing = (preview?.value.length ?? 0) > 0;
        if (composing || this.composing) { this.compositionBatch = true; setTimeout(() => { this.compositionBatch = false; }, 0); }
        this.composing = composing;
        if (!composing) { this.check(native.editPassword(value)); }
      })
      .onKeyEvent((event: KeyEvent) => { if (event.keyCode === 2054) { if (event.type === KeyType.Up) { this.enterHeld = false; this.enterSubmitted = false; } else { this.enterHeld = true; } } })
      .onSubmit(() => { if (this.enterHeld && this.enterSubmitted) { return; } if (!this.composing && !this.compositionBatch) { this.enterSubmitted = this.enterHeld; this.submit(); } })
  }
  private profileOptions(): SelectOption[] {
    return (this.model.profiles ?? []).map((profile: ProfileSummary): SelectOption => {
      return { value: `${profile.targetLabel} · ${profile.modeLabel} · ${profile.address}:${profile.port} · ${profile.username}` };
    });
  }
  private showMode(): boolean { return this.target === 2 || this.protocol !== 'auto'; }
  private protocolChoices(): ProtocolOption[] {
    return (this.model.protocolOptions ?? []).filter((option: ProtocolOption): boolean => {
      return option.id === 'auto' || option.id === this.protocol || option.targets.includes(this.target);
    });
  }
  private protocolOptions(): SelectOption[] {
    return this.protocolChoices().map((option: ProtocolOption): SelectOption => {
      return { value: option.available ? option.label : `${option.label}（不可用）` };
    });
  }
  private protocolIndex(): number {
    return this.protocolChoices().findIndex((option: ProtocolOption): boolean => option.id === this.protocol);
  }
  private protocolValue(): string {
    return this.protocolChoices().find((option: ProtocolOption): boolean => option.id === this.protocol)?.shortLabel || '已保存';
  }
  private changeTarget(target: number): void {
    if (target === this.target) { return; }
    const selected = (this.model.protocolOptions ?? []).find((option: ProtocolOption): boolean => option.id === this.protocol);
    if (this.protocol !== 'auto' && (!selected || !selected.targets.includes(target))) { this.protocol = 'auto'; }
    this.target = target; this.port = target === 2 ? '5900' : '3389';
    this.edit(); this.passwordEpoch++;
  }
  private changeProtocol(index: number): void {
    const selected = this.protocolChoices()[index];
    if (!selected || selected.id === this.protocol) { return; }
    this.protocol = selected.id; this.edit(); this.passwordEpoch++;
  }
  @Builder
  fieldLabel(label: string) {
    Text(label).fontSize(14).fontColor($r('app.color.text_secondary')).width('100%').textAlign(TextAlign.Start)
  }
  private widePhoneForm(): boolean {
    return deviceInfo.deviceType === 'phone' && this.windowWidth >= 600 && this.windowHeight < 500;
  }
  private formFieldWidth(): string { return this.widePhoneForm() ? 'calc(50% - 6vp)' : '100%'; }
  @Builder
  rememberAction() {
    // 整行只由一个原生按钮接收点击；复选框仅绘制状态，避免父子重复切换。
    Button({ type: ButtonType.Normal }) {
      Row({ space: 12 }) {
        Checkbox().select(this.remember).width(24).height(24).hitTestBehavior(HitTestMode.None).focusable(false).accessibilityLevel('no')
        Text('在此设备安全保存登录信息').fontSize(14).fontColor($r('app.color.text_primary')).layoutWeight(1).textAlign(TextAlign.Start)
      }.width('100%')
    }.buttonStyle(ButtonStyleMode.TEXTUAL).padding({ left: 0, right: 0, top: 8, bottom: 8 }).width('100%').constraintSize({ minHeight: 48 })
      .focusable(true).accessibilityText('在此设备安全保存登录信息').accessibilityRole(AccessibilityRoleType.CHECKBOX).accessibilityChecked(this.remember)
      .onClick(() => { this.remember = !this.remember; this.edit(); }).margin({ top: 8 })
  }
  @Builder
  formMessages() {
    if (this.model.modeNotice) { Text(this.model.modeNotice).fontSize(14).fontColor($r('app.color.text_secondary')).width('100%').margin({ top: 4 }) }
    if (this.model.errors?.protocol) { Text(this.error(this.model.errors.protocol)).fontSize(14).width('100%').margin({ top: 4 }) }
    if (this.model.notice || this.bridgeNotice) { Text(this.bridgeNotice || this.model.notice || '').fontSize(14).width('100%').fontColor($r('app.color.text_secondary')).margin({ top: 4 }) }
  }
  @Builder
  connectAction() {
    Button('连接').fontSize(16).constraintSize({ minHeight: 48 }).width('100%').enabled(this.model.canConnect ?? false).focusable(true)
      .accessibilityText('连接远程桌面').onClick(() => this.submit()).margin({ top: 8 })
  }
  @Builder
  connectionForm() {
    Scroll() {
      Column() {
        Flex({ direction: this.widePhoneForm() ? FlexDirection.Row : FlexDirection.Column, justifyContent: FlexAlign.SpaceBetween, alignItems: ItemAlign.Start }) {
          Column({ space: 4 }) {
            Text('连接远程桌面').fontSize(this.compact ? 24 : 28).fontWeight(FontWeight.Bold)
              .fontColor($r('app.color.text_primary')).width('100%').textAlign(TextAlign.Start)
            Text('FreeRemoteDesk').fontSize(12).fontColor($r('app.color.text_secondary')).width('100%').textAlign(TextAlign.Start)
          }.width(this.widePhoneForm() && (this.model.profiles?.length ?? 0) > 0 ? this.formFieldWidth() : '100%').constraintSize({ minHeight: 48 }).alignItems(HorizontalAlign.Start).margin({ bottom: this.compact ? 16 : 24 })
          if ((this.model.profiles?.length ?? 0) > 0) {
            Column() {
              Select(this.profileOptions()).selected(this.model.selectedIndex ?? -1).value('最近连接')
                .font({ size: 14 }).optionFont({ size: 16 }).optionWidth(OptionWidthMode.FIT_TRIGGER)
                .width('100%').constraintSize({ minHeight: 48 }).accessibilityText('选择最近连接')
                .onSelect((index: number) => { this.selectedIndex = index; this.selectPending = true; this.check(native.appAction(4, '0', 0, index)); })
                .margin({ bottom: 12 })
              if (this.model.errors?.profile) { Text(this.error(this.model.errors.profile)).fontSize(14).width('100%').margin({ bottom: 8 }) }
            }.width(this.formFieldWidth())
          }
        }.width('100%')
        // 同一组输入控件仅调整宽度和换行，旋转时不重建密码控件。
        Flex({ wrap: FlexWrap.Wrap, justifyContent: FlexAlign.SpaceBetween, alignItems: ItemAlign.Start, alignContent: FlexAlign.Start }) {
          Row({ space: 12 }) {
            Column({ space: 6 }) {
              this.fieldLabel('目标系统')
              Select([{ value: 'Windows' }, { value: 'macOS' }, { value: 'Linux' }]).selected(this.target - 1).value(this.target === 1 ? 'Windows' : this.target === 2 ? 'macOS' : 'Linux')
                .font({ size: 16 }).optionFont({ size: 16 }).width('100%').constraintSize({ minHeight: 48 }).accessibilityText('目标系统')
                .padding({ left: 8, right: 8 }).onSelect((index: number) => this.changeTarget(index + 1))
              if (this.model.errors?.target) { Text(this.error(this.model.errors.target)).fontSize(14).width('100%') }
            }.layoutWeight(this.showMode() ? 1.2 : 1).alignItems(HorizontalAlign.Start)
            if (this.showMode()) {
              Column({ space: 6 }) {
                this.fieldLabel('连接模式')
                Select(this.protocolOptions()).selected(this.protocolIndex()).value(this.protocolValue())
                  .font({ size: 16 }).optionFont({ size: 16 }).optionWidth(240)
                  .width('100%').constraintSize({ minHeight: 48 }).padding({ left: 8, right: 8 })
                  .accessibilityText(`连接模式：${this.protocolValue()}${this.model.available ? '' : '，当前不可用'}`)
                  .onSelect((index: number) => this.changeProtocol(index))
              }.layoutWeight(1).alignItems(HorizontalAlign.Start)
            }
            Column({ space: 6 }) {
              this.fieldLabel('端口')
              TextInput({ text: this.port, placeholder: '端口' }).type(InputType.Number).fontSize(16).width('100%')
                .constraintSize({ minHeight: 48 }).accessibilityText('端口').showError(this.error(this.model.errors?.port))
                .onChange((v: string) => { this.port = v; this.edit(); })
            }.width(deviceInfo.deviceType === 'phone' || !this.showMode() ? 96 : 72).alignItems(HorizontalAlign.Start)
          }.width(this.formFieldWidth()).alignItems(VerticalAlign.Top).margin({ bottom: 12 })
          Column({ space: 6 }) {
            this.fieldLabel('主机地址')
            TextInput({ text: this.address, placeholder: '主机名或 IP 地址' }).type(InputType.URL).fontSize(16).width('100%').constraintSize({ minHeight: 48 }).accessibilityText('主机地址')
              .showError(this.error(this.model.errors?.address)).onChange((v: string) => { this.address = v; this.edit(); })
          }.width(this.formFieldWidth()).margin({ bottom: 12 })
          Column({ space: 6 }) {
            this.fieldLabel('用户名')
            TextInput({ text: this.username, placeholder: '远程计算机账户' }).fontSize(16).width('100%').constraintSize({ minHeight: 48 }).accessibilityText('用户名')
              .showError(this.error(this.model.errors?.username)).onChange((v: string) => { this.username = v; this.edit(); })
          }.width(this.formFieldWidth()).margin({ bottom: this.widePhoneForm() ? 0 : 12 })
          Column({ space: 6 }) {
            this.fieldLabel('密码')
            ForEach([this.passwordEpoch], (_epoch: number) => { this.passwordInput() }, (epoch: number) => `${epoch}`)
          }.width(this.formFieldWidth())
        }.width('100%')
        if (this.model.loading) { Text(this.model.loading).fontSize(14).width('100%').fontColor($r('app.color.text_secondary')).margin({ top: 8 }) }
        if (this.model.credentialLoaded) { Text('已从系统安全存储载入密码').fontSize(14).width('100%').fontColor($r('app.color.text_secondary')).margin({ top: 8 }) }
        if (this.widePhoneForm()) {
          Row({ space: 12 }) {
            Column() { this.rememberAction() }.layoutWeight(1)
            Column() { this.connectAction() }.width('32%')
          }.width('100%').alignItems(VerticalAlign.Top)
          this.formMessages()
        } else {
          this.rememberAction()
          this.formMessages()
          this.connectAction()
        }
      }.width('100%').constraintSize({ maxWidth: this.widePhoneForm() ? 880 : 440 }).padding({ top: this.compact ? 16 : 24, bottom: this.compact ? 16 : 24 }).alignItems(HorizontalAlign.Center)
    }.width('100%').layoutWeight(1).clip(true).scrollBar(BarState.Auto).padding({ left: this.compact ? 16 : 24, right: this.compact ? 16 : 24 })
  }
  @Builder
  remoteSurface() {
    XComponent({ id: 'frdRemoteSurface', type: XComponentType.SURFACE, libraryname: 'entry', controller: this.controller })
      .width('100%').height('100%').id('frdRemoteSurfaceFocus').focusable(true).accessibilityText('远程桌面')
      // 下发拦截只属于远程画面；注册到表单祖先会阻断原生TextInput，即使回调返回false。
      .onKeyEventDispatch((event: KeyEvent): boolean => this.dispatchKey(event))
      .onAreaChange((_, area) => { this.release(); this.surfaceWidth = Math.round(this.getUIContext().vp2px(Number(area.width))); this.surfaceHeight = Math.round(this.getUIContext().vp2px(Number(area.height))); this.geometry(); })
      .onFocus(() => { if (!this.keyboard.isLocal()) { this.focus(true); } }).onBlur(() => this.focus(false))
      // 仅观察所有权，不生产第二份触摸/鼠标意图；native down/press 仍负责唯一远程 pointer。
      .onTouch((event: TouchEvent) => this.observeSurfaceTouch(event))
      .onMouse((event: MouseEvent) => { if (event.action === MouseAction.Press) { this.surfaceInteraction(); } })
      // SURFACE 的触摸/鼠标由 native XComponent 回调唯一生产，避免双送。
      .onAxisEvent((event: AxisEvent) => {
        const step = Math.max(1, event.scrollStep ?? 1);
        const wx = Math.max(-127, Math.min(127, Math.round(event.getHorizontalAxisValue() / step)));
        const wy = Math.max(-127, Math.min(127, Math.round(event.getVerticalAxisValue() / step)));
        this.pointer(event.x, event.y, 0, wx, wy);
      })
      .gesture(GestureGroup(GestureMode.Parallel,
        PinchGesture({ fingers: 2 }).onActionStart(() => { this.release(); this.pinchStart = this.zoomScale; })
          .onActionUpdate((event: GestureEvent) => { this.zoomScale = Math.max(1, Math.min(8, this.pinchStart * event.scale)); this.geometry(); }),
        PanGesture({ fingers: 2 }).onActionStart(() => { this.release(); this.panStartX = this.panX; this.panStartY = this.panY; })
          .onActionUpdate((event: GestureEvent) => { this.panX = this.panStartX + this.getUIContext().vp2px(event.offsetX); this.panY = this.panStartY + this.getUIContext().vp2px(event.offsetY); this.geometry(); })
      ))
  }
  private status(): string {
    return this.model.statusMessage || (this.model.page === 1 ? '正在连接' : this.model.page === 2 ? '等待首个完整画面' : this.model.page === 3 ? '已连接' : this.model.page === 4 ? '正在断开' : '连接失败');
  }
  private details(): void {
    if (this.detailsOpen) { return; }
    const certificate = this.model.certificate;
    const lines: string[] = [this.status(), this.model.modeNotice || '', this.model.persistenceWarning || '', this.model.notice || '', this.bridgeNotice];
    if (certificate && certificate.fingerprint.trim().length > 0) { lines.push(`服务器指纹：${certificate.fingerprint}`); }
    if (certificate && certificate.subject.trim().length > 0) { lines.push(`主题：${certificate.subject}`); }
    if (certificate && certificate.issuer.trim().length > 0) { lines.push(`签发者：${certificate.issuer}`); }
    const message = compactDetailsLines(lines);
    const ticket = this.detailsScope.begin(this.island.epoch());
    this.detailsController = new CustomDialogController({
      builder: SessionDetailsDialog({ message: message, closed: () => this.detailsClosed(ticket), observeKey: (event: KeyEvent) => this.observeDetailsKey(ticket, event) }),
      cancel: () => this.detailsClosed(ticket), autoCancel: true,
      openAnimation: { duration: 0 }, closeAnimation: { duration: 0 }
    });
    this.detailsOpen = true; this.island.pin(IslandPin.Details, true, this.now());
    try { this.detailsController.open(); }
    catch { this.detailsScope.invalidate(); this.detailsOpen = false; this.detailsController = undefined; this.island.pin(IslandPin.Details, false, this.now()); this.bridgeNotice = '连接详情暂时无法打开，请重试'; }
    this.refreshIsland();
  }
  private statusGlyph(): string {
    return this.model.page === 1 ? '\ue9d0' : this.model.page === 2 ? '\uea5b' : this.model.page === 3 ? '\uf0be' : this.model.page === 4 ? '\uef64' : '\uf8b6';
  }
  @Builder
  islandButton(glyph: string, action: number, enabled: boolean) {
    Button({ type: ButtonType.Normal }) {
      if (this.iconsReady) {
        Text(glyph).fontFamily('frd-material-symbols-rounded').fontSize(24).fontColor($r('app.color.text_primary')).accessibilityLevel('no')
      } else {
        Text(action === 0 ? '详情' : action === 1 ? this.model.page === 3 ? '断开' : this.model.page === 5 ? '返回' : '取消' : action === 2 ? '收起' : '控制').fontSize(14).fontColor($r('app.color.text_primary')).accessibilityLevel('no')
      }
    }.width(48).height(48).borderRadius(12).buttonStyle(ButtonStyleMode.TEXTUAL)
      .id(action === 0 ? 'frdIslandDetails' : action === 3 ? 'frdIslandReveal' : `frdIslandAction${action}`)
      .enabled(enabled).focusable(true).focusOnTouch(false).accessibilityText(this.actionLabel(action))
      .onFocus(() => this.islandFocus(action, true)).onBlur(() => this.islandFocus(action, false))
      .onKeyEvent((event: KeyEvent) => { if (event.type === KeyType.Down) { this.pointerInput = false; this.islandFocus(action, true); } })
      .onHover((hovered: boolean) => this.islandHover(action, hovered))
      .onClick(() => this.islandAction(action))
  }
  @Builder
  sessionIsland() {
    Column({ space: 6 }) {
      if (this.tooltipText.length > 0) {
        Text(this.tooltipText).fontSize(14).fontColor($r('app.color.text_primary')).padding(8).backgroundColor($r('app.color.island_background')).borderRadius(8)
          .constraintSize({ maxWidth: 260 }).hitTestBehavior(HitTestMode.None).accessibilityLevel('no')
      }
      Column({ space: 4 }) {
        if (this.islandExpanded) {
          if (this.model.page !== 3 || this.bridgeNotice.length > 0) {
            Text(this.bridgeNotice || this.status()).fontSize(14).fontColor($r('app.color.text_primary')).padding({ left: 8, right: 8, top: 4 })
              .constraintSize({ maxWidth: 240 }).accessibilityText(this.bridgeNotice || this.status())
          }
          Flex({ wrap: FlexWrap.Wrap, justifyContent: FlexAlign.End, space: { main: LengthMetrics.vp(4), cross: LengthMetrics.vp(4) } }) {
            this.islandButton(this.statusGlyph(), 0, true)
            this.islandButton(this.model.page === 3 ? '\ue16f' : '\ue5cd', 1, this.model.page !== 4 && !this.actionBusy)
            if (this.model.page === 3) { this.islandButton('\ue5cf', 2, true) }
          }.constraintSize({ maxWidth: Math.max(48, Math.min(152, this.windowWidth - 40)) })
        } else {
          this.islandButton('\ue5d3', 3, true)
        }
      }.padding(4).backgroundColor($r('app.color.island_background')).borderRadius(20)
        .border({ width: 1, color: $r('app.color.island_border') }).hitTestBehavior(HitTestMode.Default)
        .onHover((hovered: boolean) => { this.islandHovered = hovered; this.island.pin(IslandPin.Hover, hovered || this.hoveredAction >= 0, this.now()); this.refreshIsland(); })
        .onTouch((event: TouchEvent) => {
          if (event.source !== SourceType.TouchScreen) { return; }
          if (event.type === TouchType.Down) { this.islandPress(IslandInput.Touch, true); }
          if (event.type === TouchType.Up || event.type === TouchType.Cancel) { this.islandPress(IslandInput.Touch, false); }
        })
        .onMouse((event: MouseEvent) => {
          if (event.action === MouseAction.Press) { this.islandPress(IslandInput.Mouse, true); }
          if (event.action === MouseAction.Release) { this.islandPress(IslandInput.Mouse, false); }
        })

    }.alignItems(HorizontalAlign.End).constraintSize({ maxWidth: Math.max(56, this.windowWidth - 24) })
      .margin({ right: 10 + this.getUIContext().px2vp(this.islandSafeRight), bottom: 10 + this.getUIContext().px2vp(this.islandSafeBottom) }).hitTestBehavior(HitTestMode.None)
  }
  build() {
    Stack({ alignContent: Alignment.BottomEnd }) {
      if (this.model.page === 0) {
        Column() { this.connectionForm() }.width('100%').height('100%')
      } else {
        this.remoteSurface()
        if (this.islandShown) { this.sessionIsland() }
      }
    }.width('100%').height('100%').clip(true)
      .padding({ left: this.phoneFullscreen && this.model.page !== 0 ? 0 : this.getUIContext().px2vp(this.cutoutLeft), right: this.phoneFullscreen && this.model.page !== 0 ? 0 : this.getUIContext().px2vp(this.cutoutRight), top: this.phoneFullscreen && this.model.page !== 0 ? 0 : this.getUIContext().px2vp(this.cutoutTop), bottom: this.phoneFullscreen && this.model.page !== 0 ? 0 : this.getUIContext().px2vp(this.cutoutBottom) })
      .backgroundColor(this.model.page === 0 ? $r('app.color.page_background') : Color.Black)
      // 普通冒泡保留岛操作和原来源尾Up清理，输入框先处理数字、IME和Enter。
      .onKeyEvent((event: KeyEvent) => { if (this.dispatchKey(event)) { event.stopPropagation(); } })
      .onAreaChange((_, area) => { this.compact = Number(area.width) < 600 || Number(area.height) < 500; this.windowWidth = Number(area.width); this.windowHeight = Number(area.height); })
  }
}
