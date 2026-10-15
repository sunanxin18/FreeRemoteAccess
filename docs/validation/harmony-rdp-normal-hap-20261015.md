# 鸿蒙普通应用 RDP 验证包与本地运行

2026-10-15，状态为**实验性**。显式 `--normal-hap-rdp-validation` 的普通
Release HAP 已完成构建、开发签名、签后检查，并在独立手机和 PC 模拟器安装、
打开和显示完整登录表单。默认构建仍关闭该模式；**没有真实 Windows 认证、
远程首帧、持续刷新、远端输入或重连结果**。Mac factory 尚未注册。

## 源码与构建范围

真实 C ABI 经单一平台组合根将既有 Unix / RequiredNative / LegacyOnly RDP
factory 注入原 AppEngine/SessionHost。源码、宿主回归与官方 OHOS 最终链接
已通过独立 Spec/Quality 审查，见[组合源码验证](harmony-rdp-composition-20261015.md)。
此包没有合成会话、自动提交、开发页面、EGFX 广告或可用视频/音频后端。

固定 Rust1.96、官方 API26 SDK/Hvigor，显式使用仓库标准 `target` 路径。
输入为此前独立接收的 OHOS FFmpeg bundle，manifest SHA-256：
`44c641ed4f9cff820c520c069ba382327d2d5a24ee25d36143b5c78563b37f75`。
本轮另行执行静态 bundle 检查通过，三库与来源材料保持原字节。
包中存在 FFmpeg 库不表示生产 loader 或 codec 已启用。

首轮完整 builder 实际 exit1，原包 checker 通过，但新增 FFI 字节绑定拒绝。
根因是此前独立派生使用 `--strip-unneeded`；实际 Hvigor 插件使用：

```text
llvm-strip --keep-section=.permission SOURCE -oOUTPUT
```

错误派生物多保留 `.comment` 和 section 布局，共224字节。读取本机实际
`native-strip.js` 并按其命令派生后，与首轮包内库逐字节相同。只修正 builder
命令与聚焦测试，经独立复审通过；完整字节检查、唯一条目、原库只读和全部
既有包检查继续保留，没有允许两种结果或忽略任意 section 的比较。

修复后真实 builder 重放实际 exit0，当前源码保持冻结。原失败 HAP、日志、
错误派生物、插件原文及修复前后的回归都保留，未重标首轮为成功。

| 产物 | SHA-256 |
|---|---|
| 原始 Rust FFI，21,933,584 bytes | `f3e5eaf12d3c0d30577f56d929925fa726bca9250e34b4d91c86652450cf7279` |
| Hvigor 正确派生 / HAP 内 FFI，15,613,552 bytes | `37846a22b556bc51dd48eb04f06817987c3cdebe1ec3073787912660f365e9d7` |
| 最终未签 HAP，20,919,609 bytes | `fc53f2e0316d1ff611a50218ccb7828704aa42406835b4be8334164f5426f27b` |
| 最终开发签名 HAP | `2552a2248181c9a7ee06b480e53b7f5530d209291e6ee811d906cafac8a9129a` |

现有官方交互签名流程只通过非回显控制台提供口令；不将口令放入 argv、配置
或普通日志。复用此前已授权的本地模拟器开发身份，profile 为 debug/normal、
应用标识 `org.freeremotedesk.client`，有效期已核对。这不是生产发布签名。
官方验签、签后完整包检查和签前/签后相关条目字节比较均通过。权限仍精确
只有 `ohos.permission.INTERNET`，原生依赖集合没有扩张。

## 手机和 PC 分别运行

同一签名包覆盖安装到两个原有 API26 ARM64 专用实例，没有修改用户的 Pura
实例、清除应用数据、重建模拟器或改变系统权限/开发者设置。

| 范围 | 手机 | PC |
|---|---|---|
| 包管理器安装 | 实际成功 | 实际成功 |
| 正常应用运行 | PID9768，完整登录页 | PID32221，完整登录页和原生窗口控件 |
| 实际目标切换 | Windows无协议不可用提示；Mac显示不可用；最后恢复Windows/3389 | 同左 |
| 表单视觉检查 | 1256×2760，全部字段、保存选项、连接按钮在一页内 | 3120×2080，居中表单，原生窗口三键保留 |
| 开发验证页面 | 未出现 | 未出现 |

PC 首次 `aa start` 返回 screen-locked / 10106102，原始失败保留。随后官方
Emulator 截图显示实际桌面，通过桌面已有 FreeRemoteDesk 图标正常点击打开；
没有关闭开发者模式、绕过锁屏口令或重启设备。

UI 树由官方 Emulator CLI 获取，两张最终截图已实际查看。只操作目标选择，
没有填写真实凭据或点击连接。进程映射观察另遇 `/proc/<pid>/maps` 权限拒绝，
没有提权，也不宣称取得了运行时映射库的 hash；产物绑定来自已验签安装包的
完整字节检查。进程存在、界面运行与库映射观察是不同证据。

本轮没有重复此前的全部暗色、旋转、大字、键盘、安全区、HUKS及生命周期
矩阵，也不从旧合成验收外推真实会话结果。现有 UI 实现未在本轮改动。

## 待完成

当前授权 Windows 地址的 TCP3389 探测超时，尚未开始 TLS/认证。目标恢复
可达后，仍需在手机与 PC 分别完成实际密码认证、TOFU 首次/变化处理、完整
基线呈现、增量刷新、触控/键鼠/IME、密码保存读取及断开重连。
本机映射权限限制不替代这些行为验收，也不构成目标服务器失败原因。

目标性能、规范光标 alpha、其它架构、视频系统库/后备资格、生产选择以及
Mac 路径继续按完整计划推进。普通应用有实际 RDP 入口不等于已具备完成验收
的 Windows 远程能力。

原始产物、首次失败、重放、签名与运行资料在
`target/task4d2-rdp-normal-hap-20261015/`；修复材料在
`target/task4d1-fix1-rdp-strip-binding-20261015/`，均为本地忽略产物。
