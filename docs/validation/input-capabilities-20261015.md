# 物理键与提交文本能力分离（2026-10-15）

状态：**受限验证**。共享能力接口、确定性输入路由、独立审查与OHOS包构建通过；完整真实键盘与IME验收仍未完成。

## 问题与接口

旧SessionCapabilities、PlatformCapabilities与ProductPolicy只有text_input，AppController同时据此判断PhysicalKey与Text。Apple runtime的物理键有实际wire实现，但忽略Text却宣称text_input=true。因此不能通过全局关闭text_input修复声明：这还会禁止已经实现的物理键。

新增独立physical_key_input，分别对session/platform/product三层求交。PhysicalKey仅受物理能力门，Text仅受已提交文本能力门；pointer、ReleaseAll及焦点、完整基线、generation、后台隔离保持各自原门。Apple声明物理键可用、提交文本不可用；RDP保持两条已实现路径，重新激活时两者都暂不可用。桌面shell的物理分派和失去能力后的释放改用物理能力；文本/IME仍用text_input。

OHOS当前生产平台/产品键盘能力继续不可用；协议单独宣称支持不能提升本地未完成的能力。新接口是实际键盘和远程输入法入口的必要基础，不能将新字段、宿主测试或HAP构建当作手机/PC真实键盘验收。

## 验证边界

本轮覆盖共享能力与shell接线，没有修改协议wire、软件计算热路径或平台窗口；macOS仍仅ARM64。Windows/Linux本机cfg、完整真实远程键盘/IME、真机与生产功能广告均需独立证据。输入法登录表单数字修复仍见[鸿蒙输入分发回归](harmony-login-key-input-20261015.md)。

## 已执行的宿主验证

固定Rust1.96：App86、Platform2、Protocol API26、Apple538、RDP352、OHOS FFI52项unit通过，Apple另5项integration、Platform另1项compile-fail doc通过；23项既有ignored保持（Apple11/RDP11/OHOS1）。三桌面应用与desktop/GTK壳的host check通过，范围为macOS ARM64 cfg，不是Windows/Linux原生cfg或运行验收。

独立首审找到真实P1：text-only时physical预分派误走Ui撤销共享armed状态并吞指针ReleaseAll。已在真实InputRouter/IME classifier顺序取得两个行为RED；修复物理隔离账本、必要ReleaseAll返回发送路径、capabilitylost后Text保持及普通事件fallback门。修复后desktop完整171unit+2integration通过，0failed/ignored；格式与diff检查通过。初版源码/构建记录保留，不作为最终审查通过证据。

本地最终证据目录为 `target/harmony-input-capabilities-20261015/fix1/`；初版被拒审查、构建与签名记录在 `../initial-rejected/`。源文件14项SHA在本地manifest中，正式报告与独立审查存于当前计划的SDD目录。

最终独立复审Spec/Quality均PASS，原P1已修复。最终OHOS ARM64双协议普通Release构建、官方签名/验签/checker通过；14源码SHA不变、原ZIP条目签后逐字相等。unsigned SHA256 `3834a2973f7926b87b9d7007fff94bfe8f530c5d4a4d27ec7f062a793a13417d`；signed SHA256 `8e3c0f076c186d4b96c458e415ba0694130f0ba9683c7dbb588603cff886459d`。

## 最终手机有界互操作

官方HDC覆盖安装/启动成功并保留数据。2026-10-15 16:18，正常登录表单选择既有HUKS记录显示已载入密码并提交；16:18:30初始截图是Windows锁屏画面，不能算已出现桌面。后续原始截图显示完整PowerShell桌面与开始菜单，已观察到2760×1256全屏和右下收起浮动岛，期间发送一次开始位置触控；随后关闭菜单并显式断开。此结果只确认本次能力接口变更没有破坏保存凭据连接/画面/有界pointer，不证明OHOS物理键或Text可用。PC表单正在由用户填写，因此本轮未停止/安装/输入或接管PC；该端仍待单独验收。
