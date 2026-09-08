# GTK 帧事务适配器

本组件适配 GTK 4.14，以固定 gtk4-rs 0.9.7 的 `v4_14` API 为基线。仅 Linux x86、x86_64、ARM64 的 `gtk-shell` feature 编译 GTK 后端；macOS/Windows 不加载 GTK 系统依赖。它接收已有 `FrameTransaction`，复用 GL 执行器上传、绘制；尚未接入登录窗口、Linux 产品入口、输入或生产呈现 ACK。

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
