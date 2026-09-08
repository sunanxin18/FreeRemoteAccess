# Linux 客户端补齐设计

本设计细化已批准 RDP 多平台计划的完整客户端包门禁。Linux 目标为 i686、x86_64
和 AArch64；macOS 仍只支持 ARM64。已有 Linux FFmpeg 包不等于客户端。

## 边界与实现顺序

1. 新增 `frd-platform-linux`，仅实现现有 `frd-platform-api` 窄接口，不引用
   Windows/macOS crate，不改变协议、解码器和 SurfaceUpdate。
2. 新增独立 Linux 应用组合根，复用登录工作流、会话管理和桌面核心；默认 RDP
   LegacyOnly，现代编码只有精确 backend 可用时才允许显式 ValidationOnly 实验。
3. 单独处理 Linux 原生窗口/显示/GPU 后端，不将 DX12 或 macOS 交通灯布局搬入
   Linux。窗口标题栏、装饰与 X11/Wayland 的约束须另行进行 GUI 验收；没有运行证据
   时不得把编译当成 GUI 已验证。
4. 最后提供完整可执行包、desktop entry、图标和随包 codec verifier，并在三个目标
   的 CI 运行包验证与 fixture。真实服务端控制是另一个独立门禁。

## 平台服务

- 非秘密配置使用 XDG_DATA_HOME 下的 FreeRemoteDesk 目录；未设置时使用
  HOME/.local/share。拒绝相对环境路径，不把 HOME 等系统变量改作临时目录。
- 应用目录/锁文件须检查类型、所有者与权限；元数据原子替换且有并发锁，不接受
  符号链接指向另一个对象。证书首次写入后遇到不同指纹拒绝覆盖。
- 密码只使用标准 Secret Service，经加密 D-Bus session 访问；服务不存在、锁定或
  权限不足时明确 unavailable，绝不写入普通配置或生成本地加密文件替代品。
- 认证前 stage 仅保存于可清零内存，成功后 commit；失败、取消或退出 discard。
  session 与 profile key 精确绑定，字段采用长度分隔的哈希标识，避免泄露账号到
  keyring 索引或日志。Secret Service 调用不在绘制/输入线程执行。
- 持有文件描述符的单实例锁，关闭描述符释放锁，不删除锁文件而制造 inode 竞争。
- 音频在真实 Linux 后端接线前明确 unavailable，不虚报本机设备能力。

采用现有窄接口而不是增加跨平台“万能服务”，以减少对 Windows/macOS 的修改。
复用经过审查的标准 Secret Service 客户端库，避免自行实现密码学或调用外部命令
传递秘密。系统 keyring 服务属于客户端宿主的标准服务，不是远程服务器 companion。

## 验证

平台服务先验证合成 fixture：pin TOFU/变化拒绝、配置并发与损坏拒绝、XDG 路径、
权限/链接拒绝、单实例释放、stage/commit/discard 绑定，以及 Secret Service
不可用/锁定失败关闭。使用注入的测试 keyring 验证事务，不能把 mock 当成真实
Secret Service 往返证据。后续 Linux 原生 CI 需独立运行临时 D-Bus/keyring fixture。

官方参考：[Secret Service API](https://specifications.freedesktop.org/secret-service/latest-single/)、
[Secret Service Rust API](https://docs.rs/secret-service/latest/secret_service/)、
[XDG Base Directory](https://specifications.freedesktop.org/basedir-spec/latest/)。

## Linux 窗口壳审查结论

2026-09-08 只读检查固定 winit 0.30.13：X11 的 set_decorations 仅发送 Motif hints，show_window_menu 为空实现；Wayland frame 是私有对象，公开 WindowAttributesExtWayland 只提供 with_name。没有通用接口把产品控件注入 SSD/CSD 标题栏。当前 Linux layouts 让控制岛覆盖 client area；改用 titlebar_layouts 又保留 SSD 会形成第二条工具栏，两者均不满足最终产品边界。

下一步必须独立验证 Linux 原生 toolkit 窗口壳。首选研究 GTK4 Window.set_titlebar + HeaderBar：按 gtk-decoration-layout 保留宿主按钮行为，session 控件相对窗口居中，renderer/input 共同消费 HeaderBar 以下的实际 content allocation。GTK 类型不进入协议、decoder 或 SurfaceUpdate。不能直接把 GtkHeaderBar 附到既有 winit Window，也不能把每帧 GPU→CPU readback 当成最终 GPU 集成。

先完成原生 GPU 内容区域与事件映射的技术验证，再迁移应用组合根。验收需真实 X11 和 Wayland、左右按钮布局、光暗主题、1×/1.25×或1.5×/2×、拖动/缩放/全屏/系统菜单、IME/焦点和远程坐标。当前审查不是 GTK GPU 实现或 GUI 通过证据。M3/Apple 部分页面只返回 JavaScript 占位，正式视觉实施前仍需渲染页面复核。

窗口身份可独立修正：winit ApplicationName 同时映射 Wayland app_id 与 X11 WM_CLASS，统一为 freeremotedesk 并与 desktop entry / StartupWMClass 对应。该 Linux-only 配置不改变标题栏几何，也不宣称解决上述装饰问题。

参考：[GTK set_titlebar](https://docs.gtk.org/gtk4/method.Window.set_titlebar.html)、[HeaderBar title widget](https://docs.gtk.org/gtk4/method.HeaderBar.set_title_widget.html)、[宿主按钮布局](https://docs.gtk.org/gtk4/property.Settings.gtk-decoration-layout.html)、[WindowHandle](https://docs.gtk.org/gtk4/class.WindowHandle.html)、[GNOME Header Bars](https://developer.gnome.org/hig/patterns/containers/header-bars.html)。

## GTK 与当前 GPU 后端的接线边界

固定 wgpu/wgpu-hal 30.0.1 审查：GLES Adapter::new_external 允许使用当前外部上下文，但要求创建、使用及销毁所有派生对象时上下文保持 current。高层 Instance::create_adapter_from_hal 另要求 adapter 来自该 instance 的内部 handle，独立 GTK 上下文没有已验证的构造路径满足此前提；不得用任意新 Instance 或 unsafe Send/Sync 伪装解决。

RemoteRenderer::record_in 已能接收离屏 TextureView，但当前 compositor 的 acquire/present 持有 SurfaceTexture，当前 pass 要求 sRGB 目标。GtkGLArea 的实际 FBO 编码与完成强度均须实测，不能把未知目标标为 sRGB，也不能把上传、queue_render 或 callback 到达当作实际呈现确认。

已将 RemoteUpdateState、计划与 receipt 状态机迁至独立 frd-render-state crate，保留 frd-render-wgpu 根路径公开类型、GPU 执行与成功确认行为。新 crate 只依赖 frd-core/frd-frame，不加载 wgpu 或 GTK；平台能力声明不变。

下一实现方向：Linux GL 执行器消费同一事务计划。在 GTK current-context 生命周期内管理 texture/FBO，禁止每帧读回CPU。BGRX/BGRA方向与色阶用四角fixture验证，视频后续按原 VideoFrameLayout/VideoColorSelection 采样平面。unrealize/context丢失必须撤销未确认 receipt，并在释放GL资源后请求新完整基线；不能确认旧代帧。

该结论来自固定依赖源码与[GtkGLArea官方文档](https://docs.gtk.org/gtk4/class.GLArea.html)审查，不是GTK/wgpu互操作已实现。独立原生窗口探针已有三架构 X11 1× 运行报告，边框校验修正后离线复验通过；2×/Wayland 待复跑，GL 执行器第一阶段见下节；GTK 产品接线尚未实现。

### 共享状态 crate 的提交边界

纯状态已迁移到 frd-render-state，只依赖 frd-core/frd-frame。纯 TransactionError
明确映射至原 RendererError；GPU 错误、设备尺寸上限和目标色彩格式留在后端。
不能把现有 PlannedBatch 的 staged_state 直接公开：新 BatchCandidate 独占借用原
RemoteUpdateState，字段私有、不实现 Clone，只暴露只读操作，commit(self) 消费一次，
丢弃候选不修改已安装状态。候选存活期间不可 clear、恢复、再规划或确认回执。

Wgpu 在既有 GpuCleanToken 的 commit_if_unchanged 内消费候选并安装资源；GL 后端
提供自己的 context/错误代际检查。纯状态 crate 无法证明实际 GPU 执行成功，
不引入可伪造的公共成功 token。ConfirmedPresentation 构造仍由后端控制，禁止用
上传完成或 GTK queue_render 代替真实呈现确认。原有38项测试全部保留为纯状态10项和后端28项；新增3项候选运行时回归及7项编译拒绝测试通过。后端Metal 28项及原有文档测试通过，覆盖候选丢弃、不可重复/跨实例提交及不可修改操作。


### 独立 GL 执行器第一阶段

frd-render-gl 将 GL 对象限制在 Linux i686/x86_64/AArch64 当前线程，使用宿主提供的
真实上下文身份检查和生命周期 epoch。共享候选只在 GL 操作及宿主状态恢复检查成功后
提交；执行失败隔离旧纹理并要求精确完整恢复。BGRX sRGB 数据直接上传，由 shader
调整通道、顶部行方向和不透明 alpha，不增加 CPU 转色或生产像素读回。

第一阶段仅接受 desktop GL 3.3+ core 和 sRGB 二维纹理颜色附件，验证真实附件尺寸、
FBO、viewport 和远端尺寸。默认 FBO、GLES 和 renderbuffer 不在当前契约内；GTK
实际目标格式必须另行核对和接入，不因技术探针能绘图而假设满足该契约。
DrawReceipt 仅描述可撤销的绘制记录，不产生生产 ACK。真实 GTK 呈现时序、窗口登录
流程、输入与会话接线仍是下一阶段，不能由 EGL pbuffer 测试代替。


### GTK 会话接线的具体接口缺口

当前 Linux main 仍创建 winit EventLoop 和 DesktopApplication；现有 SessionHost
虽已从 application.rs 分离，却仍位于 frd-shell-desktop crate。启动、取消、事件与
清理 API 可复用；帧事务的 drain_frame_transactions、CompiledFrameDrain、
FrameCompileFailure 和 retire_frame_presentation 此前为 pub(crate)，现已通过下述窄 API 公开。公开 drain_frame_updates 会丢弃入队时间，不能在 GTK 壳
重新编译帧来绕过这些边界，否则会复制 generation/revision 和呈现退休逻辑。

完成目标观察后，按依赖顺序提供窄的、平台无关的已编译帧提取与退休接口，并保留
BatchMetricContext 的时间/数量信息及旧测试。不要将 GTK 类型加入 SessionHost，
也不要复制 AppLaunch、后台凭据提交、取消或清理逻辑。GL DrawReceipt 尚不具备
生产呈现确认能力；GTK 回调只可驱动上传绘制，真实呈现与会话确认的接线须另行
建立证据，不能直接将该回执转换成 FramePresented。


已公开 drain_frame_transactions/retire_frame_presentation，并从 crate 根导出
CompiledFrameDrain、FrameCompileFailure、FrameBatchMetricsSnapshot。外部壳可只读
查看事务、消费取出原事务、读取原错误与四项指标；构造与字段仍受限。原方法体未改，
没有复制编译器或增加 GTK 类型。retire 是当前会话的永久呈现退休，不能被当作临时
隐藏或可恢复 GPU 上下文丢失的暂停操作。


### GTK 呈现语义与现有后端对齐

当前 wgpu 的 execute_frame_with_fault_scope 顺序为 submit、queue.present、完成错误
scope、确认原 receipt；不是物理 scanout 证明。GTK 应实现同强度的窗口提交确认，
不无意提高门槛，也不把离屏draw或时间戳当作提交。

上游 GTK4.14.0 [Wayland实现](https://github.com/GNOME/gtk/blob/4.14.0/gdk/wayland/gdksurface-wayland.c)
会以 frame callback 时间加估计刷新间隔填写 presentation_time；complete仅表示不再
填入值。故这些字段仅作诊断。正常 GtkWindow surface_render -> widget snapshot ->
GLArea render/append GLTexture -> gsk_renderer_render -> end_frame/swap 在 PAINT 中
同步发生，AFTER_PAINT随后发生。候选接线是在 GLArea render 中确认当前正处于同一
native surface 的 render 信号栈，绑定 surface/clock/framecounter/context epoch/
renderer serial/远端revision，在同counter after-paint中消费一次并复核生命周期。
不能使用 surface::render 的 after handler，它受 true-handled accumulator 截断。

仍须实测和实现：排除离屏snapshot、隐藏/空damage、同帧覆盖和resize/unrealize；
实际 GSK/backend 提交的错误观察不能只读取 GLArea context。GTK4.14 EGL end-frame
忽略swap返回值，因此单独after-paint也不足以达到现有clean-scope强度。须固定并核对
部署的GTK/GSK版本，先取得实际提交链证据再开放生产ACK。此审查是4.14.0上游源码，
尚未声称Ubuntu具体补丁包或产品窗口通过。


### 普通 RGBA8 中的显式 sRGB 字节输出

三架构实测的 GTK 目标为 RGBA8/LINEAR；LINEAR 是附件的硬件颜色转换属性，不能据此
自动推断消费者的颜色语义。新增输出契约由宿主显式选择：原 SrgbFramebuffer 保持
sRGB附件要求；SrgbEncodedRgba8 要求普通RGBA8、LINEAR、二维单采样附件，并约定
消费者将存储字节作为sRGB编码颜色。未知目标或未声明颜色语义仍拒绝。

输入继续上传sRGB纹理，采样器解码后在线性空间过滤。前一模式由FRAMEBUFFER_SRGB
完成编码；后一模式由shader按标准分段函数重新编码后写入RGBA8，并关闭硬件编码
以免重复转换。alpha保持1；原宿主状态在结束时恢复。测试需覆盖暗灰分段附近值、
非端点颜色、上下方向、2×缩放的线性过滤oracle和两契约交叉拒绝，CPU仅用于测试
oracle。此后端能力不等于GTK的组合根、提交确认或远程会话接线已完成。


颜色语义依据：固定 [GTK4.14 GLArea源码](https://github.com/GNOME/gtk/blob/4.14.0/gtk/gtkglarea.c#L465)
分配RGBA8并将premultiplied GLTexture直接追加snapshot；[GSK blit shader](https://github.com/GNOME/gtk/blob/4.14.0/gsk/gl/resources/blit.glsl#L13)
普通texture采样后仅施加alpha/coverage，没有transfer；[GTK官方颜色说明](https://blog.gtk.org/2024/08/11/the-colors-of-gtk/)
明确4.16颜色状态工作前默认假定sRGB。因此4.14这里应存sRGB编码字节；这不是从
GL_LINEAR枚举本身推断。精确RGBA8保证通道8bit，新契约仅level0和samples0；输出
alpha1满足premultiplied解释。桌面BGRA内存格式不能成为额外交换输出红蓝的理由。
此证据不自动覆盖更新GTK的HDR/广色域color-state，也未完成GTK最终显示像素验收。


### GTK 4.14 窗口错误观察的实现路径

固定旧 `GSK_RENDERER=gl` 的上游代码有公开 API 路径补齐提交作用域。
`gsk_gl_renderer_render` 的 end_frame 后，driver_after_frame 只切逻辑 command queue，
未清除原生 current。after-paint 可读取 `GdkGLContext::current()` 并核对
`DrawContext::surface()`；不得先 make_current 覆盖实际窗口绑定。
首帧只发现上下文并请求下一帧；后续 before-paint 对保留上下文建立错误 baseline。
帧外 make_current 是 surfaceless，只用于 baseline；GTK begin/end_frame 自行重绑窗口。

EGL end_frame 忽略 swap 返回值；Wayland notify_committed 只清状态标志。此前仅检查
返回路径没有 eglGetError 不足以保证错误未被覆盖：libglvnd 的 eglGetCurrent*
查询本身也将错误设为 EGL_SUCCESS。observer 必须在 after-paint 首个 EGL 调用
立即捕获错误，再查询上下文；before-paint 也先保存旧错误，make-current 后立即
保存其结果，再建立身份和 GL 错误基线。结合原生 current context/draw surface、
同一framecounter和真实Surface render
调用栈内的精确draw receipt建立提交确认。首帧、最终空damage、离屏snapshot、resize、
unrealize、context变化或缺失作用域必须拒绝。该路径仍须native故障注入验证。
GLX后续可用公开X11 display error_trap_push/pop包住paint，pop返回X请求错误；
不能用pop_ignored或把XSync称为物理显示证明。

源码：[GSK render](https://github.com/GNOME/gtk/blob/4.14.0/gsk/gl/gskglrenderer.c#L345)、
[driver 收尾](https://github.com/GNOME/gtk/blob/4.14.0/gsk/gl/gskgldriver.c#L629)、
[GDK EGL end_frame](https://github.com/GNOME/gtk/blob/4.14.0/gdk/gdkglcontext.c#L652)、
[Wayland end_frame](https://github.com/GNOME/gtk/blob/4.14.0/gdk/wayland/gdkglcontext-wayland.c#L60)、
[X11 error trap](https://github.com/GNOME/gtk/blob/4.14.0/gdk/x11/gdkdisplay-x11.c#L2673)。
这关闭了“找不到公开观察接口”的设计不确定性，不代表生产ACK已实现或通过。

7712d24 原生诊断纠正了 expose 的语义：Surface::render 参数是 GDK expose，
`queue_render` 可以合法传空 region；GSK 随后为首帧或新节点 diff 加入实际 damage。
因此不能以 expose 非空判定窗口绘制。关联仍要求同一 surface/frame/render 调用，
并新增 GLArea draw 时实际 EGL surfaceless context 到 after-paint 不同的实际
GSK window context/非空 drawable 的转换证据。旧 GL 的最终空 clip 分支在
begin-frame 前返回；X11 empty-frame 不执行 GL 操作，Wayland 仅提交 Wayland 状态，
均不能产生该转换。原来的错误作用域、精确 receipt 和生命周期门禁保持不变。
本修正仍待原生矩阵验收，不能先发布生产 ACK。

固定源码：[queue_render](https://github.com/GNOME/gtk/blob/4.14.0/gdk/gdksurface.c#L1439)、
[最终 damage](https://github.com/GNOME/gtk/blob/4.14.0/gsk/gskrenderer.c#L489)、
[空 clip 分支](https://github.com/GNOME/gtk/blob/4.14.0/gsk/gl/gskglrenderer.c#L363)、
[X11 empty-frame](https://github.com/GNOME/gtk/blob/4.14.0/gdk/x11/gdkglcontext-x11.c#L43)、
[Wayland empty-frame](https://github.com/GNOME/gtk/blob/4.14.0/gdk/wayland/gdkglcontext-wayland.c#L87)。

### 原生分辨率选择与消费式呈现接线

Linux 表单使用带持久标签的原生 GTK DropDown 显示当前分辨率模式；顺序与其他
桌面壳一致，放在目标系统之后、地址之前。支持原生显示器（默认）、工作区、
窗口内容、服务器管理，以及固定预设和自定义正整数宽高；不添加 2560 或 4K
上限。非法输入留在表单内解释，不能启动或清除密码。自定义模式显示宽高标签，
控件沿用 44 逻辑像素目标、原生焦点和主题。

本次复核 [Apple pop-up buttons](https://developer.apple.com/design/human-interface-guidelines/pop-up-buttons)
与 [M3 menus](https://m3.material.io/components/menus/overview)；M3 站点正文需 JavaScript，
同时核对官方 [Material Web select](https://github.com/material-components/material-web/blob/main/docs/components/select.md)
的当前选项与标签语义。没有新增产品流程例外。新增原生信号 fixture 不等于
键盘/指针视觉验收，后者仍需单独执行。

GL executor 的消费式确认接口只确认 renderer/context/epoch/serial 及共享帧账本，
本身不证明窗口提交。GTK adapter 必须先消费 WindowSubmission，再在同一个
renderer 中精确消费 pending receipt；确认不切换 context、不调用 GL 或读取错误。
此窄入口供后续 runner 生成 FramePresented，不能直接复制 draw.frame() 发送。

06e382a EGL故障负例纠正上述错误语义：固定 [libglvnd v1.7.0 entrypoint](https://github.com/NVIDIA/libglvnd/blob/v1.7.0/src/EGL/libegl.c#L111)
将错误设为成功，[current查询](https://github.com/NVIDIA/libglvnd/blob/v1.7.0/src/EGL/libegl.c#L519)
会经过该入口。新增原生负例明确要求捕获 EGL_BAD_SURFACE（0x300d），不能只检查
没有确认或任意错误。纯闭包顺序测试只是辅助，实际驱动故障注入仍为必须门禁。
