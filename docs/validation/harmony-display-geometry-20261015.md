# 鸿蒙本地显示几何与连接请求（2026-10-15）

状态：**受限验证**（API26 ARM64 手机、授权 Windows 的有界 RDP 会话）。共享策略、OHOS 接线、普通 Release 包及独立复审通过，手机已记录真实本地快照、冻结投影及协议 surface；PC 实际会话和独立 wire 请求/ACK 证据仍未完成。

## 行为与边界

OHOS 从实际主窗口的 displayId 取得所属显示器物理像素和 densityPixels，分别记录窗口与 drawable 矩形。原生 px 不再乘 density，字体缩放不进入显示几何；缺失所属显示器时明确不可用。系统工作区必须完整处于显示器边界内，异常或不可用时明确使用 drawable 保守回退；合法跨屏窗口仍按所属显示器裁剪。版本化 local-display 消息独立于 renderer/input viewport，旧 ABI POD 布局保持。

Owner 在真正开始连接前通过同一 FIFO 冻结最新有效几何。默认 NativeDisplay 优先请求物理像素，不增加 2560×1440 上限。手机保持真实竖屏快照，同时以独立 Landscape 请求偏好投影长边；共享策略先投影，再执行 RDP u16/64MiB 或 Apple 已有约束。桌面默认 Preserve，Fixed、ServerManaged、WorkArea、WindowContent 的语义保持。连接前仍竖屏，完整首画面呈现后才进入横屏；窗口变化不会自行重连或绕过协议动态分辨率门。

`localDisplayEvidence`、`frozenDisplayIntent`、`acceptedSurface` 分别表示本地快照、未经协议约束的投影意图、协议 runtime 发布的 surface。后者来源固定为 `protocol_SurfaceGenerationChanged`，不是 canvas，也不能单独证明 wire ACK。普通 OHOS 包通过变化去重的 `FrdDisplay` hilog 输出这三项；日志不输出地址、用户名、配置记录、密码、完整应用快照或 ConnectRequest。

## 修复与主机证据

初版真实 Release 构建失败：EntryAbility 将 SDK Rect 直接用于消息 Rect，不符合 ArkTS 名义类型要求。独立首审另发现系统 workArea 部分越界被静默裁剪。初版源码、日志和报告完整保存在 `target/harmony-display-geometry-20261015/initial-failed/`。

Fix1 改为显式逐字段复制及比较 SDK 矩形；不依赖对象属性顺序或保存 SDK 对象别名。系统 workArea 独立严格校验，畸形输入不会污染 Owner 旧有效状态；ETS 保留 drawable 回退并标注 `invalid_work_area`。新增测试先复现失败后通过：Rust `local_display` 11 项、实际 EntryAbility 编译结果与公开 API mock 10 项；格式与 diff 检查通过。这些主机测试不等于设备运行。

首轮完整宿主测试为 App86、Core25、OHOS59通过，OHOS1项既有 ignored；另有 RDP 尺寸 helper2项、Apple native/standard/alignment各1项和 macOS ARM64 desktop check。Fix1 仅重跑发生变化的边界与日志测试，没有把旧完整结果表述为新设备验收。Windows/Linux 原生 cfg 与运行仍需各自检查，macOS 仅支持 ARM64。

最终14文件源码清单摘要为 `f7ebcd097499f6591c64578408bb3a239c2df98a280c7455d5ccede11cff76a1`。普通双协议 Release 的 Rust、Native、ArkTS 编译及精确包 checker 均成功，官方签名/验签通过，签名前31个 ZIP 条目逐字保持，构建后14项源码摘要不变。

- unsigned HAP：`367a415d50a11a40378a6d7290021421fcb7250e3a4e9609afc6171d5701fb1a`
- signed HAP：`d55a3b471c68999891524a7373edb73ea74b1b9c0fa2fb9643d8d31c6f3af55c`
- 本地原始证据：`target/harmony-display-geometry-20261015/fix1/`。

构建保留已有 scalar reference dead-code、Hvigor required source/config.h 与无内置 signingConfig 提示；随后独立官方签名已成功，不把 unsigned 提示当作已签名。未更改 decoder 广告或生产后端资格。

## 手机实际运行

独立 Spec/Quality Fix1 复审通过后，root 覆盖安装上述最终签名包，保留应用数据。17:03 普通登录表单的系统快照稳定为 physical 1256×2760、densityPixels=3.5、rotation=0，系统工作区有效；逻辑尺寸约358.857×788.571，没有再次乘3.5。启动时方向转换产生一组短暂窗口通知，随后 revision44稳定，没有持续空闲日志循环。

通过正常“最近连接”选取既有 HUKS 记录，界面确认密码已从系统安全存储载入；没有通过命令行输入密码。17:04 连接冻结 session1 的本地竖屏快照，mode仍为NativeDisplay、orientation=Landscape、projectedSize=2760×1256；协议runtime随后发布同一session/generation1的2760×1256 surface，实际截图显示完整Windows桌面。当前本地显示随后变为横屏2760×1256，冻结快照仍为原revision44。这证明大于2560的物理像素意图实际进入本次连接并匹配runtime画面；没有采集握手wire，不把这一匹配称为独立服务端ACK。

一次Start位置触控后，17:05:32原始截图仍有菜单逐步到达的中间画面；17:06:12截图为完整菜单，随后关闭恢复桌面。不能将最终完整画面描述为整体低延迟或所有局部刷新问题已解决。

官方Emulator旋转到竖屏后，真实本地快照为1256×2760、XComponent同尺寸；session1/generation1、冻结快照及远端2760×1256保持，画面按比例适配并出现预期留黑。再旋回横屏，XComponent恢复2760×1256，没有新launch。右下浮动岛详情显示“已连接”，主动断开后17:08:47回到竖屏登录表单。原始PNG均已人工查看；个别官方截图的原始方向与UI布局坐标不同，真实屏幕方向以同次SDK快照与布局共同判定。

原始日志、脱敏布局、截图与命令回执在 `target/harmony-display-geometry-20261015/fix1/phone/`。上述观察不验证远程物理键盘/IME，也不验证PC、真机、Mac协议或所有显示器场景。

## 待验收

PC需单独安装最终包并核对实际系统快照、投影意图、协议发布 surface 和呈现。协议请求/服务端 ACK 需要各自直接证据；不得将上述三个对象直接视为握手抓包。仍需 PC 搬窗/外接屏、完整键盘/IME、真机与生产资格。PC 表单保留给用户填写，本轮尚未覆盖安装或提交。

同包第二次从HUKS恢复后，session2冻结新的revision123竖屏快照，仍为2760×1256投影/协议surface，17:11:08完整桌面截图已查看；最终再次主动断开，acceptedSurface=null，本地快照恢复1256×2760。两次冻结revision分别44/123，排除了仅重复引用首会话快照。汇总断言和原始序列在`phone/bounded-sessions-summary.json`及其列明的四个时间顺序dump；hilog为滚动缓冲，不能仅由最终dump复原第一会话。记录不含连接身份或凭据。
