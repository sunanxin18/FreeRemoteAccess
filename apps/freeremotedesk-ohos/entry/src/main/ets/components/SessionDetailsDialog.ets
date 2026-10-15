@CustomDialog
export struct SessionDetailsDialog {
  controller?: CustomDialogController;
  message: string = '';
  closed: () => void = () => {};
  observeKey: (event: KeyEvent) => void = (_event: KeyEvent) => {};
  build() {
    Column({ space: 16 }) {
      Text('连接状态与服务器身份').fontSize(20).fontWeight(FontWeight.Bold).fontColor($r('app.color.text_primary')).width('100%')
      Scroll() {
        Text(this.message).fontSize(16).fontColor($r('app.color.text_primary')).width('100%').textAlign(TextAlign.Start)
      }.width('100%').constraintSize({ maxHeight: '55%' }).scrollBar(BarState.Auto)
      Button('关闭').constraintSize({ minHeight: 48 }).width('100%').defaultFocus(true).accessibilityText('关闭连接详情')
        .onClick(() => { this.controller?.close(); this.closed(); })
    }.padding(24).width('100%').backgroundColor($r('app.color.surface')).borderRadius(20)
      .onKeyEventDispatch((event: KeyEvent): boolean => { this.observeKey(event); return false; })
  }
}
