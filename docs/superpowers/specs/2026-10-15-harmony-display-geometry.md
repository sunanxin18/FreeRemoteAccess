# 鸿蒙本地显示几何与远端请求尺寸

日期：2026-10-15。落实既有Task5D与用户优先本地显示器、允许超过2560×1440的要求。

## 真实几何与请求意图

DisplayGeometry必须保留窗口所在显示器当前实际物理像素、逻辑尺寸、densityPixels比例、工作区与drawable内容矩形。display/window/avoidArea原生字段已经为px，仅ArkUI vp回调转换一次。字体scaledDensity不进入几何。未知/无效快照不伪造为可用；记录非敏感降级原因，保留既有协议fallback。

手机登录页保持竖屏，成功完整首帧呈现后才按现有策略横屏。默认NativeDisplay请求需要优先使用屏幕长边为远端宽、高为短边；这是独立的远端显示方向偏好，不能交换实际geometry并称作真实快照。共享DisplayIntent增加显式方向偏好，默认Preserve，手机NativeDisplay显式Landscape。共享planner在NativeDisplay选出真实物理尺寸后投影，再执行原协议约束。Fixed、ServerManaged、DisplayWorkArea、WindowContent及其他平台默认方向保持原语义。此偏好不承诺动态resize。

## 平台接线

EntryAbility复用真实mainWindow与所属display取得快照。窗口/display/avoid-area及前台变化刷新；每个异步请求绑定window/stage/revision，迟到结果不能污染新窗口或下次连接。窗口销毁清快照并撤监听。

通过独立的local-display消息经NAPI与版本化C ABI传给OHOS Owner，不复用appGeometry（该消息属于渲染/输入viewport）。Owner在StartSession→begin_launch之间冻结最新有效快照，注入原DisplayIntent的geometry及适用的手机方向偏好；不能将NativeDisplay改为Fixed，也不能在共享controller依赖OHOS窗口API。不可用workarea有显式来源和降级；矩形以同一显示器局部坐标记录并严格检查边界、符号与溢出。

初始模式变化与旋转仅改变local snapshot和本地renderer/input geometry；已建立会话不会因此无条件重连或突破协议dynamic capability门。RDP u16/64MiB及Apple HP8像素对齐保留。Apple兼容标准模式不能从本地快照推断可改变服务器物理桌面。

## 验收

纯转换覆盖phone非整数density/portrait真实几何+landscape请求、PC4K、超2560、不重复density缩放、外接屏原点、drawable/titlebar、无workarea、零/负/NaN/越界、旧window/revision。Owner实际队列测试截获ConnectRequest，验证local snapshot确实进入NativeDisplay，Fixed/ServerManaged不变，失效快照不会泄漏到新launch。共享planner测试默认桌面、其他resolution modes不变；保持RDP和Apple既有尺寸约束回归。

构建与API声明不算真实协商证明。手机、PC分别记录系统快照、发出请求、服务端接受和首surface尺寸；检查手机连接前竖屏/首桌面后横屏、旋转不自动重连。PC原生窗口/显示器变化单独验收。真实几何、请求方向和接受尺寸必须分别可证，不以XComponent矩形冒充远程ACK。

此项使用既有共享DisplayPlanner，不添加协议或架构窗口条件；macOS仅ARM64，Windows/Linux原功能回归独立检查。没有新控件或GUI基线例外，沿用既有Material/Apple/ArkUI布局与焦点规范。

## 官方资料核对

2026-10-15重新读取[Material官方自适应布局](https://github.com/material-components/material-components-android/blob/master/docs/adaptive/CanonicalLayouts.md)与[OpenHarmony Display API](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/reference/apis-arkui/js-apis-display.md)，并核对本机API26 SDK。SDK明确WindowProperties.displayId为可选、drawableRect相对窗口，display尺寸为px；在线Display资料对手机可用工作区仍有限制，与本机SDK版本描述不完全相同，因此以实际异常/有效值判定可用性并保留降级。Apple HIG Layout正文及其Markdown与M3 window-size-classes本轮网络读取失败，不能称已读到其最新正文；既有设计基线继续适用，本项没有增加控件或批准可访问性例外。
