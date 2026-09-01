# 统一视频解码器验收记录（2026-09-01）

## 结论

本轮在 Windows 11 主机、提交 `a2c7617` 上完成编译、全量核心回归、Windows
native capability probe、固定 FFmpeg Main444 fixture、DX12 YUV444 离屏颜色门禁、
Windows package staging/verifier，以及 codec present/absent 的单实例 GUI 启动检查。

统一视频解码器的离线/本机范围当前只能标记为 **受限验证**，Task 10 的最终验收则为
**BLOCKED**：离线解码、渲染和 package staging
均有直接证据，但 Apple High Performance 未完成本轮真机认证、RTP、FFmpeg、当前
generation 精确 present 与 Ready 闭环；Apple Standard 的独立尝试终止于
`apple_connection_failed`；RDP 没有独立真实 Windows server。本记录不把任一缺口由
另一 Apple 模式、离线 fixture、编译或既有历史证据补齐。

## 验收身份与不可混合边界

| 产品选择 | runtime identity | encoding profile | 本轮结论 |
|---|---|---|---|
| Apple Standard/MVS | `apple-hpss-mvs` | `AppleTcpMvs` | 独立尝试，未到首帧/Ready；不得由 HP 或历史 MVS 证据替代 |
| Apple High Performance | `apple-high-performance` | `AppleUdpMedia` | environment-provider auto-connect 已发起真实会话，稳定终止于 `apple_high_performance_unavailable`；不得自动回退 Standard/MVS |
| RDP | 独立 RDP identity | RDP adapter | 离线回归通过；无真实 Windows server，未真机验证 |

产品 Ready 与输入开放仍只接受已 admission 的 current-generation surface 的精确
`FramePresented(FullBaseline)`。认证、transport active、config/AU publication、decode、
upload 或 redraw 单独都不构成 Ready。

## 环境与构建身份

- OS：Windows NT `10.0.26200.0`。
- Rust：`rustc 1.96.0 (ac68faa20 2026-05-25)`；
  `cargo 1.96.0 (30a34c682 2026-05-25)`。
- GPU probe adapter：`AMD Radeon 780M Graphics`；renderer backend：DX12。
- Release GUI：`target/release/freeremotedesk-windows.exe`，42,611,712 bytes，
  SHA-256 `472302C58B5D4F018042A6A24F1F593C869779599EEF062AA1D61D01674A97C3`。
- FFmpeg package contract：FFmpeg `8.1.2`、libavcodec major `62`、固定
  `codecs/ffmpeg-8.1.2/windows-x86_64`；staged manifest 同时含 build provenance hash
  与 corresponding-source asset hash。

以上非敏感构建身份由下列命令直接取得；命令输出未写入生成物或提交：

```powershell
[Environment]::OSVersion.Version.ToString()
rustc --version
cargo --version
cargo run -p frd-video-capabilities -- --json
$exe = Get-Item -LiteralPath target/release/freeremotedesk-windows.exe
$exe.Length
Get-FileHash -Algorithm SHA256 -LiteralPath $exe.FullName
$manifest = Get-Content -Raw target/package-task10/ffmpeg-manifest.json | ConvertFrom-Json
$manifest | Select-Object ffmpegVersion,libavcodecMajor,codecDirectory,buildProvenanceSha256,correspondingSource,files
Get-ChildItem -File target/package-task10/codecs/ffmpeg-8.1.2/windows-x86_64 |
  Get-FileHash -Algorithm SHA256
```

GPU adapter/profile 取自上述 capability probe 的 JSON，而非通用设备枚举；DX12 backend
取自本记录后述 GPU readback 测试输出。Release build 的“通过”由精确 build 命令与 exit `0`
支持；未保留可审计的 stopwatch artifact，因此不再保留一次性构建耗时数值。

## 编译与核心回归

以下命令均在同一 task worktree 上直接执行并返回 exit `0`：

```powershell
cargo fmt --all -- --check
cargo test -p frd-media-api
cargo test -p frd-platform-windows
cargo test -p frd-video-ffmpeg
cargo test -p frd-video-ffmpeg-plugin
cargo test -p frd-render-wgpu
cargo test -p frd-compositor-wgpu
cargo test -p frd-shell-desktop
cargo test -p frd-protocol-apple
cargo test -p frd-protocol-rdp
cargo build --release -p freeremotedesk-windows
```

结果摘要：

| crate/gate | 结果 |
|---|---|
| `frd-media-api` | 23 passed；doc tests 0 |
| `frd-platform-windows` | 30 passed；doc tests 0 |
| `frd-video-ffmpeg` | 22 passed；doc tests 0 |
| `frd-video-ffmpeg-plugin` | 1 unit + 2 native fixture passed；doc tests 0 |
| `frd-render-wgpu` | 40 unit + 1 compile-fail doc test passed |
| `frd-compositor-wgpu` | 13 passed |
| `frd-shell-desktop` | 130 passed；测试刻意触发并捕获 decoder boundary panic，无失败 |
| `frd-protocol-apple` | 422 passed、9 ignored；auth 4、session 1 passed |
| `frd-protocol-rdp` | 110 passed |
| Windows Release GUI | build passed；未保留 stopwatch artifact，不报告一次性耗时 |

未观察到本变更引入的新 warning。编译与测试只证明静态/离线门禁，不证明真机互操作。

依赖边界另以 `cargo tree -p frd-media-api --edges normal` 检查：该 crate 只依赖
`frd-core` 与 `frd-frame`，不依赖 Apple、RDP、DirectX、FFmpeg 或 wgpu。Apple 与 RDP
dependency tree 对 `frd-platform`、`frd-render`、`frd-video-ffmpeg`、`wgpu` 的匹配均为 0。

## 离线 fixture 与 registry 证据

### FFmpeg Main444 fixture

```powershell
cargo test -p frd-video-ffmpeg-plugin --test main444_decode -- --nocapture
```

结果：2 passed。固定 synthetic HEVC Main444 8-bit Annex-B fixture 被 FFmpeg software
backend 解码为三个独立 `Yuv444P8` planes；每 plane 为 16×16、stride 16、256 bytes，
没有 CPU BGRA 整帧副本。fixture 是本地生成的统一空白画面，不含桌面、网络或凭据内容。

### capability/registry

`frd-media-api` 与 shell 回归证明：Main/Main10 不能匹配 Main444；registry 只选择
`DecoderReady` factory；fallback 只发生在 decoder 创建前的 exact candidate 之间；一旦
选定或提交 runtime 数据，不会在运行中切到 FFmpeg、Standard/MVS 或其他协议。FFmpeg
缺失只形成稳定 `BackendUnavailable`，不会阻止 GUI 进程启动。

## Windows 本机 probe

```powershell
cargo run -p frd-video-capabilities -- --json
```

实际 probe 为 `ok`，availability 为 `probe_only`，枚举一个非软件 adapter：

| query | 结果 |
|---|---|
| HEVC Main / YUV420 / 8-bit / NV12 | `hardware_exact` |
| HEVC Main10 / YUV420 / 10-bit / P010 | `hardware_exact` |
| HEVC Main444 / YUV444 / 8-bit / planar YUV444P8 | `profile_unavailable` |

因此 Windows native probe 不能为 Apple Main444 提供 decoder factory，且不能用 Main/Main10
4:2:0 能力冒充 Main444。结构化证据仍位于
[`windows-video-capabilities-20260901.json`](windows-video-capabilities-20260901.json)。

## DX12 离屏颜色与 present 前置门禁

```powershell
cargo test -p frd-render-wgpu \
  video_texture_gpu_readback_converts_red_green_blue_gray_and_applies_visible_crop \
  -- --nocapture
```

真实 DX12 readback：

```text
[[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [140, 139, 140, 255]]
```

该 4×4 fixture 同时覆盖显式 BT.709 limited-range 转换、sRGB presentation、独立 plane
stride 与 visible crop。renderer/shell 回归还证明 stale identity/generation 在 upload 前拒绝，
只有真实 present 返回的精确 receipt 才确认原始 `VideoFrameToken` 并发布
`FramePresented(FullBaseline)`。

## Windows package staging

```powershell
pwsh -NoProfile -File tools/stage-windows-package.ps1 \
  -PackageRoot target/package-task10
pwsh -NoProfile -File tools/verify-windows-package.ps1 \
  -PackageRoot target/package-task10
pwsh -NoProfile -Command \
  "Import-Module Pester -Force; Invoke-Pester -Script 'tools/tests/windows-package.Tests.ps1' -EnableExit"
```

stager 与独立 verifier 均通过。Pester 6/6 通过，覆盖 exact package、package-root approved
DLL shadow、未批准 runtime DLL、current-directory shadow、staged byte tamper，以及非隐藏的
对应源码 ZIP。staged codec 目录仅包含 `avcodec-62.dll`、`avutil-60.dll` 与
`freeremotedesk_ffmpeg.dll`；manifest、LGPL、notice、固定 build provenance 与对应源码 hash
均存在。

manifest 身份与 hash 的读取命令见“环境与构建身份”；独立 verifier 会重新计算 staged bytes
并与 manifest 比较，而不是只检查字段存在。`target/package-task10` 是本地生成目录，未提交。

这是可验证的 binary staging directory，不是 MSI/MSIX，也不是 system-owned install ACL
正向证明。GitHub-hosted workflow 本轮未执行。

## GUI codec-present / codec-absent

启动前均确认系统中不存在 FreeRemoteDesk GUI。每轮只启动 package 中一个精确 PID，观察
连接页后只关闭该 PID并确认退出；codec-absent 检查通过包内受控目录改名完成，并在
`finally` 等价的恢复步骤中恢复目录。

| 条件 | 结果 | PID 纪律 |
|---|---|---|
| codec present | GUI 窗口与连接表单正常出现 | 唯一 PID 已关闭，随后无残留 GUI |
| codec absent | GUI 窗口与连接表单正常出现 | 唯一 PID 已关闭，codec 目录已恢复 |

两个登录页都没有可见 FFmpeg/backend 状态。生产 loader 只在真实 `VideoConfig` 到达 decoder
worker 后惰性加载；因此“codec present GUI 启动”不能证明 `DecoderReady`，也不能证明 staged
user-writable directory 满足 production trusted-install ACL。缺 codec 不阻止 GUI 启动这一门禁
已通过；codec-present backend 状态仍需可信安装上下文加真实 HP config 才能观察。

## Apple Standard 真机

授权 stock Mac 的 TCP/5900 在运行前可达。Apple Standard/MVS 使用独立
`apple-hpss-mvs` / `AppleTcpMvs` 身份启动，未选择或回退 High Performance。该有界尝试从
保存的本地 profile 发起，但在首帧/Ready 前终止，GUI 只提供稳定
`apple_connection_failed`。本轮没有可证明的认证阶段细分、MVS full baseline、连续更新、
鼠标、键盘、滚轮、颜色、比例或断开互操作结果。

既有 MVS fixture/历史真机证据保持原范围，但不能替代当前 reviewed runtime 的独立
Standard end-to-end 成功，所以 README 不提升 Standard 状态。

## Apple High Performance 真机

High Performance 以独立 `apple-high-performance` / `AppleUdpMedia` 产品选择在全新 GUI
PID 中通过 CLI auto-connect 发起。私有 PowerShell wrapper 在单一进程内从 gitignored
凭据源读取地址、用户名和密码，只把用户名/密码设置到子进程 environment，地址参数也只由
运行时变量构造；命令、输出、artifact 与本文均不含目标身份或 credential material。CLI
使用 environment username/password provider 与 `--connect`，没有 GUI 密码注入，也没有
Standard/MVS fallback。

```powershell
pwsh -NoProfile -File target/task10-private-hp.ps1
```

wrapper 先确认无既有 GUI，再启动一个精确 PID；真实 auto-connect 稳定显示
`apple_high_performance_unavailable`。当前 typed product error 同时覆盖严格 SRP offer/加密
门禁与确认期内未取得可接受 virtual-display ServerState 等 fail-closed 分支，生产 UI 没有再
公开更窄子阶段，因此本文不把它臆测成认证成功或某一个 ServerState 分支。捕获时 metrics CSV
为 0 event rows、0 `PhaseBoundary`、0 `Presentation`、0 process samples；随后只关闭该记录 PID，
确认退出、process environment 清空且 GUI 进程为 0。精确证据边界为：

- 未证明本轮 HP 认证；
- 未证明 authenticated RTP；
- 未形成可归属于本轮的 Main444 config/AU；
- 未证明 FFmpeg production backend 被选择；
- 未证明 current-generation exact present 或 Ready；
- 未验证连续更新、输入、颜色、比例、stale-generation 拒绝或干净断开。

历史 mode-7、SRTP/RTP 与 SPS 证据仍是相邻的受限真机/离线证据，不能冒充本轮统一 decoder
闭环。HP 保持 **开发中**，也没有自动降级为 Standard/MVS。

## RDP 真机

`frd-protocol-rdp` 110 项离线回归通过，但本轮没有独立真实 Windows RDP server。RDP 登录、
首帧、输入、颜色、比例、动态分辨率和持续更新均明确为 **未真机验证**，状态保持
**开发中**。

## 60 秒性能预算

本轮真实 HP auto-connect 没有满足“current-generation exact present/Ready 且持续更新”，
因此 `tools/run-frame-metrics.ps1` 未进入由 `FramePresented(FullBaseline)` 启动的
`VisibleWarmup`，也没有运行会被误解为 HP 性能证明的 60 秒窗口。根据 Task 10 条件门禁，
这使最终验收为 **BLOCKED**；不以 idle GUI 数值代替。现有 frame metrics sink 能观察通用 frame batch、
mailbox age、process CPU、working set、frame response 与 input-to-next-present，但没有生产字段
直接记录下列 Task 10 指标：

| 指标 | 本轮可观察性 |
|---|---|
| AU queue 峰值 | 不可观察；仅有 64 entry / 32 MiB 有界策略与离线回归 |
| AU 丢弃计数 | 不可观察；无生产导出计数 |
| decode p50/p95 | 不可观察；无生产 decode latency histogram |
| decoded→present p50/p95 | 不可观察；无对应分段 timestamp/histogram |
| frame queue 峰值 | 不可观察；latest-frame slot 仅有离线行为回归 |
| CPU/内存 | 通用采集器可观察，但没有真实 HP session；未采集并不伪造 idle GUI 数值 |

因此本轮只能证明队列上限的代码与测试门禁，不能声明 60 秒无单调积压或任何固定毫秒 SLA。
不为制造 telemetry 而扩大 production scope。

## 最终差异与凭据审计

执行：

```powershell
git diff --check
git status --short
git diff --name-only HEAD~10..HEAD
git grep -n -E "<brief 指定的 secret/private-IP/credential-assignment patterns>" -- . \
  ":(exclude)CREDENTIALS.local.md"
```

`git diff --check` 通过。`HEAD~10..HEAD` 只覆盖 Tasks 8/9 已审阅的视频、Apple、shell、
package 与 README 路径；dependency tree 未发现 protocol → platform/renderer 反向依赖。
Task 10 暂存区只有本文件与精确 README hunk；`hpss.rs`、root `src/main.rs` 和
`apple-dual-mode-blockers-20260901.md` 仍为未暂存 WIP。

指定 `git grep` 在 Task 10 开始前的 `HEAD` 已有 10 个命中：README 的既有私网示例、
两个设计/计划文档中的审计 pattern，以及 root CLI 的既有私网测试/示例。为避免泄露，
本记录只保留文件与 pattern 分类，不复制匹配行。Task 10 暂存内容没有新增命中位置或
pattern 类，Task 10 新文档与报告的同组安全扫描为 0。由于不能改动指定的既有/脏 WIP，
“全工作树 0 命中”门禁按原样无法成立，作为已知 concern 保留。

## 最终状态语言自审

- Windows native probe：**受限验证**，仅当前单机 capability。
- FFmpeg Main444 software backend：**受限验证**，仅固定离线 fixture、DX12 离屏路径与
  package staging；不是 Apple HP live-first-frame。
- Windows package：**受限验证**，仅 staging/verifier，不是 installer/CI release。
- Apple Standard/MVS：**开发中**，本轮独立 live attempt 未通过。
- Apple High Performance：**开发中**，本轮真实 environment-provider auto-connect 稳定返回
  `apple_high_performance_unavailable`；更窄内部阶段未公开，Ready 闭环未证明。
- RDP：**开发中**，无真实 Windows server。
- 其他客户端平台 native backend：**计划中**。

没有任何状态通过自动 fallback、相邻协议、编译、fixture 或历史样本被提升为真机成功。
