import { ConfigurationConstant, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { hilog } from '@kit.PerformanceAnalysisKit';
import native from 'libentry.so';
import { BusinessError, deviceInfo } from '@kit.BasicServicesKit';

export default class EntryAbility extends UIAbility {
  private applyGraphicsMode(want: Want): void {
    // 只有 debug 原生桥允许离线图形启动参数；无地址、协议或凭据。
    const requested = want.parameters?.['frd_graphics_poc'] === true;
    const accepted = requested && native.pocAvailable();
    AppStorage.setOrCreate('frd_graphics_poc', accepted);
    hilog.info(0xFF00, 'FrdOhos', '离线图形入口 requested=%{public}s accepted=%{public}s', `${requested}`, `${accepted}`);
  }
  private mainWindow?: window.Window;
  private contentReady: boolean = false;
  private desktop: boolean = deviceInfo.deviceType === '2in1' || deviceInfo.deviceType === 'pc';
  private lastInsets: string = '';
  private windowError(step: string, error: BusinessError): void {
    hilog.warn(0xFF00, 'FrdOhos', '窗口操作 %{public}s code=%{public}d', step, error.code);
  }
  private updateChrome(): void {
    if (!this.mainWindow || !this.contentReady || !this.desktop) { return; }
    // UIContent 加载后按设备模式调用；首次布局前 button rect 可以暂时为零。
    try { this.mainWindow.setWindowDecorVisible(false); } catch (e) { this.windowError('hide', e as BusinessError); }
    try { this.mainWindow.setWindowDecorHeight(56); } catch (e) { this.windowError('height', e as BusinessError); }
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
    if (!this.mainWindow) { return; }
    try {
      const cutout = this.mainWindow.getWindowAvoidArea(window.AvoidAreaType.TYPE_CUTOUT);
      const system = this.mainWindow.getWindowAvoidArea(window.AvoidAreaType.TYPE_SYSTEM);
      const rect = this.mainWindow.getWindowProperties().windowRect;
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
      AppStorage.setOrCreate('frd_cutout_left', Math.max(0, left - systemLeft));
      AppStorage.setOrCreate('frd_cutout_right', Math.max(0, right - systemRight));
      AppStorage.setOrCreate('frd_cutout_top', Math.max(0, top - systemTop));
      AppStorage.setOrCreate('frd_cutout_bottom', Math.max(0, bottom - systemBottom));
      const geometry = `window=${rect.width},${rect.height} cutout=${JSON.stringify(cutout)} system=${JSON.stringify(system)}`;
      if (geometry !== this.lastInsets) { this.lastInsets = geometry; hilog.info(0xFF00, 'FrdOhos', '安全区 %{public}s', geometry); }
    } catch (e) { native.appFocus(false, false, false); this.windowError('insets', e as BusinessError); }
  }
  onCreate(want: Want): void {
    native.initializeApp(`${this.context.filesDir}/FreeRemoteDesk`);
    AppStorage.setOrCreate('frd_cutout_left', 0); AppStorage.setOrCreate('frd_cutout_right', 0);
    AppStorage.setOrCreate('frd_cutout_top', 0); AppStorage.setOrCreate('frd_cutout_bottom', 0);
    AppStorage.setOrCreate('frd_native_buttons_width', 0);
    AppStorage.setOrCreate('frd_native_chrome', false);
    this.applyGraphicsMode(want);
  }
  onNewWant(want: Want): void { this.applyGraphicsMode(want); }
  onWindowStageCreate(windowStage: window.WindowStage): void {
    try {
      this.context.getApplicationContext().setColorMode(ConfigurationConstant.ColorMode.COLOR_MODE_NOT_SET);
      this.mainWindow = windowStage.getMainWindowSync();
      this.updateCutout();
      this.mainWindow.on('avoidAreaChange', () => { native.appFocus(false, false, false); this.updateCutout(); });
      this.mainWindow.on('windowSizeChange', () => { native.appFocus(false, false, false); this.updateCutout(); });
      this.mainWindow.on('windowEvent', (event: window.WindowEventType) => {
        if (event === window.WindowEventType.WINDOW_ACTIVE) { this.updateChrome(); }
        if (event === window.WindowEventType.WINDOW_INACTIVE) { native.appFocus(false, false, false); }
      });
      windowStage.loadContent('pages/Index', (error) => {
        if (error.code) { hilog.error(0xFF00, 'FrdOhos', '页面加载失败：%{public}d', error.code); return; }
        this.contentReady = true;
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
  onForeground(): void { native.setForeground(true); hilog.info(0xFF00, 'FrdOhos', '前台恢复'); }
  onBackground(): void { native.setForeground(false); hilog.info(0xFF00, 'FrdOhos', '后台暂停'); }
  onWindowStageDestroy(): void {
    native.setForeground(false);
    if (this.mainWindow) {
      try { this.mainWindow.off('avoidAreaChange'); this.mainWindow.off('windowSizeChange'); this.mainWindow.off('windowEvent'); if (this.desktop) { this.mainWindow.off('windowTitleButtonRectChange'); } }
      catch (e) { this.windowError('unsubscribe', e as BusinessError); }
    }
    this.contentReady = false;
    this.mainWindow = undefined;
  }
  onDestroy(): void { native.setForeground(false); native.shutdownApp(); }
}
