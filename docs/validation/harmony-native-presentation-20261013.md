# 鸿蒙原生会话呈现与输入验证

日期：2026-10-13。本记录是手机和 PC 正常签名 HAP 的合成会话验收；没有网络、真实认证、RDP/Apple 解码或真实远端桌面。独立审查发现的重试缺口已于 `bccb321` 修复并通过复审，最终 v5 包完成受限复验；不能以本记录标记整个鸿蒙客户端完成。

## 实现边界

会话帧通过共享 SessionHost 的 FrameMailbox 编译为 FrameTransaction，再以单批次有界通道交给原生渲染线程。原生窗口与 GPU 由该线程独占；真正提交到 compositor 的完整画面确认，才允许当前会话、远端 generation、本地 surface/lifecycle 与 geometry 对应的输入。队列背压保留事务顺序，暂停时不继续排空；失败采用固定诊断并清理会话。

共享核心新增带裁剪的平移/缩放视口，渲染和输入使用同一变换；现有 ContentViewport 和默认桌面接口保留。临时窗口变化、旋转和前后台切换保留已提交的完整纹理，撤销旧的本地就绪证明后重新绘制。普通增量确认不会自动升级为完整基线；部分状态、恢复状态和 GPU 失败不能凭重绘请求恢复完整性。

原生 XComponent Touch/Mouse/Focus/Blur 事件接入既有输入校验路径。ArkUI 重复触摸/鼠标生产者移除，密度来自当前窗口 UIContext，与 geometry 一起更新。等待画面、失焦或后台时禁止远端输入，但仍处理必要的本地释放和按键隔离状态。

## 真实图形检查发现并修复的问题

初版合成包的日志曾报告完整呈现，但手机竖屏截图显示红绿三角覆盖留黑区域。原因是默认超大三角形原来依赖 viewport 裁剪，新变换路径使用整个 drawable 后，采样超出纹理范围并夹取边缘颜色。后续改为有界四边形，并增加实际 Metal 像素回读，覆盖留黑和带裁剪的负偏移/分数缩放。初版包不计验收通过。

初版点击也没有进入会话：原生 Touch 是 POC 空回调，Mouse 尚未注册。接入原生回调后，手机和 PC 单次点击都恰好产生一次按下和一次释放。这说明编译成功、显示“已连接”或单个 present 计数，都不足以替代实际界面和输入检查。

## v3 合成包的手机与 PC 证据

同一包在专用 Phone API26、PC API26 ARM64 模拟器正常安装。unsigned SHA-256 `9042327d0bb1276fa7481a8bd8bf389a2bf9c68f433d8ce2c1e05341016c43df`，signed `f7ebf114f4174b82a596bee79b09c66081f69a3aca06f5d78cfb334a15a55ea0`。生产 HAP 检查、官方签名和签名验证均通过；测试 feature 还要求 debug_assertions，普通包没有启用合成协议。

固定 `FrdSynthetic` 日志标注 `synthetic=1 network=0`，只输出帧计数、局部代际、就绪门和合成输入坐标，不输出通用配置或密码。测试生成 320×200 四象限全帧，通过正常 runtime 编译和 compositor；没有脚本设置 ready 的入口。

| 检查 | 手机 | PC |
| --- | --- | --- |
| 首帧 | applied=1 时仍 ready=0；presented=1 后 ready=1 | 相同顺序 |
| 原始窗口点击映射 | 屏幕(400,1400) → 远端(101,75)，恰好两个事件 | 屏幕(1500,1000) → 远端(98,86)，恰好两个事件 |
| 留黑区域输入 | (600,800) 不增加输入计数 | 未单独测该项 |
| 本地尺寸变化 | 竖屏→横屏撤销 ready，实际第二次绘制后恢复；(1000,600) → (69,83) | 最大化撤销 ready，实际第二次绘制后恢复；(1560,1000) → (159,96) |
| 后台/前台 | 回到桌面时 ready=0 并 ReleaseAll；点图标恢复，同 PID 重新 present 后 ready=1 | 最小化时 ready=0 并 ReleaseAll；恢复同 PID 重新 present 后 ready=1 |
| 无后续远程帧恢复 | applied 保持1，presented 随本地恢复递增 | 相同 |
| 断开 | 回到表单并关闭 ready/原生内容组件 | 相同 |
| 新进程 | 重新启动从 applied/presented/ready=0 开始，再经真实首帧确认就绪 | 相同 |

官方 Emulator CLI 用于截图、点击、旋转和返回桌面，HDC 用于正常安装/启动及固定标签日志。旋转后 CLI 的坐标校验仍使用自然方向宽高，而输入作用于实际方向；测试点选择在两者交集，不能把工具越界行为误判成产品输入错误。

原始截图、签名包和日志在忽略目录 `target/task4b1-artifacts/`。手机 v3 lifecycle、两台 fresh-process 日志保存；PC 早期 v3 运行记录在直接工具输出中，随后日志缓冲已清出，晚导出为空，不声称本地文件含全部 PC 历程。v2 截图像素位置也已独立核对；v3 横屏/最大化截图分别为 `screenshots/v3-phone-landscape/`、`screenshots/v3-pc-max/` 下的 `Screenshot_2026-10-13T174806.png`。

## 尚未完成的验收

最终 v4 包含断开清理和控制器就绪交叉检查，复验结果如下；审查后的 v5 证据见末节。完整键盘遍历、远程 IME、实体鼠标与滚轮、双指平移缩放、真机多显示器及真实网络会话未由本轮 CLI 测试证明。手机和 PC 的普通客户端仍未注册真实协议；这些合成输入计数不等于已控制 Windows。

尝试手动重连时，CLI 填写合成主机名触发系统输入法首次使用界面并将句点改为中文标点。测试取消、退出该输入法界面，没有授予联网、通讯录或麦克风权限；同进程表单重连未完成，不能用新进程首帧代替。未使用或读取真实登录密码。

## v4 与普通包恢复

产品代码提交 `87c829a`。最终 v4 unsigned SHA-256 `129fe664a34331feb5d0865f350a72cebdefc033932dd92aaadbe54b861c1d4c`，signed `5cdbdd16f63961c8dbaf7cae9b32c81486d5341fe014c48e8040a2ecff5783df`。17:58 两台均从零就绪开始，只有 applied=1、presented=1 后 ready=1；各一次点击恰好两个事件。断开后 ready=0、ReleaseAll=3、lifecycle 递增，官方 CLI 确认回到表单，没有继续显示合成桌面。`fixture-v4-{phone,pc}-first.log`、`fixture-v4-{phone,pc}-disconnect.log` 均已保存。

随后在两台恢复普通 debug 包：unsigned `047d23db8b5991d76546149138c3627f9a70c9ffed3e02dae446f47a119b389f`，signed `363b2eb14c00561f80430c7183acea0c21bb0484ad2cddf4fbe83647e74e9ddb`。17:59 两台官方 CLI 均确认登录表单和“此设备尚无可用的远程连接协议”，没有合成自动连接。手机恢复竖屏、PC 恢复最大化。普通 release unsigned 为 `fef371bf02ebb551ea3976df5db187517fb06dc779823a5ec7c2a44d6685eb26`，已构建但未作为本轮安装包。

相关核心、render-state/wgpu/compositor、OHOS FFI 与 desktop consumers 的 scoped 测试共 290 passed、0 failed、1 intentional ignored；最后断开与输入门调整另跑 FFI 30 passed。含实际宿主 Metal 像素回读。固定 Rust 1.96 的 fmt、diff-check，最终普通 debug/release 与 fixture HAP 构建均通过。这些结果不替代真实协议互操作；后续修复的独立复审见末节。

## 独立审查与修复复验

冻结差异 `87af6f4..87c829a` 的独立审查发现：已经呈现过同一 epoch 的画面后，新增量应用成功，但 compositor 因表面恢复或暂时获取失败返回 `Ok(None)` 时，没有新的远程事务便不会重试。这可能让远端最后一帧一直停留在 GPU 纹理中。该问题已修复；v3/v4 的成功首帧和前后台检查不覆盖这条确定性分支。

另发现共享绘制路径每帧重建顶点 buffer 的开销。`bccb321` 保留独立待呈现状态，并缓存最近几何的不可变 buffer；普通增量重试保持 `repaint=false`，不会被升级为完整基线。复审确认两项问题均已解决，没有新的 Critical/Important 问题。


修复后的 FFI/render/compositor/desktop 相关测试 267 passed、0 failed、1 intentional ignored；扩展 Metal 像素回读另跑 1 passed。确定性回归覆盖完整首帧成功→增量应用→一次 `Ok(None)`→没有新事务仍重试成功，以及 partial-first/full-later 分支。实际设备未强制注入获取表面失败，不能将尺寸变化冒充该分支的设备证明。

v5 unsigned SHA-256 `cf2fbd1baa1507dda5dffcc1b395e6216b69573b7f4b3e8eb18f6c98d26515d6`，signed `6b207483f7ff1ecd788d5b413ea118e78d53e5745dcc413bfdd36dd66acc91b0`，正常产品检查、官方签名和验证均通过。18:09–18:10 在两台正常安装、启动后，都观察到 applied=1/presented=0/ready=0，随后实际 presented=1/ready=1。手机点击(400,1400)→(101,75)，PC点击(1500,1000)→(99,87)，各恰好两个事件。手机横屏和 PC 最大化均关闭 ready、ReleaseAll 后，在 applied 仍为1时完成第二次实际呈现并恢复 ready。两份 18:10:32 截图已目视核对，四象限和留黑边界正确。日志为 `fixture-v5-{phone,pc}-{first,resize}.log`。

18:11 两台恢复普通 fix1 debug 包并经 CLI 确认登录表单、空协议提示；手机竖屏、PC 最大化。普通 fix1 unsigned SHA-256 `0ffba686815fc407cc20b131c02643dd49df92f8ffb1397e6cec49d879ef267e`，signed `9a8a5549bb209442fbd7fe2559e1902852e5ad66cff73f924fa81129fed5889f`。普通 release unsigned `5a0a746669d8b5dd86eca39111dc2e8016b8e37e5fbfdb5a9dd17490253dc03f` 已构建，未安装。代码呈现子任务完成，真实协议/解码/认证及完整输入验收仍未完成。
