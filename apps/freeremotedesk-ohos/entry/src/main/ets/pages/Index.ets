import native from 'libentry.so';
import { usbHid } from './InputMapping';

interface ProfileSummary { address: string; username: string; port: number; }
interface FormErrors { address?: string; port?: string; username?: string; password?: string; protocol?: string; target?: string; profile?: string; }
interface CertificateSummary { fingerprint: string; subject: string; issuer: string; }
interface AppSnapshot {
  initialized: boolean; page: number; available: boolean; canConnect?: boolean;
  target?: number; address?: string; port?: number; username?: string; protocol?: string;
  remember?: boolean; credentialLoaded?: boolean; loading?: string; profiles?: ProfileSummary[];
  selectedIndex?: number; errors?: FormErrors; notice?: string; probe?: number;
  session?: string; generation?: string; geometryRevision?: string; certificate?: CertificateSummary;
  persistenceWarning?: string;
}
interface FormEdit { target: number; address: string; port: number; username: string; protocol: string; remember: boolean; }
@Entry
@Component
struct Index {
  @StorageLink('frd_graphics_poc') private showingPoc: boolean = false;
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
  @State private graphicsStatus: string = '';
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
      this.check(native.appGeometry(this.surfaceWidth, this.surfaceHeight, this.surfaceWidth, this.surfaceHeight, this.zoomScale, this.panX, this.panY));
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
    this.edit();
    this.timer = setInterval(() => {
      try {
        this.model = JSON.parse(native.appSnapshot()) as AppSnapshot;
        if (this.selectPending && this.model.selectedIndex !== undefined && this.model.selectedIndex === this.selectedIndex) {
          this.target = this.model.target ?? 1; this.address = this.model.address ?? ''; this.port = `${this.model.port ?? 3389}`;
          this.username = this.model.username ?? ''; this.protocol = this.model.protocol ?? 'auto'; this.remember = this.model.remember ?? false;
          this.passwordEpoch++; this.selectPending = false;
        }
        if (this.showingPoc) { const s = native.pocStatus(); this.graphicsStatus = s.code === 0 ? `离线图形：${s.width} × ${s.height}，${s.frames} 帧` : `图形验证失败（${s.code}）`; }
      } catch { this.bridgeNotice = '应用状态读取失败'; }
    }, 100);
  }
  aboutToDisappear(): void { clearInterval(this.timer); this.release(); }
  onBackPress(): boolean { if (this.model.page !== 0) { native.appAction(2, '0', 0, 0); return true; } return false; }
  @Builder
  passwordInput() {
    TextInput({ placeholder: this.model.credentialLoaded ? '已从安全存储载入；输入可替换' : '请输入密码' })
      .type(InputType.Password).height(48).width('100%').enterKeyType(EnterKeyType.Go).accessibilityText('密码')
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
  @Builder
  connectionForm() {
    Scroll() {
      Column({ space: this.compact ? 10 : 16 }) {
        Text('FreeRemoteDesk').fontSize(this.compact ? 24 : 32).fontWeight(FontWeight.Bold).fontColor($r('app.color.text_primary'))
        Text('连接远程桌面').fontSize(18).fontColor($r('app.color.text_secondary'))
        if ((this.model.profiles?.length ?? 0) > 0) {
          Text('最近连接').fontSize(14).width('100%').textAlign(TextAlign.Start)
          ForEach(this.model.profiles ?? [], (profile: ProfileSummary, index: number) => {
            Button(`${profile.address} · ${profile.username}`).height(48).width('100%').focusable(true)
              .onClick(() => { this.selectedIndex = index; this.selectPending = true; this.check(native.appAction(4, '0', 0, index)); })
          })
        }
        Text('目标系统').fontSize(14).width('100%').textAlign(TextAlign.Start)
        Select([{ value: 'Windows' }, { value: 'macOS' }, { value: 'Linux' }]).selected(this.target - 1).value(this.target === 1 ? 'Windows' : this.target === 2 ? 'macOS' : 'Linux')
          .width('100%').height(48).accessibilityText('目标系统').onSelect((index: number) => { this.target = index + 1; this.port = index === 1 ? '5900' : '3389'; this.edit(); })
        Text('主机地址').fontSize(14).width('100%').textAlign(TextAlign.Start)
        TextInput({ text: this.address, placeholder: '主机名或 IP 地址' }).height(48).accessibilityText('主机地址')
          .showError(this.error(this.model.errors?.address)).onChange((v: string) => { this.address = v; this.edit(); })
        Text('端口').fontSize(14).width('100%').textAlign(TextAlign.Start)
        TextInput({ text: this.port, placeholder: '端口' }).type(InputType.Number).height(48).accessibilityText('端口')
          .showError(this.error(this.model.errors?.port)).onChange((v: string) => { this.port = v; this.edit(); })
        Text('用户名').fontSize(14).width('100%').textAlign(TextAlign.Start)
        TextInput({ text: this.username, placeholder: '远程计算机账户' }).height(48).accessibilityText('用户名')
          .showError(this.error(this.model.errors?.username)).onChange((v: string) => { this.username = v; this.edit(); })
        Text('密码').fontSize(14).width('100%').textAlign(TextAlign.Start)
        ForEach([this.passwordEpoch], (_epoch: number) => { this.passwordInput() }, (epoch: number) => `${epoch}`)
        if (this.model.loading) { Text(this.model.loading).fontSize(14).width('100%').fontColor($r('app.color.text_secondary')) }
        if (this.model.credentialLoaded) { Text('已从系统安全存储载入密码').fontSize(14).width('100%') }
        Row({ space: 8 }) {
          Checkbox().select(this.remember).width(48).height(48).accessibilityText('在此设备安全保存登录信息')
            .onChange((value: boolean) => { this.remember = value; this.edit(); })
          Text('在此设备安全保存登录信息').fontSize(14).layoutWeight(1)
        }.width('100%')
        if (!this.model.available) { Text('此设备尚无可用的远程连接协议').fontSize(14).fontColor($r('app.color.text_secondary')).width('100%') }
        if (this.model.notice || this.bridgeNotice) { Text(this.bridgeNotice || this.model.notice || '').fontSize(14).width('100%').fontColor($r('app.color.text_secondary')) }
        Button('连接').height(48).width('100%').enabled(this.model.canConnect ?? false).focusable(true).accessibilityText('连接远程桌面').onClick(() => this.submit())
        if (native.pocAvailable()) {
          Button(this.showingPoc ? '停止离线图形验证' : '离线图形验证').height(48).width('100%').focusable(true)
            .onClick(() => { this.showingPoc = !this.showingPoc; })
          Button('验证系统安全存储').height(48).width('100%').focusable(true).enabled(this.model.probe !== 1)
            .onClick(() => { this.check(native.huksProbe()); })
          if (this.model.probe !== 0 && this.model.probe !== undefined) {
            Text(this.model.probe === 7 ? '安全存储加密、重载与篡改拒绝已验证' : this.model.probe === 1 ? '正在验证系统安全存储' : `系统安全存储验证失败（${this.model.probe}）`).fontSize(14).width('100%')
          }
        }
      }.width('100%').constraintSize({ maxWidth: 440 }).padding({ top: 24, bottom: 24 }).alignItems(HorizontalAlign.Center)
    }.width('100%').layoutWeight(1).scrollBar(BarState.Auto).padding({ left: 24, right: 24 })
  }
  @Builder
  remoteSurface() {
    XComponent({ id: 'frdRemoteSurface', type: XComponentType.SURFACE, libraryname: 'entry', controller: this.controller })
      .width('100%').layoutWeight(1).focusable(true).accessibilityText('远程桌面')
      .onAreaChange((_, area) => { this.release(); this.surfaceWidth = Math.round(this.getUIContext().vp2px(Number(area.width))); this.surfaceHeight = Math.round(this.getUIContext().vp2px(Number(area.height))); this.geometry(); })
      .onFocus(() => this.focus(true)).onBlur(() => this.focus(false))
      .onTouch((event: TouchEvent) => {
        if (event.type === TouchType.Cancel) { this.release(); return; }
        if (event.touches.length > 1) { this.release(); return; }
        const touch = event.changedTouches[0]; if (!touch) { return; }
        if (event.type === TouchType.Down) { this.focus(true); }
        this.pointer(touch.x, touch.y, event.type === TouchType.Up ? 0 : 1);
      })
      .onMouse((event: MouseEvent) => {
        if (event.action === MouseAction.Press) { this.focus(true); }
        let buttons = 0;
        for (const button of event.pressedButtons ?? []) {
          if (button === MouseButton.Left) { buttons |= 1; }
          if (button === MouseButton.Middle) { buttons |= 2; }
          if (button === MouseButton.Right) { buttons |= 4; }
        }
        this.pointer(event.x, event.y, buttons);
      })
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
    const status = this.model.persistenceWarning || this.model.notice || (this.model.page === 1 ? '正在连接' : this.model.page === 2 ? '等待首个完整画面' : this.model.page === 3 ? '已连接' : this.model.page === 4 ? '正在断开' : '连接失败');
    AlertDialog.show({ title: '连接状态', message: `${status}\n${identity}`, confirm: { value: '关闭', action: () => {} } });
  }
  @Builder
  sessionChrome() {
    Stack({ alignContent: Alignment.Center }) {
      // 空白区域仍属平台标题区，不吞掉按钮或系统三键事件。
      Row().width('100%').height(56)
      Row({ space: 8 }) {
        if (this.windowWidth > this.nativeButtonsWidth * 2 + 360) {
          Text(this.model.page === 0 ? 'FreeRemoteDesk' : this.model.page === 1 ? '正在连接' : this.model.page === 2 ? '等待画面' : this.model.page === 3 ? '已连接' : this.model.page === 4 ? '正在断开' : '连接失败')
            .fontSize(14).fontColor($r('app.color.text_primary')).maxLines(1)
        }
        if (this.model.page !== 0) {
          if (this.windowWidth > this.nativeButtonsWidth * 2 + 160) {
          Button('详情', { type: ButtonType.Normal }).height(48).constraintSize({ minWidth: 48 }).focusable(true)
            .accessibilityText('连接状态与服务器身份详情').onFocus(() => this.release()).onClick(() => this.details())
          }
          if (this.model.page === 1 || this.model.page === 2 || this.model.page === 3) {
            Button(this.model.page === 3 ? '断开' : '取消', { type: ButtonType.Normal }).height(48).constraintSize({ minWidth: 48 }).focusable(true)
              .accessibilityText(this.model.page === 3 ? '断开远程连接' : '取消连接').onFocus(() => this.release()).onClick(() => { this.release(); this.check(native.appAction(2, '0', 0, 0)); })
          } else if (this.model.page === 5) {
            Button('返回', { type: ButtonType.Normal }).height(48).constraintSize({ minWidth: 48 }).focusable(true)
              .accessibilityText('返回连接表单').onFocus(() => this.release()).onClick(() => { this.check(native.appAction(3, '0', 0, 0)); })
          }
        }
      }.constraintSize({ maxWidth: Math.max(0, this.windowWidth - this.nativeButtonsWidth * 2 - 16) })
    }.width('100%').height(56).backgroundColor($r('app.color.page_background'))
  }
  build() {
    Column() {
      if (this.nativeChrome || this.model.page !== 0) { this.sessionChrome() }
      if (this.showingPoc) {
        Column({ space: 8 }) {
          Button('停止离线图形验证').height(48).onClick(() => { this.showingPoc = false; })
          Text(this.graphicsStatus).fontSize(14).fontColor($r('app.color.text_primary'))
          XComponent({ id: 'frdPocSurface', type: XComponentType.SURFACE, libraryname: 'entry', controller: this.controller })
            .width('100%').layoutWeight(1).accessibilityText('离线图形测试表面')
        }.width('100%').layoutWeight(1)
      } else if (this.model.page === 0) {
        this.connectionForm()
      } else {
        // session controls/status 由平台 window chrome 接管，不占用 remote-content toolbar。
        this.remoteSurface()
      }
    }.width('100%').height('100%')
      .padding({ left: this.getUIContext().px2vp(this.cutoutLeft), right: this.getUIContext().px2vp(this.cutoutRight), top: this.getUIContext().px2vp(this.cutoutTop), bottom: this.getUIContext().px2vp(this.cutoutBottom) })
      .backgroundColor($r('app.color.page_background'))
      .onKeyEvent((event: KeyEvent) => this.remoteKey(event))
      .onAreaChange((_, area) => { this.compact = Number(area.height) < 500; this.windowWidth = Number(area.width); })
  }
}
