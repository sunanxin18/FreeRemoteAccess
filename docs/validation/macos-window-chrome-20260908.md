# macOS ARM64 控制岛与窗口几何验证（2026-09-08）

## 修正范围

macOS 每次重绘强制显示控制岛、标题栏布局忽略可见性，这两处独立原因已移除。
显隐复用共享控制器，始终保留原生交通灯与固定远程内容矩形。

首次完整帧以前，macOS 平台适配器读取当前 NSWindow/NSScreen 的 visibleFrame，
使用当前窗口样式换算可用 content rect，再按真实远程像素比例与有效标题栏高度
设置初始窗口，并将原生 frame 平移限制在工作区内。Windows/Linux 默认适配器
返回 None，保留既有行为。用户后续调整、重连和全屏不由该首次策略改写。

不修改 RDP 请求分辨率、解码器、等比渲染或指针坐标契约。任意比例手动窗口或
最大化仍可能合理留边；本次解决默认窗口几何引入的不必要侧边。

## 自动验证

- 控制岛原生标题栏显隐回归：固定远程区域、中心对齐、原生命中区、唤出/隐藏状态。
- 首次窗口计算：横屏、竖屏、高分辨率、1×/2×缩放、工作区限制、非法几何拒绝。
- 重连继续保留用户窗口尺寸。

本地 ARM64 Rust 1.96.0：shell library 214 项测试通过，包含上述新回归。
ARM64 release `.app` 重打包、嵌套签名与包校验通过。

## 真实 GUI：受限验证

在当前显示器原生模式、默认窗口、实际 Windows RDP 登录下：

- 首次窗口完整容纳画面，未出现此前左右宽黑边，未需用户手动缩放窗口修复。
- 初始连接后控制岛隐藏，Ctrl+Alt+Home 可唤出；返回远程画面后再次隐藏。
- 默认编码会话中，控制岛隐藏时鼠标点击打开 Windows 开始菜单，键盘输入
  `notepad` 在搜索栏真实显示；Escape 退出搜索，断开返回表单，关闭进程 exit 0。
- 验证没有打开/编辑远程文件，也没有执行远程终端命令。

## 实验 EGFX 的独立故障（未作为通过结果）

同一 GUI 修正的显式 AVC444 实验会话曾在 47 次 runtime frame acceptance 后
出现 `ProgressiveState / region lacks current frame tiles`，诊断从 confirmed=true
变为 false，画面冻结。用户同时报告鼠标键盘无响应。这一冻结不能证明输入被
控制岛拦截；随后默认编码 GUI 已实际验证输入正常。

该次失败前 ClearCodec 150 次、Progressive 36 次，实际 AVC420/444 仍为零。
不得用默认编码验证代替 Progressive 修复或 H.264 互操作验收。严格 region/current
outer frame coverage 校验保留，尚需无像素的失败几何证据定位；不读旧帧补齐空洞。

本次仅覆盖当前主机/显示器和所见外观，未宣称多显示器、多 DPI/主题、全部快捷键、
长期会话或任意比例全屏无留边通过。


## 致命图形错误的可见终止

活动会话在发布固定诊断以后检查 EGFX 致命失败，返回 `rdp_egfx_failed`，经过原有
音频/剪贴板清理、输入释放与传输关闭路径退出。UI 显示“远程画面解码失败”及
“连接已停止。请使用默认图形模式重新连接。”，不会继续把冻结的旧画面标成已连接。
正常重激活产生的 `Reactivation` 停用标记不作为解码失败；未启用 EGFX 的默认
会话不改变。该错误传播修正不代表 Progressive 缺少当前帧 tile 的原因已经解决。

最终自动回归：RDP 307 passed / 3 ignored，UI 34 passed，macOS 应用 18 项单元测试及 2 项依赖边界测试通过；`cargo fmt --all -- --check` 与 `git diff --check` 通过。错误终止的 observer 顺序和真实 adapter 重激活豁免有确定性回归；新错误页面尚未通过人为故障注入作 GUI 验证。


## 后续诊断与完整工作区验证

Progressive 的确切 coverage 失败现在记录固定大小的数字快照：outer EGFX frame ID、
surface/context ID、失败矩形、缺失 tile 坐标，以及本帧/本 region 的 tile 数量。
不包含像素、压缩数据、远程文字或凭据。快照在每次 decode/frame clear/reset 时清空，
失败时由 EGFX 在释放解码状态前复制到现有实验 observer。

独立只读审查通过；真实 WireToSurface2 回归验证了诊断跨失败清理保留、无部分输出
和新 adapter 无旧诊断。完整 `cargo test --locked --workspace` 终态 exit 0：
58 组、1666 passed / 0 failed / 16 ignored。RDP 单独 309 passed / 3 ignored。
该诊断版本尚未接管当前用户会话进行真实复现，不能据此声明 coverage 根因已修复。

## 锁屏期间的输入与协议核查

后续 CUA 检查确认本机锁屏且自动解锁失败，因此未接管、重启或断开当前客户端。
只读代码审查没有发现控制岛隐藏时修改原生窗口焦点或关闭 InputRouter 的动作。
RemoteSurface 键盘域先于 egui 分发；指针仍受 egui consumed、命中矩形、原生焦点和
交互 epoch 保护，不能为了绕过未复现故障而移除这些门控。

新增真实 egui Context 回归：先绘制可见控制岛并确认消费指针，移动到远程内容后，
首个隐藏 pass 即不再消费指针，第二个 pass 及按下/释放继续核验。将实际消费结果
交给现有归属判定和 InputRouter，验证点击恢复 RemoteSurface 后可发送指针及按键；
失焦、本地按键未释放及缺失交互 epoch 时仍保留保护。既有及新增 shell 215项
全部通过。测试没有运行 AppKit/winit 原生事件循环或真实网络发送，不能据此关闭
用户报告的现场故障。

再次核对微软 [MS-RDPEGFX 2023-09-20 §2.2.4.2.1.5](https://winprotocoldoc.z19.web.core.windows.net/MS-RDPEGFX/%5BMS-RDPEGFX%5D-230920.pdf)：
region 的覆盖瓦片必须来自该 region 或当前外层帧中先前的 region。当前解码器在
外层 EGFX Start/End 边界清空 frame_tiles，不在 codec 内层 FrameBegin/End 清空。
因此没有依据通过读取上一外层帧的像素来消除错误。规范核查支持保留现有校验，
不证明真实失败是服务端违规；实际失败几何仍须在解锁后采集。
