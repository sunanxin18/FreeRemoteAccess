import { ConfigurationConstant, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { hilog } from '@kit.PerformanceAnalysisKit';
import native from 'libentry.so';

export default class EntryAbility extends UIAbility {
  private applyGraphicsMode(want: Want): void {
    // 只有 debug 原生桥允许离线图形启动参数；无地址、协议或凭据。
    const requested = want.parameters?.['frd_graphics_poc'] === true;
    const accepted = requested && native.pocAvailable();
    AppStorage.setOrCreate('frd_graphics_poc', accepted);
    hilog.info(0xFF00, 'FrdOhos', '离线图形入口 requested=%{public}s accepted=%{public}s', `${requested}`, `${accepted}`);
  }
  onCreate(want: Want): void { this.applyGraphicsMode(want); }
  onNewWant(want: Want): void { this.applyGraphicsMode(want); }
  onWindowStageCreate(windowStage: window.WindowStage): void {
    try {
      this.context.getApplicationContext().setColorMode(ConfigurationConstant.ColorMode.COLOR_MODE_NOT_SET);
      windowStage.loadContent('pages/Index', (error) => {
        if (error.code) { hilog.error(0xFF00, 'FrdOhos', '页面加载失败：%{public}d', error.code); }
      });
    } catch {
      hilog.error(0xFF00, 'FrdOhos', '窗口初始化失败');
    }
  }
  onForeground(): void { native.setForeground(true); hilog.info(0xFF00, 'FrdOhos', '前台恢复'); }
  onBackground(): void { native.setForeground(false); hilog.info(0xFF00, 'FrdOhos', '后台暂停'); }
  onWindowStageDestroy(): void { native.setForeground(false); }
  onDestroy(): void { native.setForeground(false); }
}
