# 鸿蒙普通应用标准 MVS 组合与真实 FFI（2026-10-15）

状态：**实验性**，本轮仅完成源码、宿主检查和官方 OHOS ARM64 最终 FFI 交叉链接。没有构建/签装本次组合 HAP；OHOS 正向用例仅编译，手机/PC 普通应用的 Mac 登录、帧、输入与重连仍未验证。

后续模式选择与同组合真实 Release HAP 已另行完成源码/构建检查，已开发签名核对、尚未安装或完成手机/PC GUI 与真实 Mac 会话；见[后续记录](harmony-mode-selection-20261015.md)。以下保留组合根阶段的历史产物与验证范围。

## 行为与平台边界

新增默认关闭的 `normal-hap-apple-mvs-validation`；Apple 可选依赖及类型引用均限 OHOS ARM64，选择既有 `mvs-native-arm64`（IDCT/colors/copy）。与 `normal-hap-rdp-validation` 可同时启用，真实组合根累加工厂，保留 RDP Unix/RequiredNative/LegacyOnly。默认无普通 feature 仍不注册协议。

标准模式复用 `AppleProtocolFactory`、原 C ABI、AppEngine/Stores/SessionHost。没有修改共享 catalog：Mac Automatic 仍选未注册的 HP，必须 Explicit `apple-hpss-mvs` 才能选择标准；错误目标拒绝。当前正常表单尚没有模式选择控件，因此不能把工厂代码接入说成用户已经能连接 Mac。

video loader/NoAudio 继续不可用，当前键盘/committed Text、焦点/前台/geometry/epoch 门保持。没有修改 Apple 协议、44个已资格计算/状态输入或 Windows/macOS/Linux 壳；其他架构不会通过这个开关获得标量 Apple 注册。未恢复 macOS Intel。

## 构建与包合同

两个 normal flags 与全部五种 synthetic/codec fixture/profile/benchmark 在 Debug/Release 中互斥；两个 normal flags 之间可共开。build、HAP、check、sign 入口明确传递两个参数。任一普通验证构建都执行仓库 target-dir 限制、原 FFmpeg bundle 检查和“本次源 FFI经所选 SDK strip派生后与包内字节相等”的原有检查。signer 的交互取密与官方验签路径未改，本轮未执行签名。

实际两份最终 ELF 的依赖不同，按明确参数精确校验：

| 构建 | FFI DT_NEEDED |
|---|---|
| 原默认/RDP-only | 原 base 五项，保留原唯一 AbilityKit 可选项 |
| Apple-only | 精确 base + `libc++_shared.so` |
| Apple+RDP | 精确 base + `libc++_shared.so` + `libz.so` |

base 是 `libnative_window.so`、`libhilog_ndk.z.so`、`libhuks_ndk.z.so`、`libohcrypto.so`、`libc.so`。zlib 来自 RDP 与 Apple 的 flate2 feature统一后实际使用。缺项、多项、错配配置均拒绝；没有全局放宽 PUBLIC/任意子集，也没有新增 AAC shared库。SDK正式 libz SONAME/头文件及七个实际 undefined 符号的公开导出已核对，不代表目标运行库映射或加载通过。

两份FDK rawfile `NOTICE`/`SOURCE.json` 保存完整上游许可及固定 crate来源/checksum，Apple 合同要求精确集合/字节；其它包若包含也核对，签后比较覆盖材料增加/删除/修改。44个 Apple 资格源保持。423个FDK源文件与锁定缓存crate逐字相等，归档checksum匹配Cargo.lock，未将本机路径或秘密写入来源材料。

## 实际产物

使用 coherent Rust/Cargo/Rustdoc 1.96、官方 API26 SDK clang/clang++/ar/sysroot，有限实际命令包含两旗直接构建、正式 `tools/build-harmony.sh --build --release`、Apple-only构建及恢复两旗构建；最终locked/offline均退出0，缓存命中如实记录。首次为更新一条本地lock依赖边未使用locked，未更新依赖版本。

| 最终文件 | bytes | SHA256 |
|---|---:|---|
| BOTH FFI | 23917552 | `df0ead22c56af6da87962a9727c2705119089fea1554332e0cceb868450e78b8` |
| Apple-only FFI | 11494288 | `60ba5691b560af70f9589f9e4c1afff2bbf58a52d857261d97aa059100808ac7` |
| 最终FDK静态archive | 2426828 | `fef9d3e5bb7beebee7f87f67402ac4d777863f793e7a4d9b38523d7571ff6276` |
| 已编译未执行的composition libtest | 25155200 | `488b36b234e54a6364ce84b1b6c90d04b0f2e9314eba921517ceb511c7ebc57b` |

两个FFI均为小端AArch64，SONAME正确，23个既有C ABI导出保持。实际最终BOTH含静态AAC符号和七个MVS原生叶的真实调用/指令；不是只看feature或旧libtest。FDK171个对象全AArch64。首次BOTH `90b75f…`及旧FDK `ce268b…`为历史身份，wrapper/直接构建环境切换后原生依赖重编；当前交付采用上表最终身份，未覆盖旧5A5证据。

## 验证与审查

- 宿主四配置各5项composition通过；最终BOTH完整FFI为36通过、1既有child-only ignored、doc0。实际C ABI编辑/快照/销毁覆盖idle、显式/自动/错误目标及媒体不可用；宿主不能正向证明OHOS注册。
- 20条真实互斥Cargo check均因指定compile_error退出101；不是其它编译失败。
- 最终参数/精确合同7项通过，包含签前/签后flags、target-dir和strip错误。既有普通HAP checker回归40通过/2个recipe缺失skip，另两项FDK签后/材料变异通过；测试样本是旧普通HAP，不是本次组合包。
- root在72个源冻结后重新实际执行完整FFI（36通过/1ignored）及7项脚本合同检查，均exit0，并重新核对全部源摘要、两份实际ELF精确依赖、FDK171对象和20条原始诊断。
- 独立Spec PASS/Quality PASS，无actionable finding；独立重新核验源/ELF/叶函数调用/FDK来源/原始测试证据。首次missing flag/错误组合RED、Bash3空数组回归及修正日志保留，未以改宽合同取得通过。

本地原始证据：`target/task5b1-standard-composition-20261015/`；root冻结后验证与接收：`target/task5b1-root-20261015/`。生成物不入Git。上游NOTICE原文自带末尾空行；首次暂存diff检查因此拒绝。保留许可字节，通过精确路径gitattributes固定LF并仅豁免其blank-at-eof，随后普通diff检查通过，不全局放宽检查。当前host Mac锁屏且phone实例已停止等待解锁恢复，未把该状态计为客户端失败/互操作结果，也未操作PC密码输入。

组合根阶段规划的正常模式选择、精确profile协议键/目标切换、真实HAP构建与开发签名已由[后续记录](harmony-mode-selection-20261015.md)补充；手机/PC新包安装、界面和真实Mac验证仍未完成。完整键盘/IME能力分离、系统/后备媒体、AAC-ELD输出、HP、MVS性能/生产资格仍按计划独立完成，见[实施计划](../superpowers/plans/2026-10-13-harmony-clients.md)。
