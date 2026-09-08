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

已将 RemoteUpdateState、计划与 receipt 状态机提取到 frd-render-wgpu 的 transaction_state 模块，保留根路径公开类型、wgpu 执行与成功确认行为。该模块暂时复用既有 RendererError，尚不是独立于渲染 crate 的公共执行器接口。macOS Metal 下 38 项渲染测试及 1 项文档测试通过，完整工作区 1706 项通过；此提取不改变任何平台能力声明。

下一实现方向：Linux GL 执行器消费同一事务计划。在 GTK current-context 生命周期内管理 texture/FBO，禁止每帧读回CPU。BGRX/BGRA方向与色阶用四角fixture验证，视频后续按原 VideoFrameLayout/VideoColorSelection 采样平面。unrealize/context丢失必须撤销未确认 receipt，并在释放GL资源后请求新完整基线；不能确认旧代帧。

该结论来自固定依赖源码与[GtkGLArea官方文档](https://docs.gtk.org/gtk4/class.GLArea.html)审查，不是GTK/wgpu互操作已实现。独立原生窗口探针仍需CI运行，完整GL执行器及产品接线尚未实现。

### 共享状态 crate 的提交边界

接下来将纯状态迁移到 frd-render-state，只依赖 frd-core/frd-frame。纯 TransactionError
明确映射至原 RendererError；GPU 错误、设备尺寸上限和目标色彩格式留在后端。
不能把现有 PlannedBatch 的 staged_state 直接公开：新 BatchCandidate 独占借用原
RemoteUpdateState，字段私有、不实现 Clone，只暴露只读操作，commit(self) 消费一次，
丢弃候选不修改已安装状态。候选存活期间不可 clear、恢复、再规划或确认回执。

Wgpu 在既有 GpuCleanToken 的 commit_if_unchanged 内消费候选并安装资源；GL 后端
将来必须提供自己的 context/错误代际检查。纯状态 crate 无法证明实际 GPU 执行成功，
不引入可伪造的公共成功 token。ConfirmedPresentation 构造仍由后端控制，禁止用
上传完成或 GTK queue_render 代替真实呈现确认。迁移必须保留全部 GPU 故障与回执
回归，并加入候选丢弃、不可重复/跨实例提交、不可修改操作的运行及编译拒绝测试。
