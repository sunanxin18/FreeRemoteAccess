# Linux 外部上下文 GL 后端（第一阶段）

`linux-gl` 功能仅在 Linux x86/i686、x86_64、AArch64 提供实际后端；其他平台的
`available()` 返回 false。固定使用 glow 0.17.0，共享 frd-render-state 借用候选事务。
本模块不依赖 GTK，也不连接生产远程会话或呈现 ACK。

当前仅接受 desktop OpenGL 3.3+ core 使用方式。GLES、默认 framebuffer、renderbuffer
颜色附件、未知颜色编码均拒绝。目标必须为当前 COLOR_ATTACHMENT0 的完整 sRGB
二维纹理 FBO；复核附件尺寸和完整 viewport。宿主若使用 clip-control，必须保持
LOWER_LEFT / NEGATIVE_ONE_TO_ONE，否则拒绝绘制。绘制核对远端尺寸、drawable 和
content rectangle，清黑独立内容 FBO，再绘制远端内容；polygon FILL、viewport、
clear color、颜色掩码、纹理/采样器和修改的能力状态均保存恢复。

BGRX 原始字节上传 SRGB8_ALPHA8，shader 解码后 `.bgr` 并固定 alpha=1；上行输入
经 UV 翻转显示于顶部。stride 为4倍时用 UNPACK_ROW_LENGTH 一次上传，否则逐行
提交原切片。生产代码没有 CPU 转色、像素重排或 framebuffer 读回。

`ExternalContext::new` 是集中的 unsafe 宿主边界：loader 和 actual-current 检查
必须绑定同一真实上下文，闭包维持宿主对象存活，操作期间禁止重入/换 context。
Rc 保证对象不跨线程。正常 unrealize 调用 renderer.detach，再 drain_deletions，
然后销毁原生上下文。普通 Drop 只排队；上下文丢失必须先 mark_lost，旧 GL 名称
不在新上下文删除。宿主遗漏 drain/detach 时资源可能保留到原生上下文销毁，不能
将 Rust Drop 当作已完成 GL 清理。已 detach 的 renderer 不可重用。

写入/绘制错误隔离纹理并清除 pending receipt；恢复要求精确 session/generation
完整 reset。DrawReceipt 不可复制，仅含只读身份和 is_valid；上下文丢失、后续批次、
绘制、错误、detach 或 renderer Drop 均撤销旧记录，序号溢出 fail-closed。
DrawReceipt 不等价于屏幕呈现，不能用于生产 ACK。

本机非 Linux 可运行3项纯逻辑测试。Linux 原生测试必须显式执行：

```sh
cargo test --locked -p frd-render-gl --features linux-gl --test native_egl \
  native_egl_renderer_roundtrip -- --exact --ignored --nocapture
```

EGL 使用 pbuffer；Mesa 无窗口环境可设置 `EGL_PLATFORM=surfaceless`。必须核对
恰好1项测试执行通过；非 Linux 测试文件被平台门控后为0项，不能算原生通过。
fixture 测试中才执行像素读回，覆盖非端点颜色/灰度、alpha、上下方向、两种stride、
letterbox 清黑、错误viewport、真实current解绑、回执撤销和detach。它不证明GTK
集成、窗口系统呈现或硬件GPU；llvmpipe应明确标为软件GL。
