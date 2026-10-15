# 鸿蒙手机登录后横屏与自动旋转验证（2026-10-15）

用户明确纠正：**登录页默认竖屏，远程登录成功以后默认横屏，随后支持自动旋转。** 本节为当前行为；后半保留的初版“创建窗口即横屏”仅是历史产物/测试记录，已被此策略取代。当前状态为 **受限验证**，仅 API26 ARM64 手机模拟器，不代表真机或 PC GUI。

## 当前行为与平台边界

方向仅由 Harmony 手机 Ability shell 管理。登录页/新窗口默认 `USER_ROTATION_PORTRAIT`；只有公共 snapshot 的 page3 且非空当前 session 触发 `USER_ROTATION_LANDSCAPE`。既有 controller 只有在匹配 session/generation 的 `FullBaseline` 已呈现后才进入该页，连接中和等待首帧不提前转横屏。返回登录页恢复竖屏。

同一会话去重；前后台、resize、100ms轮询不重复强制方向。两个 USER_ROTATION_* 枚举按官方 API 暂时选择默认方向，随后跟随传感器并尊重系统旋转开关。同窗口请求串行并合并最新状态；窗口代际丢弃旧完成。能力探测、同步异常和异步拒绝独立处理，不阻断加载、不反复重试。会话标识只用于内存去重，不记录到日志/资料。PC/2in1不注册方向事件、不调用方向 API；协议、解码、Rust、manifest与其他平台不改。

保留此前响应式表单，另将手机有模式选择时的端口列从72vp改96vp；不改变PC原宽度、字体、padding、密码树或48vp触控高度。此前65535模型正确但首位视觉裁切已明确记录，不能将旧布局快照当完整显示证明。

已复核官方 [Apple HIG布局](https://developer.apple.com/design/human-interface-guidelines/layout)、[Material自适应布局](https://m3.material.io/foundations/layout/canonical-examples/overview)及本地API26 SDK方向枚举。Apple官方markdown可读；M3当前页面需要JavaScript，沿用先前已审阅的官方自适应与字段规则，本轮没有新增GUI基线例外。

## 当前包与实际验证

独立审查 Spec PASS / Quality PASS；root再次运行11项纯策略用例全部通过，覆盖成功门控/去重、连接等待、回登录、重连、异步合并/失败、窗口重建和旧回调。它们不是设备结果。

root实际执行普通Release双协议构建，Rust编译和ArkTS编译/打包均退出0（Hvigor33tasks：22executed/11up-to-date），脚本精确SDK strip与包内FFI检查通过。已复用既有授权开发身份签名、官方验签及签后checker，原28条目逐字不变，官方signer只增.pages.info。只对手机覆盖安装/启动，各有成功返回，保留数据/HUKS，PC未操作。

| 对象 | SHA256 |
|---|---|
| EntryAbility | `35c5a71062974b4679504e42b4c63e7d2c17b1ce1f818a96e4647be2448f2f0f` |
| Index | `7d058139378c4114a2f46bc7ae9b851ad2cb711d59fe17d274d3c2058aa0c4a4` |
| PhoneOrientationPolicy | `6e293ede16bbca0445374974aa99b359db9de5741f37042b34199c558bbc9ba6` |
| unsigned HAP（22,497,436 bytes） | `fb37f84353ca84a28c9e9d951788e589eb1df16c3d1303c227fb1b4aeb72c9d3` |
| signed HAP（22,754,120 bytes） | `539cf8eea5ec08d220703064a994f0f0be6f1a5d776c6b3a96dd38e406b6c75f` |
| 包内 FFI（与前一模式包逐字不变） | `206c603f22c1af977f0db96e610d4d9bda10f089b0fe0de349c33e154d6c47e5` |
| ArkTS ABC | `6c500aeb30a65651a4479df52210e2097029d1b4c4a9a5995b71eff9f85b44b0` |

官方Emulator CLI实际观察：

1. 启动手机普通应用，登录页竖屏，全部主要字段/按钮可见。选原Windows连接，HUKS载入密码，无密码命令行输入。
2. 实际点击连接后14:09:39仍是竖屏“取消”阶段；14:09:50显示真实Windows桌面并自动转为横屏。两次观察之间没有CLI旋转操作。
3. 转到物理横屏后再转竖屏，14:10:12与14:10:40仍竖屏且连接继续；轮询没有将其强制转回。再转横屏，仍显示已连接。
4. 主动断开后，物理姿态仍横屏，应用登录页已恢复竖屏内容布局；再转物理竖屏，表单正常。
5. 使用公开五位端口59050检查有模式选择的表单；竖屏及横屏截图完整显示五位，无首位裁切。横屏标准MVS、其余字段、连接按钮与能力提示同页可见，未向该测试端口提交连接或保存测试资料。

实际证据 `target/phone-session-orientation-20261015/`：source-before-build、build-invocation/result/log、package/signed-identity、sign.redacted.log、runtime stop/install/start及login-default/post-connect/connected-default/connected-landscape/connected-portrait/return-login/five-digit-*截图和布局。模拟器物理姿态与应用方向可不同；自动横屏时设备仍竖放，所以原始截图看起来侧转，内容矩形与布局证明方向转换，未旋转/编辑证据图片。

本轮实际浅色普通字号、已保存Windows登录、成功后横屏、后续横竖旋转与回登录通过。系统旋转锁切换、深色/最大字号、完整前后台/IME/触控回归和真机未重新验收，PC只有源码隔离与共同HAP构建，不将其称PC运行通过。新浮动岛需求是后续独立变更，当前证据中的顶部详情/断开仍为旧会话chrome。

---

# 初版窗口创建即横屏的历史验证（2026-10-15）

本轮按用户要求实现手机首次横屏、随后自动旋转，并修正横屏登录表单。当前结论为 **受限验证**：普通 Release 验证包在 API26 ARM64 手机模拟器上完成方向、表单状态与真实 Windows 会话旋转/触控验证。模拟器不代表真机；PC 本轮没有重新安装或执行新包。

## 行为与隔离

- `EntryAbility.ets` 仅在 `deviceInfo.deviceType === 'phone'` 的主窗口创建时请求 `USER_ROTATION_LANDSCAPE`，不在 foreground 或 resize 时重复强制横屏。该官方 API 的语义为首次横屏，随后跟随传感器及系统旋转开关；本轮没有实际切换系统旋转锁，因此锁定交互仅有 API/源码依据。
- 方向请求的能力缺失、同步异常与异步拒绝独立记录，既有页面加载继续。共享 manifest 保持原 `auto_rotation_restricted`；PC/2in1 不进入手机策略。
- 手机内容区宽 ≥600vp 且高 <500vp 时，登录页使用两行双列字段，标题/最近连接和保存/连接各自并排。普通字号下所有主要字段及安全存储提示均完整显示。竖屏与 PC 保留原单列布局。
- 同一字段树只调整宽度与换行；安全密码输入、提交、IME 处理、协议、解码和输入映射逻辑未改。原 48vp 控件高度、安全区和 Scroll 保留。

设计已核对[华为横竖屏指南](https://developer.huawei.com/consumer/cn/doc/best-practices/bpta-landscape-and-portrait-development)、[OpenHarmony 官方方向枚举](https://raw.githubusercontent.com/openharmony/docs/master/zh-cn/application-dev/reference/apis-arkui/arkts-apis-window-e.md)、[Apple HIG Layout](https://developer.apple.com/design/human-interface-guidelines/layout)与[Material 官方自适应布局](https://github.com/material-components/material-components-android/blob/master/docs/adaptive/CanonicalLayouts.md)。没有新增 GUI 基线例外。

## 构建、签名及身份

实际执行 `tools/build-harmony-hap.sh --release --normal-hap-rdp-validation`，沿用已接收 FFmpeg bundle/manifest。最终构建 UTC 04:38:39–04:38:45 退出 0；Rust FFI 复用缓存，ArkTS 编译与 PackageHap 本次执行，33 tasks 中 22 executed、11 up-to-date。没有将缓存命中表述为 native 全量重编译。

| 对象 | SHA256 |
|---|---|
| EntryAbility 源码 | `a60a630df690aac1f069a25597c06854794a062ed3870fc0ef20c80b73d80b9d` |
| Index 源码 | `4b6ee564b58424d8a07d0baa9288134fc2f4ae3eff13ea7ef0d683d2308b1cc4` |
| 最终无签名 HAP（20,923,797 bytes） | `7d2f1a7d6dc228563b3de4e038ca2258af7aaa31df431eb439edf93d492d71b8` |
| 最终安装签名 HAP（21,164,092 bytes） | `543e0060429a2788c85db4d85309c491b1b06d07b53d1a8442911d8d23112e79` |
| 包内 FFI（保持原身份） | `37846a22b556bc51dd48eb04f06817987c3cdebe1ec3073787912660f365e9d7` |
| 包内 ArkTS ABC | `faae5c7806febfd81aae045e18802eb738674d8b243022ec3c4d91d725542495` |

复用既有授权测试签名身份，官方 signer/verify-app 与 native 字节检查通过；无新安全配置修改。最终 HAP 经官方 HDC 覆盖安装到手机，stop/install/start 各自宿主退出 0 且返回对应成功信息。未清数据，保留原 HUKS 凭据。RDP 仍为默认关闭的显式普通应用验证组合；本项不提升视频 codec 或生产互操作资格。

方向和表单两个子步骤均经过独立源码审查，结论各为 Spec PASS / Quality PASS。最终源码摘要已由主代理再次核对；没有为低影响布局添加镜像实现测试。

## 实际手机观察

所有旋转、点击、截图及布局提取均使用用户授权的官方 Emulator CLI；仅操作 `FreeRemoteDesk Phone API26`，未操作 PC 密码输入。

1. 冷启动应用时模拟器物理姿态仍为竖屏，但应用主内容已按横屏 2652×1158 布局；首次旧单列表单确实超出首屏，因此另行修复、审查和重建横屏布局，再验证最终包。
2. 最终包物理横屏截图为 2760×1256。普通字号下目标、端口、地址、用户名、密码、HUKS 提示、保存与连接按钮同页显示，最下方按钮底边 1099 < 内容高度 1158；左侧挖孔安全区得到保留。
3. 输入公开的非秘密夹具 `rotation-check`，在安全键盘打开时自动转回竖屏，字段和 14 字符安全输入保留；收起键盘后再转横屏仍保留。两次左转至另一横屏，安全区随挖孔移到另一侧，字段及夹具仍保留。
4. 回到正常横屏后重新选择已有连接，清除公开夹具并从 HUKS 载入原密码；实际点击连接成功显示 Windows 桌面。夹具未提交、未保存；真实密码没有通过命令行、普通文件或 UI 文本快照传递。
5. 横屏触控远端开始菜单后切竖屏，画面显示开始菜单已打开且会话仍连接；竖屏再次触控开始菜单，延后截图确认菜单关闭。再转横屏仍显示已连接。该结果证明本次旋转前后真实会话画面与触控的有限行为，不代表完整键鼠/IME验收。
6. 最后主动断开，回到正常横屏登录表单。

原始证据根目录（本地、不入 Git）：`target/phone-orientation-20261015/`。最终构建/包/运行证据位于其 `layout/`：

- `build-command.json`、`build.log`、`identity-before.json`、`identity-after.json`、`source-boundary-review.json`、`sign.redacted.log`。
- `runtime/{stop,install,start}.json` 与 `default-layout.md`、`landscape-huks-layout.md`、`portrait-dummy-layout.md`、`return-landscape-dummy-layout.md`、`reverse-dummy-layout.md`、`restored-huks-layout.md`。
- `runtime/landscape/Screenshot_2026-10-15T124218.png`：最终完整横屏表单。
- `runtime/remote-landscape/Screenshot_2026-10-15T124406.png`：真实 Windows 桌面。
- `runtime/remote-portrait/Screenshot_2026-10-15T124439.png`：旋转后菜单打开。
- `runtime/remote-portrait-input/Screenshot_2026-10-15T124515.png`：竖屏触控后菜单关闭。
- `runtime/remote-return-landscape/`：回到横屏仍连接。

## 未完成的验证

本轮实际浅色/普通字号、横竖与双向横屏、挖孔、安全键盘编辑状态和真实 Windows 有限触控通过。深色、最大字号、系统旋转锁切换、完整键盘遍历/IME、完整后台生命周期及真机尚未重新验收；源码保留既有主题/字号/滚动能力不能替代这些运行结果。尝试的 Home/控制中心滑动没有造成可观察的页面转换，不将 CLI 成功返回当作后台或设置验证。

Windows/macOS/Linux 壳未修改；PC 仅有源码条件隔离与公共处理路径保留证据，本轮没有新包 PC GUI 验收。PC 真实认证仍按原计划等待安全输入后单独验证。Apple MVS 另一个在途改动与本次手机提交分离；此记录不代表鸿蒙连接 Mac 已完成。
