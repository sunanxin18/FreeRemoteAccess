# 鸿蒙手机与PC会话浮动岛验证（2026-10-15）

当前状态为 **受限验证**：源码与普通包已完成，手机真实会话通过下述有限GUI检查；PC真实会话与物理键盘/IME仍未验证。

## 实现范围

用户指定的右下浮动岛替代额外56vp产品工具条；远程XComponent占满平台安全区内可用内容，展开/收起不重建表面。本次原包手机保留挖孔与导航安全区，PC恢复系统原生标题栏；后续按用户反馈已增加[手机会话沉浸全屏](harmony-phone-fullscreen-20261015.md)，消除系统导航白区。Windows、macOS、Linux客户端及wire/decoder均不修改。复用固定Material Symbols Rounded字体与Apache许可，24vp图标、48vp按钮，中文可访问名称与鼠标/键盘提示，原生按钮交互态和亮暗资源。成功连接空闲3秒收起；鼠标离开700ms；详情、按住、焦点、错误保持可见。点击入口或Ctrl+Alt+Home展开；本片不增加远程面全域hover感应，以免远端拖动时扩展吞掉mouse-up。

登录竖屏、成功首帧后横屏、随后自动旋转的策略保持。详情/断开/取消按本地输入所有权处理，计时隐藏不抢焦点。本地新键不传远端；旧会话可信尾Up仅用于来源匹配的隔离清理，不能借新会话身份发送普通键。

## 中间包受控GUI（fix1，非最终接受包）

证据目录 `target/harmony-session-island-20261015/fix1/`。真实Release构建、官方签名/验证、签后原31ZIP条目逐字相等；unsigned26579ce2、signed412c09be、ABC6540d6eb；打包FFI206c603f。两台均官方HDC安装/启动成功并保留应用数据。

手机真实Windows/HUKS登录：14:42:02连接仍竖屏，14:42:11真实桌面自动横屏。物理旋转left后，collapsed/expanded/details/close五个布局中的XComponent均left108/top0/2652×1158；自动隐藏不收缩内容矩形。右下入口可展开，详情停留20多秒未被隐藏。首个隐藏后远程触摸打开Windows开始菜单，关闭详情后的首个远程触摸关闭菜单，证明没有永久阻断指针。竖屏旋转仍连接，14:45:55主动断开回竖屏登录。截图为官方Emulator原图，未裁切或重绘。该结果不代表完整键盘、复杂多点、远端性能或长期稳定性。

PC：正常窗口原生DecorBar70px，client2090×1324；最大化后原生栏3120px宽，没有额外产品工具条；还原恢复2090×1394外窗/2090×1324内容，标题栏滑动使外窗从(515,281)移到(735,421)，尺寸不变。尚未进行PC真实认证，因此不把手机结果计为PC浮动岛会话/键盘验证。打开系统输入法菜单仅观察，未更改协议或权限。

## 源码修复与最终包

最终shell与OHOS私有native集成经独立Spec PASS / Quality PASS。修复多点/Cancel误恢复焦点、过期详情键回调及真实尾Up丢失三项缺口。native以原session/generation保存held/quarantine，accepted Up遇到control/geometry/presentation屏障仍只清匹配来源；旧来源不能清新状态。JS保留原Down材料跨page0，QueueFull保留pending并显式重试。0仅表示入队，不是Owner/远端ACK。ABI、协议核心、其它平台和键能力门保持。

真实回归先RED（6个Owner用例1通过5失败），修复后root完整Rust1.96 library测试51通过/1原有ignored；普通library check通过。最终root及独立审查各运行23项纯岛策略、原11项方向测试通过；不是物理键盘/IME证明。

root按两份审查freeze核对15项源码，实际Release BOTH HAP构建成功，构建/签装后再次核对未变。官方签名、验签和签后checker通过，原31ZIP条目逐字相等，仅signer增.pages.info；两台官方HDC stop/install/start均成功，无卸载/清数据。

| 对象 | SHA256 |
|---|---|
| Index | `7e1d3d0ef17113c897aa769b3f08a836d39b47408e36d7259a5e4566599b3a01` |
| SessionIslandPolicy | `959ff1e93cd6afcba74ec894c01a934bf0bd648fde1802e28e87f3dc807f01a1` |
| app_adapter | `919d9cfa28975aa48e15d8ed49bd9ee48e41f481149abe7360650b157d1a4cc0` |
| input_adapter | `152700273da006a09bd137611694396fb6d25bd27815699f56f9438f7c9018f1` |
| unsigned HAP | `fa3d8ba9ec8423ca27bf0888acd6fc7e0ea797a7916f370a8b45aaacff1044bb` |
| signed HAP（22,816,257 bytes） | `1bdf20070848e13bb7c8f54b516e1f45095fd58c1d189d7998bb7c0a8e6101a0` |
| ArkTS ABC | `2149ce1d391213b51b668de3d967b9ff32f08e6f8019b1281c071516fcd75ae4` |
| source / packaged FFI | `139673daa82088ac66024e67443a4d82e17cc44bad25a3e84c7ac96921afe746` / `2b4cad3106b00f3a7531ad03c486641d186f1a5f223105a1a0c58af2f931c7f3` |

证据 `target/harmony-session-island-20261015/final/`：source-before-build、build/native-build结果与日志、package/signed-identity、redacted signer、root三组测试、两端安装记录及runtime原始布局/截图。

最终手机（系统深色、普通字体）15:04:38登录表单完整显示，选原HUKS连接；15:05:28等待首帧仍竖屏，15:05:54真实Windows已自动横屏且入口收起，其间没有CLI旋转。之后物理left，展开/详情/关闭/隐藏前后6份布局的XComponent均top0/left108/2652×1158，root程序逐项比较相等。深色岛glyph/边框与详情截图已逐张查看；详情15:06:35到15:07:02保持打开，空字段不再制造大块空白。关闭详情后的首次远程触摸打开Windows开始菜单，隐藏后首次触摸关闭菜单，真实指针可用；15:07:48主动断开，返回竖屏登录。截图中开始菜单有局部刷新现象，保留为独立graphics限制，不能据此宣称刷新/性能问题已修复。

最终PC15:08:00正常原生标题栏/表单布局已重新观察；真实认证仍等待用户在安全框手动填写，不能将手机结果算PC会话验收。CLI填密码会进入进程argv，故按项目secret规则不使用该方式。

最终包还在浅色、系统字体slider5“特大”下重新通过HUKS登录，15:11:49浮动岛按钮均168×168px（48vp）、glyph完整，表面仍2652×1158；详情标题/状态/关闭完整显示。15:12:44会话旋转到竖屏，表面1256×2526且右下动作仍可用，15:13:08主动断开。大字登录表单主体同页可见，但目标/模式选项文字省略；未认定整个大字表单完全通过，也未进入“关怀模式”。15:14:12字体已恢复slider2“标准”，15:14:13深色模式“已关闭”，其余粗细/显示缩放与亮度未修改；原始记录在theme-check。

## 未覆盖

真实物理键盘/IME、远端收键、复杂多点与拖动、PC深色/大字、关怀模式、真机、完整后台恢复、PC真实认证与重连均须独立验收。常规远程画面仍按比例适配，黑边可能来自目标分辨率/安全区；本片释放额外工具条，不改变远程分辨率策略。普通RDP/MVS组合仍为显式默认关闭的实验构建，不能声称所有平台生产资格。
