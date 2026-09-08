# Linux 客户端基础服务验证（2026-09-08）

## 实现与支持范围

新增 `frd-platform-linux` 独立实现现有平台 API：XDG profile/pin、单实例锁、环境
provider 与 Secret Service 凭据事务。不引用 Windows/macOS 平台 crate，密码不写
普通元数据，缺少或锁定的 Secret Service 明确失败。

Linux 桌面 GPU 选择改为 Vulkan/GLES，相关 wgpu feature 仅在 Linux 目标开启；
Windows 仍选择 DX12，macOS ARM64 仍选择 Metal。此选择没有证明任何 Linux
窗口、GPU 设备或显示服务器已经运行成功。

## 已执行证据

- macOS ARM64 合成目录与注入 keyring：17 项平台服务测试通过。
- 同主机桌面 shell：214 项测试通过，Metal 选择保持。
- 固定 Rust 1.96.0 `cargo check --locked -p frd-platform-linux --all-targets`
  分别对 i686-unknown-linux-gnu、x86_64-unknown-linux-gnu、
  aarch64-unknown-linux-gnu 成功，包含真实 Secret Service 代码编译。
- x86_64-pc-windows-msvc 同命令成功，非 Unix 存储/非 Linux keyring 失败关闭。
- 根代理逐项审查了元数据 fd 相对访问、owner/mode/nlink、原子写与 flock，
  credential stage/commit/discard、DH/locked 分支及测试脚本；没有把审查称为运行证明。
- 格式、diff 检查与 shell 语法检查通过。

## 尚待执行

`tools/verify-linux-secret-service.sh` 在独立 dbus-run-session 与临时 XDG 目录中
运行唯一合成凭据的真实往返，前台 daemon 生命周期由脚本回收，不改 HOME，也不
接入用户已有 keyring。等待服务名最多 10 秒，测试过滤后必须恰有 1 项通过；零测试
不能算通过。脚本已接入 Ubuntu CI，但本次尚未取得运行结果。

新增 Linux 应用入口、原生窗口/输入、完整客户端 staging/verifier、三架构完整包、
真实 RDP 连接与 GUI 尚未完成。此前七目标 FFmpeg/Progressive 记录不能替代这些门禁。

最终完整工作区 `cargo test --locked --workspace`：exit 0，60 组，
1683 passed / 0 failed / 16 ignored。该数量来自 macOS ARM64 运行，不包含 ignored
Linux 原生 Secret Service 测试。

## 应用入口及完整包后续

Linux 应用入口 baebd80 的宿主测试通过 20 项单元及 2 项边界测试。完整工作区首次回归在旧的双平台组合根名单处失败；增加 Linux 的明确许可项，同时把 macOS/Linux 平台服务加入禁止依赖具体协议的检查后，Windows 两项架构回归通过。最终完整工作区 cargo test --locked --workspace 终态 exit 0，62 组、1705 passed / 0 failed / 16 ignored。宿主为 macOS ARM64，不能据此认定 Linux GUI 或 Secret Service 已运行。

完整 Linux stage/verifier 与三架构 CI 已接入实际应用构建、目标测试、ELF 校验、随包 decoder 加载和 tar 产物上传；9 项合成拒绝路径测试、shell 语法、YAML 三目标结构及格式检查通过。新流程尚未在 Linux 执行；完整包、窗口、GPU、输入及真实 RDP 门禁继续保持未验收。

## Linux 原生 Secret Service 与应用测试

2026-09-08 提交 `5cd73ea`，CI run `34198594600` 的 Ubuntu job `101972000994` 已终态 success。日志确认隔离 D-Bus / 临时 GNOME keyring 中 `secure_credentials::native_tests::native_secret_service_authenticated_commit_roundtrip` 实际执行并通过：1 passed / 0 failed / 0 ignored（不是默认忽略项）。该测试覆盖认证前仅暂存、认证提交后的新 store 读取及删除；使用固定合成密码，未访问用户真实凭据。运行日志保存在本机 `/tmp/frd-linux-native-services-5cd.log`。

同一 Ubuntu 原生 job 中 Linux 应用 20 项单元和 2 项依赖边界测试通过，完整工作区编译及安全核心测试通过。范围为 Ubuntu x86_64 的临时 keyring，不是全部 Linux 发行版、锁定 keyring 的交互解锁体验或实际远程登录证明。三架构完整客户端包和原生窗口仍独立待验收。

## 完整包首次目标运行失败与修正

`5cd73ea` Linux run `34198594626` 的 ARM64 job `101971931726` 与 x86_64 job `101971932074` 均完成实际 release 客户端编译，但 verifier 因未审核依赖 `libz.so.1` 失败；不能标为完整包通过。`cargo tree --locked -p freeremotedesk-linux --target aarch64-unknown-linux-gnu -i libz-sys` 确认依赖来自 flate2，经 Apple 协议与 IronRDP SSPI 引入。已将标准 zlib SONAME 纳入明确系统依赖名单，未知库继续拒绝；新增回归后合成包测试 10 项通过。修正后的目标流程尚未重跑。

独立 `tools/frd-linux-native-shell-probe` 与三架构 X11/Wayland CI 验证流程已实现，尚未在 Linux 编译/运行。探针只验证 GTK 原生 HeaderBar 与 GLArea 合成路径；有界 pass 要求实际 backend、mapped、独立内容 allocation、中心位置、viewport 与逻辑尺寸/缩放一致及无 GL错误。CI显式使用软件 Mesa，因此硬件 GPU、鼠标键盘、视觉验收和产品 RDP 集成仍不在通过范围。脚本及工作流 shell/YAML 语法已检查。

## 首个完整 Linux 客户端包通过：i686

2026-09-08 `5cd73ea` 的 Linux i686 job `101971932014` 已终态 success。同一32位目标进程通过应用20项单元及2项边界测试；完整 release 客户端、ELF/资源/权限校验、显式 `FRD_LINUX_PACKAGE_RUNTIME_SMOKE=1` 下加载器检查及真实 `--verify-codec-bundle` 均通过。完整 tar 包上传为 artifact `10045359087`（freeremotedesk-client-linux-x86），对应 FFmpeg source artifact `10045362850`。日志 `/tmp/frd-linux-client-i686-5cd.log`。

此证明是 x86_64 Linux 宿主运行真实 i686 进程，不是 i686 GUI、真实输入或 RDP 控制。整个 run `34198594626` 仍为 failure，因为同次 ARM64/x86_64 verifier 拒绝 zlib 依赖；不得用单个目标成功覆盖其余失败。

已下载上述 tar 产物并独立检查：28,838,880 字节，SHA-256 `253dfa97fb5504bc30f841df799d1593d5c5ac7ac4b466ebec1422ddc86427f8`，44 个归档条目；实际客户端为 ELF32/Intel80386、权限0755。未在 macOS 执行该 Linux ELF。

原生窗口探针新增独立 Xvfb 输入驱动及严格 JSONL 校验：仅操作本次 PID/标题匹配的窗口，用实际焦点上的 XTEST 点击/F8，并要求鼠标、按键配对、内容焦点和坐标比例成立；Wayland 不允许宣称输入通过。14 项合成报告回归及脚本语法检查通过。新输入驱动尚未在 Linux 实际运行，不能把这些合成测试算作 GUI 输入证据。

## 共享会话宿主分离

将 SessionHost/启动 barrier/取消/媒体工作线程/帧事务和清理从 application.rs 提取至 session_host.rs，未引入 GTK 或改变平台窗口。既有根 API 保留，新增 AcceptedLaunchOutcome 根重导出，供后续原生壳匹配后台启动结果。生产状态保持私有；跨模块呈现测试仅用 cfg(test) 窄接口。主代理比对启动、取消、事件发送、清理、回滚及视频/帧方法体，保持原逻辑。

全部既有 shell214项及新增公开API doctest通过；最终完整 cargo test --locked --workspace 终态exit0，62组1706 passed/0 failed/16 ignored。该证据来自macOS ARM64宿主，不代表GTK产品接线或Linux原生GUI完成。

## a60eebd 原生包与窗口探针证据

Linux run `34202134581` 的 ARM64 job `101983147418` 与 x86_64 job `101983147612` 已成功，均实际通过应用20+2测试、完整release构建、包静态校验、显式目标进程解码器加载及产物上传。此前 zlib 名单遗漏已在目标环境复验关闭。同轮 i686 job `101983147659` 随后也成功完成应用20+2测试、release包静态校验、目标进程解码加载与上传；run `34202134581` 三目标均终态成功。日志分别为 `/tmp/frd-linux-arm64-a60.log`、`/tmp/frd-linux-x64-a60.log`、`/tmp/frd-linux-i686-a60.log`。

原生窗口 run `34202134605` 三架构均实际编译成功，在首个 X11 1× 报告校验处失败。原始报告记录 ARM64/x86_64各89帧、i686为91帧，零GL错误；各有配对鼠标/键盘事件，内容焦点及窗口活跃，标题栏操作零次。实际标题栏 bounds 为 `(-1,-1,952,47)`，内容为 `(0,46,950,584)`，左右各外扩1逻辑点且底边恰好相接。旧 verifier 不允许负原点，造成错误拒绝。

新增实测fixture先触发旧规则失败；修正规则只允许固定探针主题中最多1逻辑点的对称标题栏外扩，内容保持窗口左边对齐，并保留接缝、DPI、焦点、配对及无标题栏误点击条件。16项回归通过，下载的三份 `x11-1.jsonl` 均通过 `--require-input` 离线复验。原 run 仍是失败；此复验不补充未执行的2×、Wayland、任意主题或完整产品GUI验收。GL renderer实际是软件llvmpipe，不是硬件GPU证明。

基础 CI run `34202134493` Ubuntu成功；macOS完成编译与安全测试后，在获取 pinned EGFX 的 arbitrary 依赖时因 `index.crates.io` DNS超时失败。确认终态后仅重跑失败job；attempt2 的 macOS job `101986081337` 已终态成功，原来未执行的 pinned EGFX 与 macOS 平台/应用测试均实际通过。整个基础 CI 已成功，没有修改锁文件或跳过测试。

三份同轮完整包已下载到 `target/validation/linux-a60-{x86,x86_64,aarch64}`，逐一只读核对
归档没有绝对/父级路径或链接，各44项，客户端权限0755。ELF class/machine分别是
1/3、2/62、2/183；未在macOS执行Linux程序。解码器实际加载证据来自前述Linux jobs。

| 架构 | Artifact ID | tar.gz字节数 | SHA-256 |
| --- | --- | ---: | --- |
| i686 | 10046637877 | 28847515 | `93ed27b643d64ed2c413a6725a446068ad1ce9a5ec0e9ccffe7ba21c2c58031c` |
| x86_64 | 10046581491 | 28474112 | `7a18da90250b02e9fe8be27fdb709c3fcceb5e7c5383b81b09488e6bbb81bf91` |
| ARM64 | 10046569301 | 28075993 | `1944b2aeb764adb6936bb9bb35b9821858493c0d80ab00cf4a7a3ae0e3d05160` |

## 共享帧事务状态的完整迁移

新增 frd-render-state，依赖树仅 frd-core/frd-frame。原公开回执/身份类型在
frd-render-wgpu 根路径继续重导出，纯 TransactionError 穷尽映射到既有 RendererError。
BatchCandidate 独占借用原状态，私有字段、只读操作、无Clone，commit(self)只消费一次；
丢弃候选保持原状态。GPU clean gate和ConfirmedPresentation继续由后端负责。

原38项渲染测试保留为28项后端测试及10项迁移状态测试；新增3项候选生命周期测试、
7项编译拒绝测试全部通过。根代理逐一比对9个规划/提交/校验方法体，除错误类型名称
替换外逻辑不变。Metal后端28项及既有doctest通过，Windows x64渲染crate目标check通过。
纯状态crate另通过Windows i686/ARM64及Linux i686/x86_64/ARM64目标check；这些是编译证据。
完整工作区终态exit0：64组、1717 passed / 0 failed / 16 ignored，日志
`/tmp/frd-render-state-workspace.log`；fmt与diff检查通过。该迁移不代表Linux GL执行器、
GTK产品呈现或RDP实机验证已经完成。


## 独立 GL 执行器（原生结果待 CI）

新增 frd-render-gl：真实纹理分配、完整/局部上传、sRGB BGRX shader 绘制、内容矩形
与目标附件验证、宿主 GL 状态恢复、上下文失效隔离及延迟资源删除。后端仅在 Linux
三种目标架构启用；Windows 和 macOS 保持原有后端。生产路径没有像素读回或 CPU 转色。

首次集成完整工作区测试在 macOS ARM64 上通过：1720 passed / 0 failed / 16 ignored，
日志 `/tmp/frd-gl-workspace.log`。其中新 crate 的3项为纯逻辑测试；Linux 原生测试在
该宿主为0项，不能据此宣称 GL 执行通过。Linux 三目标编译检查由实现代理执行通过。

新增独立三架构软件 Mesa EGL CI，要求明确执行且仅执行1项原生 fixture；覆盖颜色、
alpha、行方向、stride、黑边清理、错误 viewport、真实 current 解绑、恢复和资源释放。
该测试仅为离屏 GL 执行验证，不代表硬件 GPU、GTK 窗口呈现或实际 RDP 控制。审查后
生命周期修正的最终验证及 CI 结果另行记录。


最终审查修正：非 current detach 也先撤销旧回执、退休资源，再决定是否可删除；绘制
只修改/恢复 draw-buffer 0 的混合和颜色掩码，scissor 按上下文能力隔离 index 0，
拒绝启用多个 draw buffer 的目标，避免清理宿主其他颜色附件。对应原生 fixture 已补充，
仍待 Linux CI 实际执行。修正后 macOS 3项纯逻辑测试及格式/差异检查通过；这些检查
不覆盖被 Linux cfg 门控的 GL 实现运行。


## 09565d3 原生 CI 首轮结果与待重跑修正

GL run `34206959440` 已结束，整体失败。x86_64 job `101998580384`、AArch64 job
`101998580745` 均实际执行 native_egl_renderer_roundtrip：各1 passed / 0 failed /
0 ignored。软件 Mesa EGL 的颜色、上传、绘制和生命周期 fixture 首次取得两架构运行
证据；不升级为硬件或 GTK 产品呈现证明。i686 job `101998580665` 在链接阶段因
Scrt1.o、crti.o 和 libdl 开发链接文件缺失而失败，尚未执行 fixture。CI 补充与
cross GCC 配套的 libc6-dev-i386-cross，修正本身待下一轮运行验证。

窗口探针 run `34206959337` 的 x86_64 job `101998582995`、AArch64 job
`101998583020` 均失败，但两个架构的 X11 1×/2× 均已通过真实输入和 GL/几何验证。
主代理下载原始报告并独立用相同严格 verifier 复验这四份结果通过。
x86_64 Wayland 1× 也通过；2× 实际报告 scale=1，verifier 正确拒绝。
AArch64 Wayland 1× 的 stdout 混入 AT-SPI Registry daemon 文本，严格 JSON 解析失败；
不将其原始报告改写为通过。修正方向是 compositor 实际输出缩放和隔离 probe 报告
与 D-Bus 后台服务日志，保留原有缩放/JSON 验收条件。

原始 artifacts 保留在 target/validation/native-shell-095-x64 与
native-shell-095-arm64；GL 作业日志为 /tmp/frd-gl-095-{x64,arm64,i686}.log。


同轮 i686 窗口 job `101998582705` 最终也通过 X11 1×/2× 及 Wayland 1×，在 Wayland
2× 实际 scale=1 处失败。整个窗口 run 已结束为失败。后续 workflow 改为每个 scale
启动独立 Weston，使用实际输出 --scale，分别保留 compositor 日志；probe stdout
在 D-Bus session 内独立写入 JSONL，服务 stdout 另存。verifier 未放宽。两个修正
workflow 的 YAML/Bash 语法及 diff 检查通过；尚未推送重跑，等待当前打包 CI 完成。


09565d3 基础 run `34206959364` 已成功结束：格式 job101998579977、Ubuntu
job101998659618、macOS job101998659780 通过。已下载两宿主日志到
/tmp/frd-base-095-linux.log 与 /tmp/frd-base-095-mac.log；macOS pinned EGFX 21项、
平台24项（2ignored）、应用18项和依赖边界2项均明确执行通过。该 run 不包含后续
未推送的 GTK 目标诊断，也不改变前述原生 GL/窗口失败边界。


### 下一版 GTK 目标观察的验收范围

新增 render 入口观察只记录固定枚举、尺寸和计数，不包含对象名称、地址、像素或输入
文本。确认上下文身份，再按真实附件类型查询；GL4.5+ 使用 DSA 取得纹理 target，
旧版无法安全确认的纹理保留未知哨兵。严格 verifier 独立重算 compatible，线性附件
或 renderbuffer 可成为成功的“不兼容”观察，不能因此宣称可直接接入 GL renderer。
查询失败、字段矛盾或伪造兼容声明仍拒绝。

该 schema 新增必需的 target_observation，旧 a60/095 原始报告只能用其对应版本
verifier 复验，不具备目标格式证据。新的 C 查询尚未在 Linux 编译/执行，待后续 CI。
OpenGL 查询版本依据 [Khronos 官方参考源码](https://github.com/KhronosGroup/OpenGL-Refpages/blob/main/gl4/glGetTexParameter.xml)：GL_TEXTURE_TARGET 仅4.5及以后可用。


09565d3 Linux 完整包 run34206959387 已成功，x86_64 job101998580604、AArch64
job101998580848、i686 job101998580911 的日志均明确记录完整包静态验证和目标进程
解码器加载通过。macOS ARM64 run34206959528/job101998580553 也已成功，日志明确
记录包结构、签名和 CLI 验证通过。原始日志 /tmp/frd-package-095-{linux-x64,
linux-arm64,linux-i686,mac}.log。Windows 同轮仍运行中；不扩大为 GUI、实际AVC或
后续未推送改动的验证。


GL 独立审查补充修正：indexed viewport只操作0，所有clip-distance启用位在绘制期间
关闭并恢复，非二维附件绑定失败立即处理自身GL错误并恢复宿主绑定。新增原生fixture
覆盖不同viewport1值、clip0启用仍颜色正确和cube拒绝后有效capture立即成功。
最低GL3.3保持。修正后三目标 --tests 编译检查、macOS纯逻辑3项、fmt/diff通过，
新增原生断言尚待下一轮CI。GTK目标观察严格报告测试最终20项通过，C实现待Linux。


共享帧接口已公开，原 drain/retire 方法体保持一致。新增外部 integration test 使用
真实空 SessionHost 验证提取、指标、重复空提取和无效退休；不访问平台服务。shell
215项单元测试、新外部测试1项及原doctest1项通过，日志
/tmp/frd-public-frame-api-tests.log。此接口接线准备不代表 GTK 客户端已运行。


打包工作流并发策略调整：Windows/Linux/macOS同组保留正在运行的完整验证，最新提交
排队；原生GL/窗口探针保持独立并发组，可立即验证修正。依据[GitHub官方并发语义](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#concurrency)，cancel-in-progress=false不取消运行中的任务，默认仅保留最新pending。此配置不修改产品或验收条件。


347a79e 原生 GL run34209182139 三架构全部成功：x64 job102005830534、i686
job102005830626、ARM64 job102005830776 各明确执行1 passed/0 failed/0 ignored。
日志 /tmp/frd-gl-347-{x64,i686,arm64}.log，包含新增viewport/clip/cube恢复断言。
这是软件 EGL 原生执行，仍不代表硬件GPU/GTK呈现/生产RDP控制。


347a79e GTK ARM64 job102005830614成功：X11/Wayland各1×/2×，X11含真实输入，
Wayland仅GL/几何。已下载 target/validation/native-shell-347-arm64。四份目标记录均为
GL4.5core、TEXTURE_2D、RGBA8(32856)、LINEAR(9729)、samples0、query_errors0、
compatible0。Wayland2×实际viewport1920×1186对应960×593逻辑面积。目标对象每帧
更换计数与观察次数相差1，进一步支持每次render重新capture而非缓存FBO。
该测量明确阻止当前sRGB-only接口直接接入；下一步必须核对GTK纹理颜色语义，再
实现明确的GPU输出转换或中间合成，不得把LINEAR附件谎报为sRGB。


347a79e 原生窗口 run34209182174 最终三架构全部成功：x64 job102005830389、i686
job102005830604、ARM64 job102005830614。下载三架构各X11/Wayland1×/2×共12份
报告，并用该版本严格verifier逐份复验通过。全部目标均RGBA8/LINEAR、compatible0；
Wayland2×真实缩放及ARM64日志隔离修正取得原生证据。X11包含输入，Wayland仅
GL/几何；不宣称完整产品登录、键盘焦点全覆盖或硬件GPU通过。artifact目录
 target/validation/native-shell-347-{x64,i686,arm64}。


新增显式SrgbEncodedRgba8输出模式，默认sRGB capture不变。新模式只接受明确宿主
声明的RGBA8/LINEAR/2D/level0/单采样目标，shader在线性过滤后重新编码sRGB字节，
没有CPU整帧转色。新增原生测试覆盖两目标输出对照、暗灰分段、2×缩放、alpha/X、
状态恢复及格式/mip/多采样拒绝。macOS3项纯逻辑和fmt/diff通过，原生断言待CI；
此实现不表示GTK产品接线或实际窗口最终颜色已通过。


7bf2415 原生GL run34210290196首轮三架构全部失败，日志
/tmp/frd-gl-7bf-{x64,i686,arm64}.log。三个进程均在native_egl.rs:214的新mip负例
构造处发现FRAMEBUFFER_INCOMPLETE_ATTACHMENT(36054)，未到契约拒绝断言。
level1与base0尺寸配置不一致，需修复测试纹理base/max层配置，保留完整性断言和
生产level0限制。首轮结果保留为失败，不以编译或前段颜色检查代替完整原生通过。
