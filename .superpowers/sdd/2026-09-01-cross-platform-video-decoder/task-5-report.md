# Task 5 报告：固定 LGPL FFmpeg 8.1.2 与 Main444 fixture software decode

日期：2026-09-01

状态：完成；software HEVC Main444 8-bit fixture decode 为 GREEN。

提交：`feat: decode HEVC Main444 with FFmpeg fallback`（本报告随 Task 5 文件一起提交；最终 hash 由提交结果给出）。

基线：`dfc5227`（Task 4 hardened ABI/plugin loader）。

## 1. FFmpeg 来源、签名与哈希

固定版本为官方 FFmpeg `8.1.2`，`libavcodec` major `62`。构建脚本只声明以下官方地址：

- `https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz`
- `https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz.asc`
- `https://ffmpeg.org/ffmpeg-devel.asc`

验证结果：

- source archive SHA-256：`464BEB5E7BF0C311E68B45AE2F04E9CC2AF88851ABB4082231742A74D97B524C`
- detached signature SHA-256：`0A0963FCCD70597838073F3E31B20F4A4D8CC2B5E577472C9A5A1F22624246F8`
- release public key SHA-256：`397B3BECEDCD5A98769967FF1FF8501DDC89F8368B8F766E4701377D7DBAABE5`
- 要求的主密钥 fingerprint：`FCF986EA15E6E293A5644F10B4322F04D67658D8`
- `gpg --batch --no-autostart --status-fd 1 --verify ...` 返回 `GOODSIG` 和 `VALIDSIG FCF986EA15E6E293A5644F10B4322F04D67658D8`；签名时间为 2026-06-17 02:48:59 UTC（本地 +08:00 为 10:48:59）。

脚本在进入解压/configure 前同时强制校验 archive hash、精确 fingerprint 和 `VALIDSIG`。Git for Windows 的 GPG 在 PowerShell 中会尝试通过 `/usr/bin/gpg-agent` 启动 agent；公共密钥验证不需要 agent，因此脚本使用 `--no-autostart`，并以 fingerprint + `VALIDSIG` 作为不可省略的成功门禁。

空缓存时脚本从上述 ffmpeg.org URL 下载。本次本地 source cache 最初因 ffmpeg.org archive 传输长期停滞而由 XMission 的 FFmpeg release mirror 预填；该文件与官方发布物 hash 完全一致，并由 ffmpeg.org 发布公钥与 detached signature 验证为同一官方签名内容。此镜像只用于被忽略的本地缓存，不出现在构建脚本或产品元数据中。

对应源代码构建输入位于本地被忽略的 `.codex-target/ffmpeg-8.1.2/source-cache/ffmpeg-8.1.2.tar.xz`；解压树现在只存在于每次唯一 attempt 中并在构建后清理。本提交不包含 DLL 或构建树，并保存 FFmpeg 原始 `LICENSE.LGPLv2.1` 与“无源码修改”的 `changes.diff`。第 9 节给出 distributor-controlled 对应源码发布要求。

## 2. 构建命令、配置与工具 provenance

执行：

```powershell
pwsh -NoProfile -File tools/build-ffmpeg-windows.ps1 -Configuration Release -ForceRebuild
pwsh -NoProfile -File tools/build-ffmpeg-windows.ps1 -Configuration Release
```

第一次命令从删除并重建的配置目录完成 source configure/compile/install；在修正生成头文件门禁后，第二条及后续命令均完整通过签名、配置、import library、plugin 与 bundle hash 阶段。固定 Release configure 为：

```text
./configure '--prefix=<DIST_DIR>' '--arch=x86_64' '--target-os=mingw32' '--cross-prefix=x86_64-w64-mingw32-' '--disable-static' '--enable-shared' '--disable-programs' '--disable-doc' '--disable-everything' '--enable-decoder=hevc' '--enable-parser=hevc' '--enable-protocol=file' '--disable-gpl' '--disable-nonfree' '--disable-version3' '--disable-autodetect' '--disable-network' '--disable-x86asm' '--disable-debug' '--enable-stripping'
```

configure 摘要为 `License: LGPL version 2.1 or later`，external libraries 与 hardware accelerators 均为空，只启用 HEVC decoder、HEVC parser、file protocol。脚本还检查 `CONFIG_GPL=0`、`CONFIG_NONFREE=0`、`CONFIG_HEVC_DECODER=1`、`CONFIG_HEVC_PARSER=1`、`CONFIG_FILE_PROTOCOL=1`。没有链接 x264、x265、fdk-aac 或其他外部 codec library；x265 只作为一次性的 fixture authoring CLI，位于被忽略目录，不参与 FFmpeg/plugin 构建或运行时。

已用工具均为已有、workspace-local/ignored 输出所调用，没有安装服务、服务器组件或持久后台程序：

- WSL distribution：`ubuntu24.04`
- Bash：`5.2.21(1)-release`
- GNU Make：`4.3`
- MinGW-w64 GCC：`13-win32`
- Visual Studio 2022 Community：installation `17.14.37614.0`
- MSVC x64 compiler：`19.44.35228.0`

MinGW 生成共享 LGPL DLL/`.def`，Visual Studio `lib.exe` 从 exact `.def` 生成 MSVC import libraries。最终 bundle 只复制 `avcodec-62.dll`、`avutil-60.dll`、`freeremotedesk_ffmpeg.dll`。PE import 检查结果：native plugin 只新增 `avcodec-62.dll` 与 `avutil-60.dll`；`avcodec-62.dll` 只依赖 `avutil-60.dll` 和 Windows CRT/system DLL。

## 3. RED → GREEN

先提交测试与 synthetic fixture，再继续 decoder callbacks。RED 命令：

```powershell
cargo test --locked -p frd-video-ffmpeg-plugin --test main444_decode -- --nocapture
```

RED 在 Task 4 unavailable plugin 上按预期失败：`native plugin 必须提供完整 API 表`，实际 `FrdStatus(-1)`，期望 `FrdStatus(0)`。没有用 fake callback 或放宽 loader/ABI 使其通过。

实现 MSVC C shim、真实 `avcodec_send_packet`/`avcodec_receive_frame`、Rust ownership/copy/validation 后，同一命令 GREEN：2 passed（fixture decode 与 out-of-contract ABI negative paths）。真实输出为 `Yuv444P8`、3 planes、每 plane 16×16、tight stride 16、256 bytes，timestamp `90000`，三个 checksum 与 JSON 完全一致。

## 4. Fixture provenance 与隐私检查

fixture `apple-main444-idr.hevc` 并非 ARD/桌面捕获；它是本 workspace 内生成的完全合成空白画面，因此无需接触用户名、桌面、IP、凭据或个人内容。生成输入是两个相同的 16×16 `YUV444P8` frame：Y 全为 `0x10`，U/V 全为 `0x80`；使用两个 frame 是为了得到一般 `Main 4:4:4` profile，而不是 Still Picture profile。使用的临时 authoring tool 为 Ubuntu `x265 3.5-2build1`（encoder `3.5+1-f0c1022b6`），`.deb` SHA-256 为 `A8ADE8F8DDBE253674A663B7DBA7FA7CCCC247D9ADA43FB59536FB6BCEEC91C8`，位于被忽略的 `.codex-target/fixture-tools/`。生成控制包括 i444/8-bit、16×16、lossless、固定单线程/no pools、无 AUD、无 info、固定帧数；只提取第一个 random-access AU，并把 start code 规范化为四字节。

提交 fixture SHA-256：`C6C7AC14B6DCE19582FBABA901030EB62904E382CC0809BA72E063FED56F45C0`，长度 544 bytes。NAL 顺序严格为 `32(VPS), 33(SPS), 34(PPS), 20(IDR_N_LP)`，没有 SEI。`strings -a -n 4` 只产生非语义字节串 `uGTuGTu`；解码回放确认平面仍为统一空白值。JSON 只含 codec/profile/chroma/bit-depth、coded/visible size、frame count/stride 与 plane SHA-256，不含时间戳、机器、用户或网络字段。

平面 SHA-256：

- Y：`35854e72e7083b583ea6597960452e80ca5ac358c4cb90f93eb5374f6e5904cf`
- U：`5a5f307aa9ce504d9235634f15cf382e8914c49fbd8dd4d4c47136c917886f7b`
- V：`5a5f307aa9ce504d9235634f15cf382e8914c49fbd8dd4d4c47136c917886f7b`

## 5. Decoder、ABI 与 ownership

因 Windows 环境没有可用 libclang，而 `ffmpeg-the-third` 的 native path 强依赖 bindgen，经主任务批准改用狭窄 MSVC C shim：FFmpeg headers/structs 全部留在 `ffmpeg_bridge.c`；Rust 侧只见自有、固定大小的 `FrdNativeFrameView` 与 opaque pointer。代价是需要随固定 libavcodec 62 维护这层小型 C bridge，当前 native build 明确限于 Windows MSVC。删除了 `ffmpeg-the-third`/bindgen 依赖及其 lockfile transitive packages。

Task 4 hardened raw ABI 未改变、未放宽 production trust：

- size-qualified `RawFrdFfmpegApiV1`、ABI version、avcodec major、alignment 与全部 required contract flags 原样发布；callbacks 仅在 exact native prerequisites 存在时返回。
- `create` 总是先清零 output；成功 handle 由 plugin `Box` 与 native context 独占，允许跨线程移动但调用必须 serialized；`destroy` 在 plugin 内 exactly once 释放。
- `submit` 将 caller Annex-B bytes 复制进 owned `AVPacket`，正确处理 send `EAGAIN`：先 bounded drain，再重试未消费 packet 一次。
- `receive` 在每个 status 上先发布 zero frame；成功时只接受 exact `AV_PIX_FMT_YUV444P` 与 config 尺寸，拒绝负 linesize/短 stride/null data，再逐行复制到三个 plugin-owned tight planes；不做 CPU BGRA conversion。
- `flush` 用 null packet，遇到 `EAGAIN` bounded drain 后重试。
- `reclaim_frame` 对 zero、partial、success/error receive output 均可调用，遍历所有 plane 并在 plugin allocator 内释放后清零。单元测试覆盖 partial ownership；Task 4 loader tests 覆盖 error/validation partial reclaim。
- 配置只接受 HEVC Main444 8-bit、YUV444、Annex-B；dimension 必须为 1..8192，timebase 为 1..`INT_MAX`。每个 access unit ≤64 MiB、parameter set ≤1 MiB；单帧三 plane aggregate ≤256 MiB，最多排队 8 frames 且 queued aggregate ≤256 MiB。所有拒绝映射为稳定 `INVALID_ARGUMENT`、`UNSUPPORTED` 或 `DECODE_FAILED`。

Direct-load fixture test 只从被忽略的 dev bundle 绝对路径加载 DLL，用于绕开 production install ACL 的开发测试；产品 loader 的 system-owned install ACL、absolute path 与 trusted DLL policy 没有 test seam 或 relaxation。

## 6. 验证结果

- 基线（实现前）：`cargo test -p frd-video-ffmpeg -p frd-video-ffmpeg-plugin` → loader 22 passed，default plugin 1 passed。
- Release build script：通过 source hash、GPG fingerprint/signature、LGPL configure gates、shared FFmpeg build、MSVC C shim/plugin link 与 bundle hash。
- `cargo test --locked -p frd-video-ffmpeg-plugin --features native-ffmpeg --lib -- --nocapture` → 6 passed（negative linesize、non-YUV444P、size mismatch、dimension/timebase、queue count/bytes、partial reclaim）。
- `cargo test --locked -p frd-video-ffmpeg-plugin --test main444_decode -- --nocapture` → 2 passed（real fixture + ABI invalid inputs）。
- 临时将 `.codex-target/.../Release/codec` 移出后执行 `cargo test --locked -p frd-video-ffmpeg` → 22 passed；bundle 已恢复。missing plugin 仍为 backend unavailable，production trust tests 全部通过。
- `cargo test --locked -p frd-video-ffmpeg-plugin --lib` → default/native-disabled 1 passed并失败关闭。
- `cargo build --locked --workspace` → 通过，包含 `freeremotedesk-windows` GUI；只有既存 root MVS dead-code warnings。
- `cargo tree --locked -p frd-video-ffmpeg-plugin --edges normal` → default graph 无 FFmpeg wrapper/native library。
- PE import：default `target/debug/freeremotedesk_ffmpeg.dll` 与 `target/debug/freeremotedesk-windows.exe` 均无 `avcodec`/`avutil` import；native release plugin 有且仅有动态 FFmpeg imports。
- `cargo fmt --all -- --check` → 通过。
- `git diff --check` → 通过。

## 7. Task 5 文件

- `Cargo.lock`
- `crates/frd-video-ffmpeg-plugin/Cargo.toml`
- `crates/frd-video-ffmpeg-plugin/build.rs`
- `crates/frd-video-ffmpeg-plugin/src/decoder.rs`
- `crates/frd-video-ffmpeg-plugin/src/ffmpeg_bridge.c`
- `crates/frd-video-ffmpeg-plugin/src/lib.rs`
- `crates/frd-video-ffmpeg-plugin/tests/main444_decode.rs`
- `crates/frd-video-ffmpeg-plugin/tests/fixtures/apple-main444-idr.hevc`
- `crates/frd-video-ffmpeg-plugin/tests/fixtures/apple-main444-idr.json`
- `third_party/ffmpeg/8.1.2/README.md`
- `third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1`
- `third_party/ffmpeg/8.1.2/configure-windows.txt`
- `third_party/ffmpeg/8.1.2/changes.diff`
- `tools/build-ffmpeg-windows.ps1`
- 本报告。

未编辑、未 stage、未提交已有 dirty Apple HP/README/root CLI 文件。

## 8. 剩余关注点与声明边界

- 本任务证明 fixed FFmpeg software HEVC Main444 8-bit 对一个 synthetic exact fixture 的解码；不证明 FFmpeg hardware exact、不证明 Apple live stream/transport framing，也不扩大当前 capability 声明。
- native bundle 不提交，production 安装仍需后续 packaging 在满足 LGPL 对应源码义务的同时设置 Task 4 要求的 system-owned ACL；开发目录 direct-load 不能作为 release readiness 证据。
- source/build tree 和 portable fixture authoring tool 都在 ignored output。空缓存构建需要网络可访问 ffmpeg.org，并要求已有 WSL MinGW 与 Visual Studio C++ toolchain；脚本不会安装它们或启动持久服务。
- FFmpeg DLL 的本地 build hash 会受 PE/build timestamp 影响，因此只记录在 ignored provenance/output；可复现门禁是 exact signed source、固定 configure、固定当前工具 provenance 与 fixture checksum，而不是提交 DLL hash。

## 9. 修复轮 1/5：构建来源、bundle 与对应源码分发加固

日期：2026-09-01。针对 review 的 4 个 Important finding 完成修复；本节取代第 1、2、6 节中关于复用解压树、仅依赖 upstream URL 以及手工准备 native test 环境的旧描述。修复提交信息为 `fix: harden FFmpeg build and source packaging`，最终 commit hash 随提交结果返回。

### 9.1 4 个 finding 的处置

1. `tools/build-ffmpeg-windows.ps1` 不再读取稳定 `source`、`dist` 或 `configure-command.txt` cache。每次普通 build 都在持有 configuration lock 时重新校验 archive hash、固定 GPG fingerprint 与 `VALIDSIG`，再把 archive 解压到唯一 `.attempt-<pid>-<guid>`，无条件重新 configure/build/install。旧脚本遗留的稳定源码/build/configure tree 会被删除，attempt 在 lock 释放前清理。`-ForceRebuild` 只额外清理中断遗留物，不是获得可信 fresh build 的前提。
2. codec bundle 先在空的同级 staging directory 构造，文件集合必须精确等于 `avcodec-62.dll`、`avutil-60.dll`、`freeremotedesk_ffmpeg.dll`；再用 `dumpbin /dependents` 检查三个文件的精确 PE import set。只有 staging 的文件集/import 全部通过后，才通过同卷 directory rename 可恢复地替换当前 bundle；发布后再次验证 exact set/import。旧 bundle 中的 stale/GPL/nonfree/unrelated DLL 不会被保留，含 `libx265.dll` 等未批准文件的 staging 会在替换前失败。
3. 对应源码不再以 upstream URL 或开发机 ignored 目录作为发布完成证据。固定的分发 asset 是 `FreeRemoteDesk-ffmpeg-8.1.2-corresponding-source.zip`，精确包含 `ffmpeg-8.1.2.tar.xz`、detached signature、`LICENSE.LGPLv2.1`、`changes.diff`、`SOURCE-MANIFEST.txt`。build 校验 archive/signature 输入 hash、ZIP exact member set 和 ZIP 内 archive/signature hash。本次 staging asset SHA-256 为 `35A47C42B3B47C87A7E0F7296B23E268EAC1E296135C9D7AED7C5B796B70E305`。Task 9 必须由 FreeRemoteDesk 分发者把这个固定 ZIP 作为每个 Windows binary package 同一 distributor-controlled release/download 页面的 sibling asset 发布，并让 binary package/installer 明确指向它；`.codex-target/.../release-assets` 只是 ignored staging，不是合规分发位置。
4. build script 新增 `-RunNativeTests`。它在同一个 fresh `pwsh -NoProfile` 进程中通过 `VsDevCmd` 初始化 `cl.exe`/`lib.exe`，把当前 unique attempt 的 `dist` 设为 `FFMPEG_DIR`，把已验证 codec bundle 加入 process `PATH`，然后运行 native unit test 和 fixture integration test；调用者无需预设这三个环境条件。没有为开发目录放宽 Task 4 production install ACL/trust policy。

共享、可测试的文件系统门禁位于 `tools/ffmpeg-build-common.psm1`；覆盖测试位于 `tools/tests/ffmpeg-build-common.Tests.ps1`。MSVC C shim、raw ABI、thread/ownership/reclaim/bounded-output 合同没有改变。

### 9.2 RED → GREEN

先写四项脚本安全测试，再改 build 实现。初始 RED 命令：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "Invoke-Pester -Script 'tools\tests\ffmpeg-build-common.Tests.ps1' -EnableExit"
```

初始实现不存在时，tampered extraction、stale bundle replacement 与 corresponding-source staging 三项失败；unapproved bundle 测试因“函数不存在也抛异常”的测试写法产生一次 false positive。切换到仓库可用的 Pester/pwsh 并收紧该断言后，中间结果为 2 passed / 2 failed，分别暴露 Pester 3 `Should Throw` 兼容性与 ZIP 排序预期问题。修正测试本身后，最终 GREEN 命令与实际摘要：

```powershell
pwsh -NoProfile -Command "Import-Module Pester -Force; Invoke-Pester -Script 'tools\tests\ffmpeg-build-common.Tests.ps1' -EnableExit"
```

结果：`Passed: 4 Failed: 0`（最终复跑 786 ms）。四项分别验证：已污染的 extracted tree 被 signed-archive fresh extraction 完全替换；旧 bundle 的 `stale-gpl.dll` 被移除且只剩精确三文件；含 `libx265.dll` 的 staging 在替换前拒绝且当前 bundle 保留；对应源码 ZIP 的 exact members、manifest package/location/hash 字段正确。

### 9.3 完整构建、污染注入与自包含 native 测试

构建前在旧稳定 source tree 写入 `TAMPERED-SOURCE-MARKER.txt`，并在旧 codec bundle 写入 `stale-gpl.dll`，然后从未预设 `FFMPEG_DIR`、VS developer environment 或 runtime DLL `PATH` 的新 PowerShell 运行文档入口：

```powershell
pwsh -NoProfile -File tools/build-ffmpeg-windows.ps1 -Configuration Release -RunNativeTests
```

命令 exit 0。实际构建摘要：source hash/fingerprint/`VALIDSIG` 通过；`source_extraction=fresh-per-build`；configure 为第 2 节所列固定 Release LGPL line；license 为 `LGPL version 2.1 or later`，enabled decoder/parser 为 HEVC，external libraries/hwaccels 为空；shared FFmpeg、MSVC import libraries 与 C shim/plugin 构建通过；exact codec/import gate 通过。旧 source marker 和 `stale-gpl.dll` 均为 `False`，最终 bundle 只有 3 个批准 DLL。脚本内测试结果为 native `6 passed; 0 failed`，fixture `2 passed; 0 failed`。对应源码 ZIP 生成并校验通过。

构建产物检查命令调用 `Assert-ExactDirectoryFiles`、`Assert-CorrespondingSourcePackage` 与 PowerShell AST parser；实际摘要为：

```text
ARTIFACT_GATE_OK exact_codec=3 tampered_marker=false stale_gpl=false source_package=valid ast=valid
```

build provenance 记录了 exact official URLs、三个输入 hash、固定 release fingerprint、`signature=VALID`、fresh extraction、完整 configure line、tool versions、exact codec set/import gate、对应源码 asset path/hash。没有提交源码 ZIP、DLL、attempt/build tree 或 portable build cache。

### 9.4 回归矩阵

- `pwsh -NoProfile -Command "Import-Module Pester -Force; Invoke-Pester -Script 'tools\tests\ffmpeg-build-common.Tests.ps1' -EnableExit"` → 4 passed，0 failed。
- `pwsh -NoProfile -File tools/build-ffmpeg-windows.ps1 -Configuration Release -RunNativeTests` → build exit 0；native 6 passed；fixture 2 passed；exact bundle/source-package gates 通过。
- 将 `.codex-target/.../Release/codec` 同卷临时改名，在 `finally` 恢复，然后运行 `cargo test --locked -p frd-video-ffmpeg` → loader 22 passed，0 failed；missing plugin 仍 fail-closed，Task 4 trust/ACL/ABI/reclaim tests 未放宽。
- `cargo test --locked -p frd-video-ffmpeg-plugin --lib` → default/native-disabled 1 passed，0 failed。
- `cargo build --locked --workspace` → 通过，包括 default GUI；只有既存 root MVS dead-code warnings，没有 FFmpeg import/link requirement。
- `cargo fmt --all -- --check` → 通过。
- `git diff --check` → 通过；仅提示 Windows working copy 未来会把 build script LF 转为 CRLF，无 whitespace error。

### 9.5 修复文件与剩余关注点

- `tools/build-ffmpeg-windows.ps1`
- `tools/ffmpeg-build-common.psm1`
- `tools/tests/ffmpeg-build-common.Tests.ps1`
- `third_party/ffmpeg/8.1.2/README.md`
- 本报告。

仍需 Task 9 真正把固定对应源码 ZIP 与每个 Windows binary package 发布在同一 distributor-controlled release/download location，并配置 Task 4 system-owned install ACL；本任务只生成并验证 release asset staging，不能声称 packaging 已完成。软件 exact 的证据仍只限 synthetic HEVC Main444 8-bit fixture，不代表 hardware exact、Apple live transport framing 或普遍流兼容性。既有 Apple HP/README/root CLI dirty files 未编辑、未 stage、未提交。
