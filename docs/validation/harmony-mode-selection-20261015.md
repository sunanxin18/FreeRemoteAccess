# 鸿蒙普通表单连接模式与双协议 HAP（2026-10-15）

状态：**实验性**。正常登录表单已接入显式标准 MVS 模式，完成宿主回归和官方工具链双协议 Release HAP 构建。最终无签名包尚未签装；手机/PC 实际模式菜单、横屏同页布局和真实 Mac 会话仍未验证。手机横屏策略及此前 RDP 旋转验证见[方向记录](harmony-phone-orientation-20261015.md)，不能将旧包观察当作本包 GUI 验收。

## 行为

- macOS 目标显示持久标签“连接模式”；显式选择“标准（MVS）”沿原表单编辑路径选择 `apple-hpss-mvs`。自动模式保留原 HP 选择语义，未注册时明确不可用，不偷偷回退。
- 选项的可连接状态和允许目标来自当前真实 catalog；已保存但不可用、目标不匹配或未知的显式模式仍可见，并有固定安全说明。可用标准模式明确提示当前仅画面/指针范围，键盘、文本、音频、剪贴板没有因此开放。
- 最近连接显示目标和模式，以区分相同地址/账号；仍按原 index 和精确 profile key 恢复。实际目标变化仅重置不兼容模式，并复用原凭据身份失效和迟到票据保护；相同目标事件、轮询和旋转不触发重置。
- 模式列放入原目标/端口组，保留 48vp 控件、原短横屏双列、竖屏/PC 单列、安全区和滚动。原安全密码、提交、远端键盘、远端 surface 函数与基线字节一致。新布局的完整可见性仍需实测。

协议、解码器、组合根、输入/几何、存储 schema、C ABI 版本和 EntryAbility 均未改；Windows/macOS/Linux 壳未改。默认验证 feature 仍关闭，macOS 产品仍仅 ARM64。

## 本次构建身份

官方 wrapper 实际执行两个普通验证参数，沿用已接收 FFmpeg bundle/manifest。Rust 1.96 + API26 SDK 构建退出 0，Rust Release 45.77 秒；Hvigor 33 项中 22 执行、11 缓存，CompileArkTS/PackageHap 本次执行。不是 clean build。

| 对象 | bytes | SHA256 |
|---|---:|---|
| 冻结无签名 HAP | 22,493,764 | `6acbf69671d8b77bffb5a217bf025eb1d52caf2c20de4f5413d7c4289e6c462a` |
| 本次源 FFI | 23,930,168 | `3d4964e46f2cb4d367d2aa07367c806d5ec3579825ee822fd526d537995d3a8d` |
| 包内 FFI / SDK strip 派生 | 17,170,072 | `206c603f22c1af977f0db96e610d4d9bda10f089b0fe0de349c33e154d6c47e5` |
| 本次 FDK 静态归档 | 2,426,828 | `bf2db46bc49f679a7975ae6353bb91bbc9ecfc57e21953b3c64852ed72bc0ae4` |

FFI 为 AArch64，包内文件与本次源 FFI 经实际 SDK strip 后逐字相同；精确两个 flag 的 HAP checker 通过。FDK 171 个对象均 AArch64，423 个源文件与锁定 crate 逐字相同，包内 NOTICE/SOURCE 与已接收材料一致。静态 AAC 符号不表示本应用音频已开放或运行成功。

## 检查与边界

六项模式回归获得真实 RED 后通过，覆盖实际 owner、catalog、精确 profile/迟到凭据、目标兼容、原 RDP 提交及 C ABI idle snapshot。最终完整 FFI 为 **42 通过、1 项既有子进程专用 ignored**；root 在冻结后再次运行相同完整命令，结果相同。宿主实际 catalog 没有 OHOS 工厂，正向策略 fixture 不冒充目标运行。

root 重新核对 78 个当前源码输入、构建前后摘要、四份工件与包内 FFI。原密码/提交/编辑/远端按键/surface 函数与 Git 基线逐字相同。首轮测试析构二次 panic、实现借用编译错误及 host target-dir 误传导致 wrapper 拒绝的原始失败均保留；最终没有通过修改协议或放宽 checker 取得成功。

独立审查为 Spec PASS / Quality PASS，无需修正的源码问题。审查重新核对两产品文件、78 个源输入、实际六项 RED、完整回归及真正执行的 ArkTS/PackageHap 日志；结论仅覆盖源码与无签名包。特别保留三列模式组、72vp 五位端口、菜单/焦点及横竖屏/大字/深色的实际设备验收要求。

本地原始证据为 `target/task5b2-mode-ui-20261015/`，root 接收记录为 `target/task5b2-root-20261015/`；产物不入 Git。本次没有签名、安装、设备操作或服务器连接。host Mac 锁屏阻止新增界面检查，PC 手动安全密码输入仍按原流程等待，不覆盖或截取其界面。

下一步使用同一组合包完成签后身份及手机/PC 界面、stock Mac 认证/首帧/更新/指针/重连。完整物理键与文本能力分离、本地显示器 geometry、媒体/HP 和整管线性能保持独立未完成项，见[实施计划](../superpowers/plans/2026-10-13-harmony-clients.md)。
