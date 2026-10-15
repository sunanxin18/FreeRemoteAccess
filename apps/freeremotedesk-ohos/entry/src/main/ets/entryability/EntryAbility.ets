import { ConfigurationConstant, UIAbility } from '@kit.AbilityKit';
import { window, display } from '@kit.ArkUI';
import { hilog } from '@kit.PerformanceAnalysisKit';
import native from 'libentry.so';
import { BusinessError, deviceInfo } from '@kit.BasicServicesKit';
import { PhoneFullscreenPolicy, PhoneFullscreenRequest } from '../common/PhoneFullscreenPolicy';
import { PhoneOrientation, PhoneOrientationPolicy, PHONE_SESSION_ORIENTATION_EVENT } from '../common/PhoneOrientationPolicy';

import { LocalDisplayPolicy, LocalDisplayGeometry, DisplayTicket, makeDisplayRect, validDisplayWorkArea } from '../common/LocalDisplayPolicy';

export default class EntryAbility extends UIAbility {
  private localDisplay: LocalDisplayPolicy = new LocalDisplayPolicy();
  private observedDisplay?: display.Display;
  private localDisplayChanged = (): void => { this.refreshLocalDisplay(); };
  private systemDisplayChanged = (_id: number): void => { this.refreshLocalDisplay(); };
  private sendLocalDisplay(ticket: DisplayTicket, geometry: LocalDisplayGeometry | null, reason: string | null): void {
    if (!this.localDisplay.isCurrent(ticket)) { return; }
    const result = native.appLocalDisplay(JSON.stringify(this.localDisplay.message(ticket, deviceInfo.deviceType === 'phone', geometry, reason)));
    if (result !== 0) {
      hilog.warn(0xFF00, 'FrdOhos', '本地显示消息失败 code=%{public}d', result);
      if (geometry !== null) {
        const invalid = this.localDisplay.begin(ticket.windowId);
        native.appLocalDisplay(JSON.stringify(this.localDisplay.message(invalid, deviceInfo.deviceType === 'phone', null, 'invalid_geometry')));
      }
    }
  }
  private clearLocalDisplay(reason: string): void {
    const ticket = this.localDisplay.begin(0);
    this.sendLocalDisplay(ticket, null, reason);
  }
  private observeDisplay(current: display.Display): void {
    if (this.observedDisplay && this.observedDisplay.id === current.id) { return; }
    this.unobserveDisplay();
    this.observedDisplay = current;
    try { current.on('availableAreaChange', this.localDisplayChanged); }
    catch (e) { this.windowError('工作区监听', e as BusinessError); }
  }
  private unobserveDisplay(): void {
    if (this.observedDisplay) {
      try { this.observedDisplay.off('availableAreaChange', this.localDisplayChanged); }
      catch (e) { this.windowError('撤销工作区监听', e as BusinessError); }
    }
    this.observedDisplay = undefined;
  }
  private refreshLocalDisplay(): void {
    const currentWindow = this.mainWindow;
    if (!currentWindow || !this.contentReady) { this.clearLocalDisplay('window_not_ready'); return; }
    if (!this.foreground) { this.clearLocalDisplay('background'); return; }
    // 每次先失效旧 revision，即使本轮同步 API 失败也不能残留旧显示器快照。
    let ticket = this.localDisplay.begin(0);
    try {
      const properties = currentWindow.getWindowProperties();
      if (properties.displayId === undefined || !Number.isSafeInteger(properties.displayId) || properties.displayId < 0) {
        this.sendLocalDisplay(ticket, null, 'display_unavailable'); return;
      }
      const currentDisplay = display.getDisplayByIdSync(properties.displayId);
      this.observeDisplay(currentDisplay);
      ticket = this.localDisplay.begin(properties.id);
      const geometry: LocalDisplayGeometry = {
        displayId: currentDisplay.id, width: currentDisplay.width, height: currentDisplay.height,
        density: currentDisplay.densityPixels, rotation: currentDisplay.rotation,
        window: makeDisplayRect(properties.windowRect.left, properties.windowRect.top, properties.windowRect.width, properties.windowRect.height),
        drawable: makeDisplayRect(properties.drawableRect.left, properties.drawableRect.top, properties.drawableRect.width, properties.drawableRect.height), workArea: null,
        fullscreen: properties.isLayoutFullScreen
      };
      // 先发布真实屏幕及保守 drawable 工作区，不让可选异步 API 阻塞首次连接。
      this.sendLocalDisplay(ticket, geometry, null);
      try {
        currentDisplay.getAvailableArea().then((area: display.Rect) => {
          if (this.mainWindow !== currentWindow || !this.localDisplay.isCurrent(ticket)) { return; }
          // 回调期间系统属性可能先变、通知后到；再核对所属屏、方向、尺寸及窗口。
          try {
            const latest = currentWindow.getWindowProperties();
            const latestDisplay = display.getDisplayByIdSync(currentDisplay.id);
            if (latest.displayId !== properties.displayId || latest.id !== properties.id ||
              latestDisplay.width !== geometry.width || latestDisplay.height !== geometry.height ||
              latestDisplay.rotation !== geometry.rotation || latestDisplay.densityPixels !== geometry.density ||
              latest.windowRect.left !== geometry.window.left || latest.windowRect.top !== geometry.window.top ||
              latest.windowRect.width !== geometry.window.width || latest.windowRect.height !== geometry.window.height ||
              latest.drawableRect.left !== geometry.drawable.left || latest.drawableRect.top !== geometry.drawable.top ||
              latest.drawableRect.width !== geometry.drawable.width || latest.drawableRect.height !== geometry.drawable.height) {
              this.refreshLocalDisplay(); return;
            }
            const workArea = makeDisplayRect(area.left, area.top, area.width, area.height);
            if (!validDisplayWorkArea(workArea, geometry.width, geometry.height)) {
              const fallback = this.localDisplay.begin(properties.id);
              this.sendLocalDisplay(fallback, geometry, 'invalid_work_area');
              return;
            }
            geometry.workArea = workArea;
            const completed = this.localDisplay.begin(properties.id);
            this.sendLocalDisplay(completed, geometry, null);
          } catch (e) {
            this.clearLocalDisplay('display_unavailable');
            this.windowError('工作区完成时读取窗口', e as BusinessError);
          }
        }).catch((error: BusinessError) => {
          if (this.mainWindow === currentWindow && this.localDisplay.isCurrent(ticket)) { this.windowError('读取工作区（使用保守内容区）', error); }
        });
      } catch (e) { this.windowError('读取工作区（使用保守内容区）', e as BusinessError); }
    } catch (e) {
      this.sendLocalDisplay(ticket, null, 'display_unavailable');
      this.windowError('读取本地显示', e as BusinessError);
    }
  }
  private mainWindow?: window.Window;
  private windowDisplayChanged?: () => void;
  private contentReady: boolean = false;
  private foreground: boolean = false;
  private windowActive: boolean = true;
  private publishActive(): void { AppStorage.setOrCreate('frd_window_active', this.foreground && this.windowActive); }
  private desktop: boolean = deviceInfo.deviceType === '2in1' || deviceInfo.deviceType === 'pc';
  private lastInsets: string = '';
  private phoneOrientation: PhoneOrientationPolicy = new PhoneOrientationPolicy();
  private orientationAvailable: boolean = false;
  private phoneFullscreen: PhoneFullscreenPolicy = new PhoneFullscreenPolicy();
  private sessionOrientationChanged = (page: number, session: string): void => {
    this.phoneFullscreen.observe(page, session);
    this.applyPhoneFullscreen();
    this.phoneOrientation.observe(page, session);
    this.applyPhoneOrientation();
  };
  private applyPhoneFullscreen(): void {
    if (deviceInfo.deviceType !== 'phone' || !this.mainWindow || !this.contentReady) { return; }
    const request = this.phoneFullscreen.nextRequest();
    if (!request) { return; }
    this.setPhoneFullscreen(this.mainWindow, request);
  }
  private async setPhoneFullscreen(currentWindow: window.Window, request: PhoneFullscreenRequest): Promise<void> {
    try {
      native.appFocus(false, false, false);
      await currentWindow.setWindowLayoutFullScreen(request.fullscreen);
      if (this.mainWindow !== currentWindow) { return; }
      // 即使请求在 await 期间过期，也先读取实际布局，再由最新请求恢复。
      this.updateCutout();
      if (!this.phoneFullscreen.isCurrent(request)) { return; }
      await currentWindow.setWindowSystemBarEnable(request.fullscreen ? [] : ['status', 'navigation']);
      if (this.mainWindow !== currentWindow || !this.phoneFullscreen.isCurrent(request)) { return; }
      // 手势导航条不属于上面的 navigation（三键导航）；需独立处理。
      if (canIUse('SystemCapability.Window.SessionManager')) {
        await currentWindow.setSpecificSystemBarEnabled('navigationIndicator', !request.fullscreen, false);
      }
      if (this.mainWindow === currentWindow && this.phoneFullscreen.isCurrent(request)) { this.updateCutout(); }
    } catch (e) {
      if (this.mainWindow === currentWindow) { this.updateCutout(); this.windowError('手机会话全屏', e as BusinessError); }
    } finally {
      if (this.phoneFullscreen.complete(request)) { this.applyPhoneFullscreen(); }
    }
  }
  private applyPhoneOrientation(): void {
    if (deviceInfo.deviceType !== 'phone' || !this.mainWindow || !this.orientationAvailable) { return; }
    const request = this.phoneOrientation.nextRequest();
    if (!request) { return; }
    const currentWindow = this.mainWindow;
    const orientation = request.orientation === PhoneOrientation.Landscape ?
      window.Orientation.USER_ROTATION_LANDSCAPE : window.Orientation.USER_ROTATION_PORTRAIT;
    try {
      // 同一窗口串行申请；只处理最新偏好，不因轮询、旋转或前后台通知重新强制方向。
      currentWindow.setPreferredOrientation(orientation).then(() => {
        if (this.phoneOrientation.complete(request)) { this.applyPhoneOrientation(); }
      }).catch((error: BusinessError) => {
        if (this.phoneOrientation.complete(request)) {
          this.windowError('手机旋转', error);
          this.applyPhoneOrientation();
        }
      });
    } catch (e) {
      if (this.phoneOrientation.complete(request)) {
        this.windowError('手机旋转', e as BusinessError);
        this.applyPhoneOrientation();
      }
    }
  }
  private windowError(step: string, error: BusinessError): void {
    hilog.warn(0xFF00, 'FrdOhos', '窗口操作 %{public}s code=%{public}d', step, error.code);
  }
  private updateChrome(): void {
    if (!this.mainWindow || !this.contentReady || !this.desktop) { return; }
    // UIContent 加载后按设备模式调用；首次布局前 button rect 可以暂时为零。
    // 会话动作改由右下浮动岛承载；系统标题栏保留原生三键、拖动和系统菜单。
    try { this.mainWindow.setWindowDecorVisible(true); } catch (e) { this.windowError('decor', e as BusinessError); }
    try { this.mainWindow.setWindowTitleMoveEnabled(true); } catch (e) { this.windowError('move', e as BusinessError); }
    try {
      const visible = this.mainWindow.getWindowDecorVisible();
      const height = this.mainWindow.getWindowDecorHeight();
      const buttons = this.mainWindow.getTitleButtonRect();
      AppStorage.setOrCreate('frd_native_chrome', !visible);
      AppStorage.setOrCreate('frd_native_buttons_width', buttons.width + buttons.right);
      hilog.info(0xFF00, 'FrdOhos', '窗口装饰 visible=%{public}s height=%{public}d buttons=%{public}d', `${visible}`, height, buttons.width);
    } catch (e) { this.windowError('readback', e as BusinessError); }
  }
  private updateCutout(): void {
    this.refreshLocalDisplay();
    if (!this.mainWindow) { return; }
    try {
      const cutout = this.mainWindow.getWindowAvoidArea(window.AvoidAreaType.TYPE_CUTOUT);
      const system = this.mainWindow.getWindowAvoidArea(window.AvoidAreaType.TYPE_SYSTEM);
      const properties = this.mainWindow.getWindowProperties();
      const rect = properties.windowRect;
      const immersive = deviceInfo.deviceType === 'phone' && properties.isLayoutFullScreen;
      AppStorage.setOrCreate('frd_phone_fullscreen', immersive);
      let navigationRight: number = 0;
      let navigationBottom: number = 0;
      if (immersive) {
        const navigation = this.mainWindow.getWindowAvoidArea(window.AvoidAreaType.TYPE_NAVIGATION_INDICATOR);
        navigationRight = navigation.rightRect.width > 0 ? rect.width - navigation.rightRect.left : 0;
        navigationBottom = navigation.bottomRect.height > 0 ? rect.height - navigation.bottomRect.top : 0;
      }
      // AvoidArea 与 windowRect 使用物理像素、窗口内坐标。默认内容已经避让 system；
      // 仅补 cutout 与剩余内容的交集，left/top 必须取远边，不能只取孔洞宽高。
      const left = cutout.leftRect.width > 0 ? cutout.leftRect.left + cutout.leftRect.width : 0;
      const right = cutout.rightRect.width > 0 ? rect.width - cutout.rightRect.left : 0;
      const top = cutout.topRect.height > 0 ? cutout.topRect.top + cutout.topRect.height : 0;
      const bottom = cutout.bottomRect.height > 0 ? rect.height - cutout.bottomRect.top : 0;
      const systemLeft = system.leftRect.width > 0 ? system.leftRect.left + system.leftRect.width : 0;
      const systemRight = system.rightRect.width > 0 ? rect.width - system.rightRect.left : 0;
      const systemTop = system.topRect.height > 0 ? system.topRect.top + system.topRect.height : 0;
      const systemBottom = system.bottomRect.height > 0 ? rect.height - system.bottomRect.top : 0;
      AppStorage.setOrCreate('frd_island_safe_right', immersive ? Math.max(right, systemRight, navigationRight) : 0);
      AppStorage.setOrCreate('frd_island_safe_bottom', immersive ? Math.max(bottom, systemBottom, navigationBottom) : 0);
      AppStorage.setOrCreate('frd_cutout_left', Math.max(0, left - systemLeft));
      AppStorage.setOrCreate('frd_cutout_right', Math.max(0, right - systemRight));
      AppStorage.setOrCreate('frd_cutout_top', Math.max(0, top - systemTop));
      AppStorage.setOrCreate('frd_cutout_bottom', Math.max(0, bottom - systemBottom));
      const geometry = `window=${rect.width},${rect.height} cutout=${JSON.stringify(cutout)} system=${JSON.stringify(system)}`;
      if (geometry !== this.lastInsets) { this.lastInsets = geometry; hilog.info(0xFF00, 'FrdOhos', '安全区 %{public}s', geometry); }
    } catch (e) { native.appFocus(false, false, false); this.windowError('insets', e as BusinessError); }
  }
  onCreate(): void {
    native.initializeApp(`${this.context.filesDir}/FreeRemoteDesk`);
    if (deviceInfo.deviceType === 'phone') {
      this.context.eventHub.on(PHONE_SESSION_ORIENTATION_EVENT, this.sessionOrientationChanged);
    }
    AppStorage.setOrCreate('frd_cutout_left', 0); AppStorage.setOrCreate('frd_cutout_right', 0);
    AppStorage.setOrCreate('frd_cutout_top', 0); AppStorage.setOrCreate('frd_cutout_bottom', 0);
    AppStorage.setOrCreate('frd_phone_fullscreen', false);
    AppStorage.setOrCreate('frd_island_safe_right', 0); AppStorage.setOrCreate('frd_island_safe_bottom', 0);
    AppStorage.setOrCreate('frd_native_buttons_width', 0);
    AppStorage.setOrCreate('frd_native_chrome', false);
    AppStorage.setOrCreate('frd_window_active', false);
  }
  onWindowStageCreate(windowStage: window.WindowStage): void {
    try {
      this.context.getApplicationContext().setColorMode(ConfigurationConstant.ColorMode.COLOR_MODE_NOT_SET);
      this.contentReady = false;
      this.localDisplay.attach();
      this.mainWindow = windowStage.getMainWindowSync();
      const createdWindow = this.mainWindow;
      const createdStage = this.localDisplay.stageId();
      this.windowActive = true; this.publishActive();
      if (deviceInfo.deviceType === 'phone') {
        this.phoneOrientation.attachWindow();
        this.phoneFullscreen.attachWindow();
        try {
          this.orientationAvailable = canIUse('SystemCapability.Window.SessionManager');
          if (this.orientationAvailable) { this.applyPhoneOrientation(); }
          else { hilog.warn(0xFF00, 'FrdOhos', '手机旋转能力不可用'); }
        } catch (e) {
          this.orientationAvailable = false;
          this.windowError('手机旋转能力', e as BusinessError);
        }
      }
      this.updateCutout();
      const currentWindow = (): boolean => this.mainWindow === createdWindow && this.localDisplay.stageId() === createdStage;
      // 软键盘避让/尺寸通知不是 blur；真实内容变化由 XComponent/Index geometry 清 held。
      this.mainWindow.on('avoidAreaChange', () => { if (currentWindow()) { this.updateCutout(); } });
      this.mainWindow.on('windowSizeChange', () => { if (currentWindow()) { this.updateCutout(); } });
      this.mainWindow.on('windowEvent', (event: window.WindowEventType) => {
        if (!currentWindow()) { return; }
        if (event === window.WindowEventType.WINDOW_ACTIVE) { this.windowActive = true; this.publishActive(); this.updateChrome(); }
        if (event === window.WindowEventType.WINDOW_INACTIVE) { this.windowActive = false; this.publishActive(); native.appFocus(false, false, false); }
      });
      this.windowDisplayChanged = (): void => { if (currentWindow()) { this.refreshLocalDisplay(); } };
      try { this.mainWindow.on('windowRectChange', this.windowDisplayChanged); } catch (e) { this.windowError('窗口几何监听', e as BusinessError); }
      try { this.mainWindow.on('displayIdChange', this.windowDisplayChanged); } catch (e) { this.windowError('窗口显示器监听', e as BusinessError); }
      try { display.on('change', this.systemDisplayChanged); display.on('add', this.systemDisplayChanged); display.on('remove', this.systemDisplayChanged); }
      catch (e) { this.windowError('显示器监听', e as BusinessError); }
      windowStage.loadContent('pages/Index', (error) => {
        if (this.mainWindow !== createdWindow || this.localDisplay.stageId() !== createdStage) { return; }
        if (error.code) { hilog.error(0xFF00, 'FrdOhos', '页面加载失败：%{public}d', error.code); return; }
        this.contentReady = true;
        this.refreshLocalDisplay();
        this.applyPhoneFullscreen();
        this.updateChrome();
        if (this.mainWindow && this.desktop) {
          this.mainWindow.on('windowTitleButtonRectChange', (updated: window.TitleButtonRect) => {
            AppStorage.setOrCreate('frd_native_buttons_width', updated.width + updated.right);
          });
        }
      });
    } catch {
      hilog.error(0xFF00, 'FrdOhos', '窗口初始化失败');
    }
  }
  onForeground(): void { this.foreground = true; this.publishActive(); native.setForeground(true); this.refreshLocalDisplay(); hilog.info(0xFF00, 'FrdOhos', '前台恢复'); }
  onBackground(): void { this.foreground = false; this.publishActive(); this.clearLocalDisplay('background'); native.setForeground(false); hilog.info(0xFF00, 'FrdOhos', '后台暂停'); }
  private detachLocalDisplay(): void {
    this.localDisplay.detach();
    this.clearLocalDisplay('window_destroyed');
    this.unobserveDisplay();
    try { display.off('change', this.systemDisplayChanged); } catch (e) { this.windowError('撤销显示器变化监听', e as BusinessError); }
    try { display.off('add', this.systemDisplayChanged); } catch (e) { this.windowError('撤销显示器新增监听', e as BusinessError); }
    try { display.off('remove', this.systemDisplayChanged); } catch (e) { this.windowError('撤销显示器移除监听', e as BusinessError); }
    if (this.mainWindow) {
      try { this.mainWindow.off('windowRectChange', this.windowDisplayChanged); } catch (e) { this.windowError('撤销窗口几何监听', e as BusinessError); }
      try { this.mainWindow.off('displayIdChange', this.windowDisplayChanged); } catch (e) { this.windowError('撤销窗口显示器监听', e as BusinessError); }
    }
  }
  onWindowStageDestroy(): void {
    this.detachLocalDisplay();
    this.windowActive = false; this.publishActive();
    this.phoneOrientation.detachWindow();
    this.phoneFullscreen.detachWindow();
    this.orientationAvailable = false;
    native.setForeground(false);
    if (this.mainWindow) {
      try { this.mainWindow.off('avoidAreaChange'); this.mainWindow.off('windowSizeChange'); this.mainWindow.off('windowEvent'); if (this.desktop) { this.mainWindow.off('windowTitleButtonRectChange'); } }
      catch (e) { this.windowError('unsubscribe', e as BusinessError); }
    }
    this.contentReady = false;
    this.mainWindow = undefined;
  }
  onDestroy(): void {
    this.detachLocalDisplay();
    this.foreground = false; this.publishActive();
    if (deviceInfo.deviceType === 'phone') {
      this.context.eventHub.off(PHONE_SESSION_ORIENTATION_EVENT, this.sessionOrientationChanged);
    }
    this.phoneOrientation.detachWindow();
    this.phoneFullscreen.detachWindow();
    this.orientationAvailable = false;
    native.setForeground(false); native.shutdownApp();
  }
}
