# FreeRemoteDesk 应用图标资产

`source/portal-foreground.png` 由内置 ImageGen 以 `remote3.jpeg` 为身份参考，在
`precise-object-edit` 模式下生成。生成提示词要求保留白色椭圆远程入口和中央旋涡，
移除灰色画布、深蓝底板、阴影、透视与预制圆角。由于首次结果把透明棋盘烘焙进了
RGB，正式结果改用纯黑哑光背景，再由导出工具按亮度恢复真实 alpha 并去除黑底。

正式平台资产由 `frd-icon-assets` 从透明前景和固定背景色
`rgba(6, 27, 69, 255)` 确定性生成：

```powershell
cargo run -p frd-icon-assets -- assets/app-icon/source/portal-foreground.png assets/app-icon
```

原始 `D:\FreeRemoteDesk\remote3.jpeg` 不会被修改或纳入生成流程的覆盖目标。
