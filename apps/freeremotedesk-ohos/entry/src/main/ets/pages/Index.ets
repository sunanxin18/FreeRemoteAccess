import native from 'libentry.so';
import { KeyboardAvoidMode } from '@kit.ArkUI';
import { usbHid } from './InputMapping';

interface ProfileSummary { address: string; username: string; port: number; }
interface FormErrors { address?: string; port?: string; username?: string; password?: string; protocol?: string; target?: string; profile?: string; }
interface CertificateSummary { fingerprint: string; subject: string; issuer: string; }
interface AppSnapshot {
  initialized: boolean; page: number; available: boolean; canConnect?: boolean;
  target?: number; address?: string; port?: number; username?: string; protocol?: string;
  remember?: boolean; credentialLoaded?: boolean; loading?: string; profiles?: ProfileSummary[];
  selectedIndex?: number; errors?: FormErrors; notice?: string;
  session?: string; generation?: string; geometryRevision?: string; certificate?: CertificateSummary;
  persistenceWarning?: string; statusCode?: string; statusMessage?: string;
}
interface FormEdit { target: number; address: string; port: number; username: string; protocol: string; remember: boolean; }
@Entry
@Component
struct Index {
  @StorageLink('frd_native_buttons_width') private nativeButtonsWidth: number = 0;
  @StorageLink('frd_native_chrome') private nativeChrome: boolean = false;
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
  @State private passwordEpoch: number = 0;
  @State private bridgeNotice: string = '';
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
  private systemModifierHeld: boolean = false;
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
  private focus(value: boolean): void { this.remoteFocused = value; this.check(native.appFocus(true, value, value)); }
  private release(): void { native.appFocus(true, false, false); }
  private pointer(x: number, y: number, buttons: number, wx: number = 0, wy: number = 0): void {
    if (!this.model.session || !this.model.generation || !this.model.geometryRevision) { return; }
    this.check(native.appInput(this.model.session, this.model.generation, this.model.geometryRevision, 1, buttons,
      this.getUIContext().vp2px(x), this.getUIContext().vp2px(y), 0, 0, wx + 127, wy + 127));
  }
  private remoteKey(event: KeyEvent): void {
    if (!this.model.session || !this.model.generation || !this.model.geometryRevision) { return; }
    const hid = usbHid(event.keyCode); if (hid === 0) { return; }
    if (hid === 227 || hid === 231) { this.systemModifierHeld = event.type === KeyType.Down; this.release(); return; }
    if (this.systemModifierHeld || event.metaKey !== 0) { return; }
    if (event.getModifierKeyState?.(['Ctrl']) && event.getModifierKeyState?.(['Alt']) && event.keyCode === 2071) { return; }
    const modifiers = (event.getModifierKeyState?.(['Shift']) ? 1 : 0) | (event.getModifierKeyState?.(['Ctrl']) ? 2 : 0) |
      (event.getModifierKeyState?.(['Alt']) ? 4 : 0);
    this.check(native.appInput(this.model.session, this.model.generation, this.model.geometryRevision, 2,
      event.type === KeyType.Down ? 1 : 0, 0, 0, hid, modifiers, 127, 127));
  }
  aboutToAppear(): void {
    this.getUIContext().setKeyboardAvoidMode(KeyboardAvoidMode.RESIZE);
    this.edit();
    this.timer = setInterval(() => {
      try {
        this.model = JSON.parse(native.appSnapshot()) as AppSnapshot;
        if (this.selectPending && this.model.selectedIndex !== undefined && this.model.selectedIndex === this.selectedIndex) {
          this.target = this.model.target ?? 1; this.address = this.model.address ?? ''; this.port = `${this.model.port ?? 3389}`;
          this.username = this.model.username ?? ''; this.protocol = this.model.protocol ?? 'auto'; this.remember = this.model.remember ?? false;
          this.passwordEpoch++; this.selectPending = false;
        }
      } catch { this.bridgeNotice = '应用状态读取失败'; }
    }, 100);
  }
  aboutToDisappear(): void { clearInterval(this.timer); this.release(); }
  onBackPress(): boolean { if (this.model.page !== 0) { native.appAction(2, '0', 0, 0); return true; } return false; }
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
      return { value: `${profile.address}:${profile.port} · ${profile.username}` };
    });
  }
  @Builder
  fieldLabel(label: string) {
    Text(label).fontSize(14).fontColor($r('app.color.text_secondary')).width('100%').textAlign(TextAlign.Start)
  }
  @Builder
  connectionForm() {
    Scroll() {
      Column() {
        Column({ space: 4 }) {
          Text('连接远程桌面').fontSize(this.compact ? 24 : 28).fontWeight(FontWeight.Bold)
            .fontColor($r('app.color.text_primary')).width('100%').textAlign(TextAlign.Start)
          Text('FreeRemoteDesk').fontSize(12).fontColor($r('app.color.text_secondary')).width('100%').textAlign(TextAlign.Start)
        }.width('100%').constraintSize({ minHeight: 48 }).alignItems(HorizontalAlign.Start).margin({ bottom: this.compact ? 16 : 24 })
        if ((this.model.profiles?.length ?? 0) > 0) {
          Select(this.profileOptions()).selected(this.model.selectedIndex ?? -1).value('最近连接')
            .font({ size: 14 }).optionFont({ size: 16 }).optionWidth(OptionWidthMode.FIT_TRIGGER)
            .width('100%').constraintSize({ minHeight: 48 }).accessibilityText('选择最近连接')
            .onSelect((index: number) => { this.selectedIndex = index; this.selectPending = true; this.check(native.appAction(4, '0', 0, index)); })
            .margin({ bottom: 12 })
          if (this.model.errors?.profile) { Text(this.error(this.model.errors.profile)).fontSize(14).width('100%').margin({ bottom: 8 }) }
        }
        Column({ space: 12 }) {
          Row({ space: 12 }) {
            Column({ space: 6 }) {
              this.fieldLabel('目标系统')
              Select([{ value: 'Windows' }, { value: 'macOS' }, { value: 'Linux' }]).selected(this.target - 1).value(this.target === 1 ? 'Windows' : this.target === 2 ? 'macOS' : 'Linux')
                .font({ size: 16 }).optionFont({ size: 16 }).width('100%').constraintSize({ minHeight: 48 }).accessibilityText('目标系统')
                .onSelect((index: number) => { this.target = index + 1; this.port = index === 1 ? '5900' : '3389'; this.edit(); })
              if (this.model.errors?.target) { Text(this.error(this.model.errors.target)).fontSize(14).width('100%') }
            }.layoutWeight(1).alignItems(HorizontalAlign.Start)
            Column({ space: 6 }) {
              this.fieldLabel('端口')
              TextInput({ text: this.port, placeholder: '端口' }).type(InputType.Number).fontSize(16).width('100%')
                .constraintSize({ minHeight: 48 }).accessibilityText('端口').showError(this.error(this.model.errors?.port))
                .onChange((v: string) => { this.port = v; this.edit(); })
            }.width(96).alignItems(HorizontalAlign.Start)
          }.width('100%').alignItems(VerticalAlign.Top)
          Column({ space: 6 }) {
            this.fieldLabel('主机地址')
            TextInput({ text: this.address, placeholder: '主机名或 IP 地址' }).fontSize(16).width('100%').constraintSize({ minHeight: 48 }).accessibilityText('主机地址')
              .showError(this.error(this.model.errors?.address)).onChange((v: string) => { this.address = v; this.edit(); })
          }.width('100%')
          Column({ space: 6 }) {
            this.fieldLabel('用户名')
            TextInput({ text: this.username, placeholder: '远程计算机账户' }).fontSize(16).width('100%').constraintSize({ minHeight: 48 }).accessibilityText('用户名')
              .showError(this.error(this.model.errors?.username)).onChange((v: string) => { this.username = v; this.edit(); })
          }.width('100%')
          Column({ space: 6 }) {
            this.fieldLabel('密码')
            ForEach([this.passwordEpoch], (_epoch: number) => { this.passwordInput() }, (epoch: number) => `${epoch}`)
          }.width('100%')
        }.width('100%')
        if (this.model.loading) { Text(this.model.loading).fontSize(14).width('100%').fontColor($r('app.color.text_secondary')).margin({ top: 8 }) }
        if (this.model.credentialLoaded) { Text('已从系统安全存储载入密码').fontSize(14).width('100%').fontColor($r('app.color.text_secondary')).margin({ top: 8 }) }
        // 整行只由一个原生按钮接收点击；复选框仅绘制状态，避免父子重复切换。
        Button({ type: ButtonType.Normal }) {
          Row({ space: 12 }) {
            Checkbox().select(this.remember).width(24).height(24).hitTestBehavior(HitTestMode.None).focusable(false).accessibilityLevel('no')
            Text('在此设备安全保存登录信息').fontSize(14).fontColor($r('app.color.text_primary')).layoutWeight(1).textAlign(TextAlign.Start)
          }.width('100%')
        }.buttonStyle(ButtonStyleMode.TEXTUAL).padding({ left: 0, right: 0, top: 8, bottom: 8 }).width('100%').constraintSize({ minHeight: 48 })
          .focusable(true).accessibilityText('在此设备安全保存登录信息').accessibilityRole(AccessibilityRoleType.CHECKBOX).accessibilityChecked(this.remember)
          .onClick(() => { this.remember = !this.remember; this.edit(); }).margin({ top: 8 })
        if (!this.model.available) { Text('此设备尚无可用的远程连接协议').fontSize(14).fontColor($r('app.color.text_secondary')).width('100%').margin({ top: 4 }) }
        if (this.model.errors?.protocol) { Text(this.error(this.model.errors.protocol)).fontSize(14).width('100%').margin({ top: 4 }) }
        if (this.model.notice || this.bridgeNotice) { Text(this.bridgeNotice || this.model.notice || '').fontSize(14).width('100%').fontColor($r('app.color.text_secondary')).margin({ top: 4 }) }
        Button('连接').fontSize(16).constraintSize({ minHeight: 48 }).width('100%').enabled(this.model.canConnect ?? false).focusable(true)
          .accessibilityText('连接远程桌面').onClick(() => this.submit()).margin({ top: 8 })
      }.width('100%').constraintSize({ maxWidth: 440 }).padding({ top: this.compact ? 16 : 24, bottom: this.compact ? 16 : 24 }).alignItems(HorizontalAlign.Center)
    }.width('100%').layoutWeight(1).clip(true).scrollBar(BarState.Auto).padding({ left: this.compact ? 16 : 24, right: this.compact ? 16 : 24 })
  }
  @Builder
  remoteSurface() {
    XComponent({ id: 'frdRemoteSurface', type: XComponentType.SURFACE, libraryname: 'entry', controller: this.controller })
      .width('100%').layoutWeight(1).focusable(true).accessibilityText('远程桌面')
      .onAreaChange((_, area) => { this.release(); this.surfaceWidth = Math.round(this.getUIContext().vp2px(Number(area.width))); this.surfaceHeight = Math.round(this.getUIContext().vp2px(Number(area.height))); this.geometry(); })
      .onFocus(() => this.focus(true)).onBlur(() => this.focus(false))
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
  private details(): void {
    this.release();
    const certificate = this.model.certificate;
    const identity = certificate ? `服务器指纹：${certificate.fingerprint}\n主题：${certificate.subject}\n签发者：${certificate.issuer}` : '';
    const status = this.model.statusMessage || this.model.persistenceWarning || this.model.notice || (this.model.page === 1 ? '正在连接' : this.model.page === 2 ? '等待首个完整画面' : this.model.page === 3 ? '已连接' : this.model.page === 4 ? '正在断开' : '连接失败');
    this.getUIContext().showAlertDialog({ title: '连接状态', message: `${status}\n${this.model.persistenceWarning || this.model.notice || ''}\n${identity}`, confirm: { value: '关闭', action: () => {} } });
  }
  @Builder
  sessionChrome() {
    Stack({ alignContent: Alignment.Center }) {
      // 空白区域仍属平台标题区，不吞掉按钮或系统三键事件。
      Row().width('100%').height(56)
      Row({ space: 8 }) {
        if (this.windowWidth > this.nativeButtonsWidth * 2 + 360) {
          Text(this.model.page === 0 ? 'FreeRemoteDesk' : this.model.page === 1 ? '正在连接' : this.model.page === 2 ? '等待画面' : this.model.page === 3 ? '已连接' : this.model.page === 4 ? '正在断开' : (this.model.statusMessage || '连接失败'))
            .fontSize(14).fontColor($r('app.color.text_primary')).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis }).accessibilityText(this.model.statusMessage || '连接状态')
        }
        if (this.model.page !== 0) {
          if (this.windowWidth > this.nativeButtonsWidth * 2 + 160) {
          Button('详情', { type: ButtonType.Normal }).constraintSize({ minHeight: 48, minWidth: 48 }).focusable(true)
            .accessibilityText('连接状态与服务器身份详情').onFocus(() => this.release()).onClick(() => this.details())
          }
          if (this.model.page === 1 || this.model.page === 2 || this.model.page === 3) {
            Button(this.model.page === 3 ? '断开' : '取消', { type: ButtonType.Normal }).constraintSize({ minHeight: 48, minWidth: 48 }).focusable(true)
              .accessibilityText(this.model.page === 3 ? '断开远程连接' : '取消连接').onFocus(() => this.release()).onClick(() => { this.release(); this.check(native.appAction(2, '0', 0, 0)); })
          } else if (this.model.page === 5) {
            Button('返回', { type: ButtonType.Normal }).constraintSize({ minHeight: 48, minWidth: 48 }).focusable(true)
              .accessibilityText('返回连接表单').onFocus(() => this.release()).onClick(() => { this.check(native.appAction(3, '0', 0, 0)); })
          }
        }
      }.constraintSize({ maxWidth: Math.max(0, this.windowWidth - this.nativeButtonsWidth * 2 - 16) })
    }.width('100%').height(56).backgroundColor($r('app.color.page_background'))
  }
  build() {
    Column() {
      if (this.nativeChrome || this.model.page !== 0) { this.sessionChrome() }
      if (this.model.page === 0) {
        this.connectionForm()
      } else {
        // session controls/status 由平台 window chrome 接管，不占用 remote-content toolbar。
        this.remoteSurface()
      }
    }.width('100%').height('100%').clip(true)
      .padding({ left: this.getUIContext().px2vp(this.cutoutLeft), right: this.getUIContext().px2vp(this.cutoutRight), top: this.getUIContext().px2vp(this.cutoutTop), bottom: this.getUIContext().px2vp(this.cutoutBottom) })
      .backgroundColor($r('app.color.page_background'))
      .onKeyEvent((event: KeyEvent) => this.remoteKey(event))
      .onAreaChange((_, area) => { this.compact = Number(area.width) < 600 || Number(area.height) < 500; this.windowWidth = Number(area.width); })
  }
}
