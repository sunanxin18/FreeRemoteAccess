# GTK 帧事务适配器

本组件适配 GTK 4.14，以固定 gtk4-rs 0.9.7 的 `v4_14` API 为基线。仅 Linux x86、x86_64、ARM64 的 `gtk-shell` feature 编译 GTK 后端；macOS/Windows 不加载 GTK 系统依赖。它接收已有 `FrameTransaction`，复用 GL 执行器上传、绘制，并由 runner 接入登录窗口、窗口提交确认和原生键鼠输入。

`GtkFrameArea` 应由 GTK 主线程持有，`widget()` 加入宿主窗口。`submit_batch` 仅保留一个完整批次，至多 4096 个事务、256 MiB 像素；拒绝时 `RejectedBatch` 原样返回批次。调用者必须保留并重试 Busy，不能丢弃依赖前序内容的增量。`drain_events` 返回同类合并的有限状态事件。`Drawn` 携带非 Clone `DrawReceipt` 和渲染同一 `ContentViewport`，它只证明 GL 命令提交，不证明 GTK snapshot 被合成或显示，不能发送生产 ACK。

每次 render 验证实际 GDK current identity，按 GLArea allocation × scale 计算像素范围并重新 capture 现场目标。目标显式使用 `SrgbEncodedRgba8`，不把任意 LINEAR 附件推断为 sRGB 内容。RGBA8、LINEAR、二维纹理 level 0、无 MSAA、单绘制附件等门禁由 GL 后端执行。GPU 完成采样、BGR 通道和 sRGB 编码，alpha 恒为 1；生产没有 CPU 像素转换或读回。

普通 unrealize 信号回调在 GTK 类处理器释放上下文之前 make_current、detach，并 mark_lost 撤销旧证明。无法 current 时只放弃资源名称。主动 `detach` 永久关闭组件；普通 Rust Drop 不调用 GL。unrealize/错误会清除待执行批次，并要求新的完整 Startup，调用者须据 `Invalidated` 恢复，而非续传旧增量。宿主若需要保留组件跨 realize，必须重新供给完整 Startup。

GL 加载读取 libepoxy.so.0 公开的 `epoxy_gl*` 函数指针变量内容（两层间接寻址），不调用私有 `epoxy_get_proc_address`、resolver 或 thunk 符号。每个上下文生命周期独立创建 glow，不能跨上下文复用加载结果。GdkGLContext 与 libepoxy Library 均由 actual-current 闭包强持有，直到最后一个 renderer/receipt 放弃引用；上下文失效后调用全部 fail-closed。

官方接口证据：

- [gtk4-rs 0.9.7 GLArea API](https://github.com/gtk-rs/gtk4-rs/blob/0.9.7/gtk4/src/auto/gl_area.rs)，render/allowed_apis/required_version。
- [GTK 4.14.0 render 生命周期](https://github.com/GNOME/gtk/blob/4.14.0/gtk/gtkglarea.c#L747)，先 make_current/attach_buffers，再发 render，最后封装纹理 snapshot。
- [libepoxy 1.5.10 公开函数指针变量生成](https://github.com/anholt/libepoxy/blob/1.5.10/src/gen_dispatch.py#L549)。

本机 macOS 可运行纯队列/生命周期与几何测试，不是 GTK 运行证据：

```sh
cargo +1.96.0 test --locked -p frd-shell-gtk --features gtk-shell
```

原生 fixture 还覆盖同 session 增量 patch、unrealize 后重新 realize 的 Startup、旧回执撤销、realize 前/后 detach，以及 GTK set_error 后拒绝并原样退回批次。建议 CI 使用 `G_DEBUG=fatal-criticals`，将 GTK 生命周期误用作为失败。

原生 Linux CI 必须安装 GTK >=4.14 开发包、libepoxy、Mesa 及独立显示服务，并显式执行 ignored fixture：

```sh
FRD_GTK_TEST_BACKEND=x11 FRD_GTK_TEST_SCALE=1 \
cargo +1.96.0 test --locked -p frd-shell-gtk --features gtk-shell \
  --target x86_64-unknown-linux-gnu --test native_gtk \
  native_gtk_frame_adapter_roundtrip -- --exact --ignored --nocapture
```

同一命令覆盖 Linux i686 / ARM64 native target，X11 / Wayland 和 scale 1 / 2。测试核对真实 GDK display 类型与 GLArea scale；X11 使用独立 Xvfb/GDK_SCALE，Wayland 必须配置独立 Weston 输出 scale，不能用 GDK_SCALE 假装 compositor scale。要求恰好 1 passed、0 ignored；必须设置外层有界 timeout。fixture 的 GL readback 仅在测试中读取真实事务渲染的中间灰、非端点颜色、原点和不透明 alpha。Mesa llvmpipe 是原生 GL 管线验证，不是硬件加速或远程会话证据。

## GTK runner 当前接线

`GtkRunner::new(AppLaunch, factories, GtkRunnerStores, AudioOutputFactory)` 直接复用 AppController/SessionHost；`present()` 展示真实 GTK 原生 HeaderBar、单列表单和 GLArea。PasswordEntry 激活与 Connect 按钮共享提交路径；平台原生窗口按钮由 GTK 保留，产品状态、详情和取消/断开按钮位于 HeaderBar 的中心 title widget。没有新增自定义功能图标。HeaderBar 以下在连接期间仅有远程内容组件。

产品事件泵使用合并 wake 的异步主线程消息，覆盖启动结果、取消、迟到启动、清理、证书 pending commands、后台加载密码和 deferred profile persistence。controller 读取的 profile store 是只读内存快照；真实目录 list/upsert 在后台 job 中执行，缓存从 AppLaunch 已有目录初始化。选择新连接或修改连接身份会清除已加载的旧密码。不同 session 重建 GLArea 和 frame pump；清理会丢弃旧事务、纹理及事件，防止重连时显示旧会话内容。

runner 在确认独立 WindowSubmission 后发布 `FramePresented`，仅在当前完整首帧建立 `InputGate::Interactive`。GLArea 的键盘控制器通过当前 GDK 设备和 XKB 映射解析 USB HID 物理键，释放沿用成功按下时的映射；指针使用同一 `ContentViewport`，窗口失焦、代际变化、画布失效或拖出内容区会发送一次 `ReleaseAll`。输入仍由 `AppController::route_input` 和 `SessionHost` 发送，GTK 不直接依赖 RDP wire 类型。Linux 正式组合根现在使用 GTK `Application`/`ApplicationWindow`，但合成输入与软件 Mesa smoke 不构成物理 Wayland 输入、硬件 GPU 或真实 RDP 控制证明。当前能力继承组合根传入的工厂和策略，不会把尚未接线的视频、音频、剪贴板伪装为支持。

随 Linux 包提供 Noto Sans SC 变量字体到私有 `share/fonts/freeremotedesk` 目录。runner 通过 fontconfig/PangoCairo 公开 ABI 建立窗口树私有 font map，不修改全局或用户字体配置；缺少该 ABI 时保留宿主字体并让包验证失败，避免用系统安装状态掩盖缺失资源。

2026-09-08 GUI 实现依据：已读取 Apple 官方 DocC text-fields/windows JSON，采用持久字段标签、安全密码字段、合理 Tab 顺序、原生系统窗口控制。M3 主站仅返回 JavaScript，当前工具无可用浏览器，因此改读 Google 官方 Material Web 的 text-field 文档（label、password、验证行为）；不能把此替代读取记录为完整 M3 交互页面或视觉验收。正式 GUI 验收仍需 scale、主题、键盘/IME 和窄窗检查。

- [Apple Text fields](https://developer.apple.com/design/human-interface-guidelines/text-fields)
- [Apple Windows](https://developer.apple.com/design/human-interface-guidelines/windows)
- [Google 官方 Material Web text field](https://github.com/material-components/material-web/blob/main/docs/components/text-field.md)

每个 backend/scale 独立执行三个 fixture，并分别验证恰好 1 passed、0 ignored：

```sh
cargo +1.96.0 test --locked -p frd-shell-gtk --features gtk-shell --target "$TARGET" \
  --test native_gtk native_gtk_frame_adapter_roundtrip -- --exact --ignored --nocapture
cargo +1.96.0 test --locked -p frd-shell-gtk --features gtk-shell --target "$TARGET" \
  --test native_runner native_gtk_login_session_cancel -- --exact --ignored --nocapture
GDK_DEBUG=gl-egl cargo +1.96.0 test --locked -p frd-shell-gtk --features gtk-shell --target "$TARGET" \
  --test native_submission native_gtk_window_submission_roundtrip -- --exact --ignored --nocapture
```

统一使用独立 Xvfb/Weston、`FRD_GTK_TEST_BACKEND`、`FRD_GTK_TEST_SCALE`、`G_DEBUG=fatal-criticals` 和外层 timeout。runner fixture 使用真实 GTK 表单激活与 mock ProtocolFactory/ProtocolRuntime，覆盖保存密码加载、新连接清密、修改目标清密、Enter 一次启动、后台保存、取消/清理、重新连接的新画布增量读回及 pending launch cancellation；不连接网络，也不是硬件键盘注入或实际 RDP 服务端互操作证据。WindowSubmission fixture 的窗口提交含义由独立呈现模块定义，仍不等价物理扫描输出或协议 ACK。
