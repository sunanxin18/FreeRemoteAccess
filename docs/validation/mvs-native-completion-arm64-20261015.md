# Apple MVS ARM64 输出映射与位图行计算

2026-10-15，状态 **实验性**。在既有默认关闭的 `mvs-native-idct-arm64`、
`mvs-native-colors-arm64` 及聚合 `mvs-native-arm64` 下补全两个计算路径。
默认桌面选择、协议广告和普通鸿蒙应用的 Apple 注册状态保持原样。

## 实现范围

IDCT 两个 i64 输出先由 NEON 掩码取低10位，再按原 Apple 表比较、选择和窄化。
0..127→index+128、128..383→255、384..895→0、896..1023→index−896；
负数及 i64 极值也遵循同一环绕语义。原 scalar oracle、蝶形、反量化和舍入不变。
输出字节的散写仅处理布局，没有逐 lane 标量范围变换。

mode3/mode4 共享的双色位图及纯色 fill 使用一个8像素RGB行内核，0x80为最左
像素，三个颜色分量用向量位测试/选择后交错写入24字节固定scratch。裁剪行仍
执行同一内核再复制有效前缀。wire/BitReader仍读取全部八行所需掩码，包括不可见
行，保留短读错误链与 staging 前缀。无新分配，提交/缓存/代际/发布边界不变。

编译门仍要求小端 ARM64 和 NEON 基线，IDCT 原运行期 NEON 检查保持；条件不
满足时显式拒绝该后端，不静默回退。其他架构保持原选择，此项不增加其原生资格。
macOS 产品只支持 ARM64。

## 宿主与实际产物

使用绝对路径 Rust/Cargo/Rustdoc1.96，`--locked --offline`。先用未实现叶函数的
零输出取得真实断言RED（exit101、3项数值失败），随后在冻结源码执行：

| 验证 | 实际结果 |
|---|---|
| 新聚焦Release，default / aggregate | 5 / 6 passed，0 failed |
| 完整Apple包，default | lib527、auth4、session1通过；9项既有ignored |
| 完整Apple包，aggregate | lib531、auth4、session1通过；9项既有ignored |
| 指定pipeline Release，每种配置 | 各3passed，38条/70,802字节完整值及顺序相等 |
| 六个改动源文件Rustfmt、diff检查 | 实际exit0 |
| 官方OHOS aggregate Release libtest | 最终可执行ELF链接成功，非check/rlib |

新覆盖含8192个range值、14个边界固定答案、全部256个bitmap masks及反向两色、
13056个raw leaf非对齐/canary/颜色案例、512个裁剪bitmap布局、128个solid布局和
9个短读案例。既有非对称mode3/4和完整记录/状态/发布fixture期望保持。

最终OHOS ELF为7,145,096字节，SHA-256：
`cb744d7476b6a97c9fd08c3450365c657cc1aa3d57061badb4c79194a286239a`。
实际ELF64小端AArch64 PIE，解释器`/lib/ld-musl-aarch64.so.1`，直接依赖官方
`libc++_shared.so`和`libc.so`；实际链接既有FDK静态库，没有stub或依赖变更。
SDK C++为1,262,504字节，SHA-256：
`f69947af100b652e349ae51e4a4888bcc7a327db9085272d318176ef1782bf0f`。

实际反汇编确认bitmap行内核23条指令，含向量AND/CMEQ、3×BSL和ST3；fill及
bitmap消费者均调用该叶。IDCT尾部是向量AND/CMHI/BSL/BIF/XTN等，没有逐lane
标量范围选择或scalar helper调用。LLVM以向量UQSUB合并低段比较/减法/零选择，
完整结果仍按原环绕表验证；不是把表替换为饱和clip。暂存库哈希不是mapped库身份。

## 目标执行与限制

源码、实际目标ELF及代码生成经独立Spec/Quality审查通过。随后同一最终ELF在
手机、PC两个专用API26 ARM64实例分别实际执行新聚焦6项及完整记录pipeline3项。
四条测试命令共18次执行全部通过，无失败、忽略或重试；实际HDC宿主退出及每条
独有guest退出均为0。每端38条/70,802字节的完整值及顺序与当前冻结宿主default、
aggregate两份输出相等，手机与PC之间也逐字相等。

root独立解析20条实际HDC命令、16个guest标记及四份测试raw输出，复算79份运行
证据和43份当前源码身份，核对两端上传后/执行后ELF及SDK C++哈希。记录位于
`target/task5a4-root-20261015/actual-target-test-acceptance.json`。仅向新私有目录
暂存两个文件，使用命令局部库搜索路径；未修改系统设置、应用或权限。CLI结果
不证明普通HAP沙盒、mapped库身份或真实会话。

两个raw tile copy、overlay扫描顺序、cache/save-Y/row copy、staging和surface
拷贝等保持原样；未认定全部计算热路径已取得SIMD资格。没有本轮性能测量、
普通HAP Apple接线、真实Mac认证/首帧/刷新/输入/重连或生产注册结论。

原始证据位于`target/task5a4-mvs-native-completion-20261015/`，root复核位于
`target/task5a4-root-20261015/`。这批代码属于[鸿蒙实施计划](../superpowers/plans/2026-10-13-harmony-clients.md)的计算子步骤。
