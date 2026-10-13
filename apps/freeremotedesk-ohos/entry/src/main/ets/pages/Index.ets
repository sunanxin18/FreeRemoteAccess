import native from 'libentry.so';
import { hilog } from '@kit.PerformanceAnalysisKit';

@Entry
@Component
struct Index {
  @StorageLink('frd_graphics_poc') @Watch('onGraphicsModeChanged') private showingPoc: boolean = false;
  @State private compact: boolean = false;
  @State private status: string = '尚未启动图形验证';
  private controller: XComponentController = new XComponentController();
  private timer: number = -1;
  private onGraphicsModeChanged(): void {
    this.status = !native.pocAvailable() ? '发布版本不提供离线图形验证入口' :
      (this.showingPoc ? '正在创建原生图形表面' : '图形验证已停止');
  }
  aboutToAppear(): void {
    if (!native.pocAvailable()) { this.status = '发布版本不提供离线图形验证入口'; }
    this.timer = setInterval(() => {
      if (this.showingPoc) {
        const snapshot = native.pocStatus();
        hilog.info(0xFF00, 'FrdOhos', '离线图形 POC code=%{public}d size=%{public}d,%{public}d frames=%{public}s foreground=%{public}s', snapshot.code, snapshot.width, snapshot.height, snapshot.frames, `${snapshot.foreground}`);
        this.status = snapshot.code === 0 ? `图形验证：${snapshot.width} × ${snapshot.height}，已呈现 ${snapshot.frames} 帧` : `图形验证失败（错误 ${snapshot.code}）`;
      }
    }, 1000);
  }
  aboutToDisappear(): void { clearInterval(this.timer); }
  build() {
    Column({ space: this.compact ? 8 : 16 }) {
      Text('FreeRemoteDesk').fontSize(this.compact ? 20 : 28).fontWeight(FontWeight.Bold).fontColor($r('app.color.text_primary'))
      Text('离线图形验证 · 开发用途').fontSize(18).fontColor($r('app.color.text_primary'))
      Text('四色图案用于验证 XComponent 与 GPU 生命周期。此页面尚未提供远程登录。')
        .fontSize(16).fontColor($r('app.color.text_secondary')).textAlign(TextAlign.Center)
        .visibility(this.compact ? Visibility.None : Visibility.Visible)
      Button(this.showingPoc ? '停止图形验证' : '启动图形验证')
        .enabled(native.pocAvailable()).height(48).constraintSize({ minWidth: 180 }).focusable(true)
        .accessibilityText(this.showingPoc ? '停止原生图形验证' : '启动原生图形验证')
        .onClick(() => { this.showingPoc = !this.showingPoc; })
      Text(this.status).fontSize(14).fontColor($r('app.color.text_secondary')).textAlign(TextAlign.Center)
      if (this.showingPoc) {
        XComponent({ id: 'frdPocSurface', type: XComponentType.SURFACE, libraryname: 'entry', controller: this.controller })
          .width('100%').layoutWeight(1).accessibilityText('原生图形测试表面：四色与白色十字')
      } else {
        Blank().layoutWeight(1)
      }
    }.width('100%').height('100%').padding(this.compact ? 12 : 24)
      .onAreaChange((_, area) => {
        const compact = Number(area.height) < 500;
        if (this.compact !== compact) { this.compact = compact; }
      })
      .backgroundColor($r('app.color.page_background'))
  }
}
