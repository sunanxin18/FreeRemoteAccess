# 鸿蒙 FFmpeg 后备包构建验证

日期：2026-10-13。系统解码器按精确格式和远程桌面延迟要求优先；当前两台模拟器的系统 AVC 静态帧输出未通过，原因与边界见[系统验证](harmony-system-codec-20261013.md)。本记录覆盖 ARM64 OHOS 后备包构建、模块加载器实现、普通 HAP 包装及两台模拟器应用内合成解码。实际测试进度见文末；尚未注册生产协议或证明真实远程互操作。

## 目标边界

构建器使用官方 API26 Clang/sysroot、Rust `aarch64-unknown-linux-ohos`，固定 FFmpeg 8.1.2 源码归档 SHA-256 `464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`。启用 H.264/HEVC decoder/parser、共享 PIC 与 ARM64 NEON；禁用 GPL/nonfree/version3、网络、程序及 autodetect。固定逻辑 prefix `/usr`，以独立 DESTDIR 暂存；不重命名 Linux 产物冒充鸿蒙。

插件仅在 `target_env=ohos` 选择 `thread_count=1, thread_type=0`，避免 frame-thread 管线需要后续输入才能释放静态帧；Windows、桌面 Linux 与 macOS 原策略保持。该策略保留 NEON，但不消除码流自身的 B 帧重排，也不证明 1440p/4K 吞吐已满足要求。

仅包含三个固定原生库：`libavutil.so.60`、`libavcodec.so.62`、`libfreeremotedesk_ffmpeg.so`。随包保留许可、NOTICE、对应源码取得说明、精确 configure 与 provenance。实际插件链接依赖为上述 codec/util 加公开 NDK `libc.so`；不存在对 GNU libc、宿主库或私有系统 codec 库的依赖。没有修改插件 v1 ABI。

## 验证状态

独立构建与 22 项实际 ELF/输入变异与目标链接测试已执行，独立审查发现无版本链接别名与已验证版本库未绑定，已于 `8343bd3` 改为精确版本链接，修复通过独立复审，没有新的 Critical/Important 问题。构建产物留在忽略的 `target/ffmpeg-ohos/`；生成二进制不入库。不能据 configure 的 NEON 开关宣称实际设备执行了全部 SIMD 热路径。

后续独立检查包括普通签名 HAP 的依赖包装、应用自身模块锚点、不可写安装目录和实际加载身份、ABI/回调生命周期、单帧及最后一帧在无 EOS 时输出、逐像素比较、1440p/4K 性能、协议协商和真实 Windows/Mac 桌面。当前手机/PC 普通包仍为空协议目录。


最终干净构建尝试为 `target/ffmpeg-ohos/attempt-qjsezbh7`，经校验发布到 `bundle-attempt-qjsezbh7`；没有覆盖此前的有效包。库 SHA-256 为：

| 库 | SHA-256 | DT_NEEDED |
| --- | --- | --- |
| libavutil.so.60 | `925ac0829cab1a8a6e246653e4dbf553db4d54e04d44bccc1f908351ffd3255a` | libc.so |
| libavcodec.so.62 | `84ca7851005b3dab87e5f3162d631b30726eab727d60397e10a130ac587b4a55` | libavutil.so.60、libc.so |
| libfreeremotedesk_ffmpeg.so | `4e0f435d4e1fd3a85e53711ec15d30048bdf1c64d310a37d28932336d916cd05` | libavcodec.so.62、libavutil.so.60、libc.so |

默认插件测试 5 项通过；OHOS 禁用 native feature 时，未设置 FFMPEG_DIR/OHOS_SDK_ROOT 的测试二进制成功交叉编译，链接器仍使用官方 SDK，未执行。旧预检目录缺少新来源合同，实际 Cargo 构建会拒绝它。宿主构建日志保留已有 `from_trusted_paths` dead_code 警告，没有把它混称为纯净无警告。没有进行目标解码、SIMD 热路径执行或吞吐基准。

实现提交 `9c2cd42`、精确链接修复 `8343bd3`，独立复审已通过；既有 loader dead_code warning 保留至后续加载器切片处理。最终 manifest SHA-256 `c9515bdaa88c062c231d72ebb22db7d19d4ecabca6463d2da1ac8eb030e973bc`；主代理另行重算三库与 manifest 哈希，一致。当前承诺固定来源和可重复构建步骤，没有以同一最终 recipe 的两轮干净构建证明 bit-identical。


独立审查 I1 的实际复现是：`verify-inputs` 检查带版本名的库，但原 `-lavcodec/-lavutil` 链接读取未检查的无版本别名。修复后 OHOS 使用 `dylib:+verbatim=libavcodec.so.62` / `libavutil.so.60`，桌面保留原链接方式。新增测试在临时 prefix 将 `libavcodec.so` 错指向 util60，并删除 `libavutil.so`，随后实际 OHOS 编译成功，验证精确链接指令、依赖清单及插件 bytes 与正常构建完全相同。它证明别名已不参与该平台链接，不声称畸形输入的所有场景均已覆盖。

## OHOS 应用模块加载器

`4f5089e` 增加 OHOS ARM64 专属加载分支：用私有模块标记的 `dladdr` 定位本应用 Rust 动态库，再从同目录加载三个固定后备库。公开 AbilityKit `OH_AbilityRuntime_ApplicationContextGetBundleCodeDir` 提供应用代码目录边界；不接受调用方路径、环境路径、桌面 Linux 的 executable 路径或私有系统 codec 库。

实现保留目录/文件句柄和 dev/inode，拒绝符号链接及应用可写对象，并检查实际版本入口、getter 和六个 ABI callback 属于预期映射，完成检查后才转换函数指针。加载使用 NOW|LOCAL，句柄按依赖逆序清理。所有权与不可写检查依赖正常系统安装这一前提；`dladdr` 只给出路径和基址，不是映射 inode 或签名证明。

固定 Rust 1.96 下宿主模块 47 项测试通过，覆盖路径/权限拒绝、文件替换、callback/ABI、依赖清退及 decoder/frame 生命周期。官方 OHOS ARM64 rlib 编译无 warning；独立 cdylib 链接探针确认为 ELF64/AArch64，公开 NDK 依赖包含 `libability_runtime.so` 与 `libc.so`。该探针未在设备执行。旧 `from_trusted_paths` warning 通过限定其真实 macOS 调用范围消除，桌面行为保持。

正常 HAP 内的代码目录与实际模块路径是否一致、整个目录链是否满足保守所有权/权限假设，仍待两台模拟器分别确认。调用须在应用上下文建立后进行；当前没有后备库注册、真实出帧或远程互操作验收。独立审查通过，无 Critical/Important；审查记录一项非阻断测试改进：文件替换用例应精确断言身份错误，避免权限拒绝掩盖身份检查退化。正常 HAP 和解码门仍未完成。原始测试与链接产物在 `target/task4c3b-loader-check/`。

## 普通 HAP 包装、签名与安装

普通 Debug/Release HAP 均已构建；构建入口显式接收已验证 bundle 和独立 manifest SHA-256，纳入三个带固定版本名的 FFmpeg 库与许可、来源材料。最终包精确包含桥接库、Rust FFI、SDK C++ 运行库及三项后备库。Hvigor 使用针对后备库的 strip 排除规则；Debug/Release 三库均与上表字节一致。最终 module.json 显式 `extractNativeLibs=true`、`compressNativeLibs=false`。桥接库构建时的 RUNPATH 已去除。

33 项 HAP/暂存检查通过，包含重复/额外/缺失文件、架构/SONAME/依赖错误、损坏字节、许可缺失、签名前后内容比较、暂存符号链接及部分失败清理。实际错误 manifest 构建被拒绝，已有输入 bundle 和已生成 HAP 哈希未改变。受控 ZIP 测试完成后，主代理另行执行真实签名与安装，见下文。

未签名包：Debug `ede7490ca3e03622da1ab2b3ae7f2a15ec5b69acf5cb391686f0e74ee4a3690c`，Release `2a5559a21eca4bc86da03874e930b0c7f09a88335018d870f137dfca1a9addea`，位于 `target/task4c3d-hap-bundle/`。没有通过此次包装注册 codec 或协议；普通包仍为空协议目录。独立审查及修复复审已通过；实际库加载仍为后续门。

同日主代理沿用既有 normal 开发身份执行官方 sign-app/verify-app，随后产品 checker 再次逐项比较 signed/unsigned 六库、八份材料和 module.json，通过。签名 Debug SHA-256 `35c53d0472e3467c7b3e716fb6970778a5e45e21c482f5b4251a128911e31481`；密码仅经关闭回显的交互 console 提交，未进入 argv 或日志。

两台专用模拟器均以该同一 HAP 完成覆盖安装和新进程启动，官方 Emulator CLI UI 树显示 FreeRemoteDesk 登录表单及“此设备尚无可用的远程连接协议”。这只验证安装与启动；此包尚无 loader caller，不证明后备库被实际加载。两台 bm 元数据仍报告 `isCompressNativeLibs=true`/模块 `compressNativeLibs=true`，尽管 HAP 的 module.json 为上述显式配置；不将该字段当成实际提取路径或压缩行为的结论。代码根、版本名库提取与运行映射留待应用内 fixture 验证，未采用 shell 权限绕过。签名、安装、布局和筛选后的非秘密安装元数据保存在 `target/task4c3d-hap-bundle/`。

包装实现 `22513b2` 的独立审查发现 ownership 清单直接写正式路径，半截 JSON 会阻断清理和重试。`dc9419b` 将清单完整写入并关闭私有临时文件后无覆盖发布，失败时只清理本次已发布对象，保留未知清单。10 项定向、38 项范围测试通过，独立复审确认 I1 已解决且无新增阻断项。既有 Hvigor SemVer warning 延后处理。该修复只变更暂存工具与测试，未重新构建 HAP；上列签名安装证据对应 `22513b2` 的产物，不能描述为一次 `dc9419b` 新构建。下一步是普通应用内加载与解码 fixture。

## 正常应用内解码夹具与目录句柄修正

显式 `normal-hap-codec-fixture` 只在 debug 构建的正常应用上下文建立后运行，使用真实 `FfmpegBackend::load`，没有路径注入、网络、凭据或生产协议注册。测试线程自有 decoder/frame 并协作取消；同步原生调用不能安全抢占，缺少完成标记时不能宣称完成或清理。默认和 release 包排除测试入口。

首轮手机和 PC 均在打开祖先根目录时返回 EACCES，根目录模式为 0711；普通应用有路径搜索权限而没有目录读取权限。第二轮通过 `OpenOptions.custom_flags` 添加 `O_PATH` 仍失败。对实际 HAP `.text` 与未剥离中间产物逐字节对应后发现，Rust 1.96 标准库使用 `!O_ACCMODE` 屏蔽自定义标志，而 OHOS/musl 的该常量包含 `O_PATH`，导致真正调用时该标志被清除。[固定版本 Rust 源码](https://github.com/rust-lang/rust/blob/1.96.0/library/std/src/sys/fs/unix.rs#L1273-L1283)与包内反汇编一致；不能将这一轮误称为沙箱拒绝了实际 O_PATH 调用。

修正仅限 OHOS 目录句柄：直接调用公开 `open`，保留 `O_PATH|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`，成功后立即移交 `File`。锚点及三项库仍使用可读句柄；全部祖先的不可写、所有权、类型、符号链接及 dev/inode 重查保持。没有修改系统权限、省略目录或使用私有 namespace。v4 包机器码确认标志完整传至公开 `open`，两台随后通过全部 loader 检查。

v4 同一已签 HAP SHA-256 `725298c852d1867fe002d481204af491c96c7a4e2ebd0a55ddad5376b69b8180` 在手机和 PC 分别运行，均完整通过 10/13 组。256×144 AVC420/444、1440p/4K AVC420 的 Annex-B 和长度前缀输入均在各自 submit 返回时产生一帧，首 IDR 与最终依赖 P 帧无需后续输入或 EOS；像素与独立参考相等。剩余三个失败是 tiny AVC420 两种输入格式的编码/裁剪尺寸契约，以及 HEVC 坏包即时拒绝判定。未将循环完成或部分出帧标记为全组通过。

原始分轮 HAP、签名与包检查日志、两台 hilog、机器码和摘要保留在 `target/task4c3e-runtime/`；合成输入的来源、命令和参考哈希进入夹具 provenance。模拟器合成 submit 耗时不是持续吞吐或远程端到端延迟；H.264 High444 解码不是 RDP AVC444 双流组合验证，HEVC 样本也不是 RDP HEVC wire 支持证明。运行时 SIMD 分派与完整热路径、真机、生产注册及实际 Windows/Mac 互操作仍是独立待验收项。

最终 v5 修正夹具尺寸契约：原 tiny H.264 的 SPS 为 16×16 编码尺寸、右/下各裁剪 14 像素，可见 2×2；保留原输入及 6-byte 参考不变。插件本来禁用自动裁剪，故夹具必须分别传编码尺寸和可见矩形。HEVC 原无效 VCL 在 FFmpeg 下可能被丢弃并返回 NeedMoreData；保留为观察项，不计拒绝成功。单独用无 Annex-B 起始码的确定无效输入检查 typed 错误，再重新创建解码器验证首帧，没有改变 FFmpeg 容错策略。

v5 unsigned SHA-256 `8922f512634b1738aef7d1a39a9d9056ccbbc45fe30e8e952e80bdef8555504b`，signed `555e0464fc62cff8f8df13b3ba7ee0f843493406a1498e96473b7aed5c2791ce`。官方签名、验签及签后包内容比对通过；同一包分别在手机新进程 10463、PC 新进程 13585 运行。两台均 `completed=1 passed=13 failed=0`，包含 tiny/cropped、256×144、2560×1440 和 3840×2160，各组无 EOS 出帧、独立像素比较、重置/旧代际拒绝、重复创建、backend/decoder drop 后帧所有权、确定无效输入拒绝及重建恢复全部通过。总测试约 12.4/12.3 秒包含 debug 逐像素/SHA 核对，不是媒体延迟。

夹具及句柄修正提交 `6a2c91c`。宿主 FFI 38 项通过（另有一个既有 panic 子进程 helper 标记 ignored），加载器 49 项通过；OHOS 默认 debug 与显式 feature release 的实际库均不存在 fixture 日志标记，显式 debug 夹具包保留标记，互斥特性组合被明确拒绝。格式与 diff 检查通过。测试后两台恢复前述 `35c53d04…` 普通签名包，官方 Emulator CLI 确认登录界面及当前无已注册协议的提示。该恢复包是此前普通产物，不冒称新修正的生产构建。

独立任务审查结论为 Spec compliant / Approved，无 Critical/Important；审查只读核对手机/PC 原始日志、签名 HAP 哈希和六项原生库与 checker 一致。既有 Hvigor 配置头资源及 unsigned 阶段缺省 signingConfig 警告作为非阻断项记录，不等同于最终签名失败。

## 官方 profiler 的原包 SIMD 执行证据

同日继续使用上述 v5 已签包和原样接受的三库，在两台正常可调试应用中通过系统 `hiperf` 采样：限定 FreeRemoteDesk 应用、`sw-cpu-clock:u`、1000 Hz、18 秒、8 MiB 上限，不采集调用栈、不使用系统范围模式、不提权或修改 perf 设置。手机/PC 测试仍各 13/13；实际捕获 12135/15656 个 sample，sample 与 non-sample 丢失均为 0。该轮采样不能用于无干扰性能结果。

原构建未剥离 `libavcodec.so.62` 仅作为 profiler 读取的符号数据，未替换应用中的运行库。它与接受包的 `.text` 地址均为 `0x3a380`、大小 1954508 bytes、SHA-256 `6bd09fdf1784aecdba9a84a4dcb691b08f986b6cf34d1506bdecbbd0c97dc580`，已逐字节核对。官方报告把运行 PC 归属到下列原库函数；对应 IDCT 反汇编含向量载入、加减、移位和转置，证明计算核实际使用 NEON。

| 设备 | 正向观测的部分计算核 |
| --- | --- |
| 手机 | `ff_h264_idct_add16intra_neon`、`ff_h264_idct_add_neon`、add8/add16、`ff_put_h264_chroma_mc8_neon` |
| PC | 上述四类 IDCT、`ff_put_pixels16_neon`、`ff_pred16x16_vert_neon`、`ff_pred16x16_hor_neon` |

这是原包 H.264 SIMD 的正向运行证据，范围为混合合成负载；尚不能逐样本归因到 AVC420/444，也不能宣称 HEVC 或全部计算热路径通过。未命中 tiny HEVC 不是没有 SIMD 的证明。报告的 period weight 不是函数调用次数，包含 debug SHA 的全进程百分比也不是 codec 内部占比。原始 perf、日志、官方符号报告、`.text` 与符号区间摘要、具体核反汇编位于 `target/task4c4-runtime/`。下一步只增加分格式有界负载及关闭 profiler 的优化 harness 基准，不修改或插桩 FFmpeg 库。


## 优化应用内基准及分格式运行证据

`f70b381` 增加三个默认关闭、互斥的编译期开关：优化基准、AVC444 采样负载、HEVC444 采样负载。原正确性夹具仍只在 debug 生效；新增测量模式刻意允许 Rust release 优化，但包装为正常可调试 HAP。所有模式保持空产品协议目录，不接收运行时路径或凭据、不启动网络、不改变 NativeFirst 策略、插件 ABI、FFmpeg 原三库或 OHOS 单线程策略。具体界限见 [`MEASUREMENT.md`](../../crates/frd-shell-ohos-ffi/MEASUREMENT.md)。

| 运行产物 | 未签名 SHA-256 | 已签名 SHA-256 |
| --- | --- | --- |
| benchmark | `1cc5341697d6d4f1501edc9f65bceab6e6befcbe2e96d4f1ce68c6e617181b75` | `292d683d95d0439db042f2eac52c18d26a41d508c712cecff2a9399439853cfd` |
| AVC444 profile v2 | `89c4c53355dea68c2e589f02cb59c24d62e38e2020d178c0050eea32d2237f11` | `3ca1b829eedfb76792bd7cc0bb87d4b619ddce279d1ec0a5c430531b24c0e7c1` |
| HEVC444 profile v2 | `8fe512dd31d2d8ec96a652c68e9ba6bc52076c1606025bb828cd749b521aa194` | `adf62f3cab1668fd03e2d32210dc253502b9be16773a37df247a4df6df32b820` |

三包均经未修改的产物检查、官方签名/验签、签后库内容比较。benchmark 产物早于 profile-only 的零重复工作拒绝修复；基准执行分支未变，未将已测包冒称为后续源码的重建。两个 profile v2 已包含该修复。

### 关闭 profiler 的测时

手机 PID10781/TID10906、PC PID11473/TID11592 各通过全部 10 组：256×144 AVC420/444、1440p/4K AVC420 的两种输入 framing，tiny HEVC 与新增 256×144 HEVC。测试前后独立像素/可见平面 SHA 通过，每次 AU 返回恰好一帧，包含最终 P 帧，不发送 EOS。完整 worker 分别 5.709/5.750 秒；期间暂停相关宿主构建和其他基准，但未作 CPU 绑定或频率隔离。

每组冷创建和首序列单列，随后 4 次预热，再三批各 20 次完整序列；每轮 reset 后重新发送 IDR/P。下表是 Annex-B、各 60 个有效样本的 submit median / p95，单位 ms。submit 包含 framing、原生 send/receive 和帧数据复制，排除 AU 创建、校验、日志、reset 与清理；后两项另行测量。

| 样本 / 帧 | 手机 | PC |
| --- | --- | --- |
| AVC420 256×144 / IDR | 0.082 / 0.500 | 0.091 / 1.093 |
| AVC420 256×144 / P | 0.035 / 0.333 | 0.038 / 0.121 |
| AVC444 256×144 / IDR | 0.198 / 2.826 | 0.122 / 0.468 |
| AVC444 256×144 / P | 0.067 / 0.240 | 0.046 / 0.315 |
| AVC420 2560×1440 / IDR | 5.210 / 9.801 | 4.937 / 6.097 |
| AVC420 2560×1440 / P | 4.554 / 10.491 | 4.352 / 6.084 |
| AVC420 3840×2160 / IDR | 13.173 / 15.017 | 13.301 / 14.392 |
| AVC420 3840×2160 / P | 13.155 / 14.020 | 13.408 / 15.260 |
| HEVC444 256×144 / IDR | 0.308 / 0.457 | 0.316 / 0.448 |
| HEVC444 256×144 / P | 0.085 / 0.125 | 0.084 / 0.132 |

每种输入只有固定两帧，重复 reset，不代表长 GOP 或复杂桌面负载。小样本 p95 波动明显，不能用均值或最优轮次掩盖它；模拟器共享宿主，也不是两台独立真机。结果不包含网络、渲染、用户输入往返，不证明真实桌面持续帧率、功耗或硬件解码性能。全部组、两种 framing、冷启动/清理指标见本地 `target/task4c4-runtime/benchmark-statistics.json` 与两个 `*-benchmark-codec.log`。

### 分格式采样与身份边界

AVC444 和 HEVC444 分别在独立进程运行约七秒，重复完整 IDR/P 代际序列，前后像素和无 EOS 检查均通过。官方 profiler 限制本应用、用户态、12 秒、1000 Hz、8 MiB，不提权、不采集栈。初轮 AVC 手机/PC 6583/6642 samples，HEVC 手机/PC 6796/6413 samples，均零丢样本；样本总数不是 SIMD 调用次数。

PC HEVC 初轮虽通过正确性，但 perf 缺失 libavcodec 的 MMAP2，不能归属计算地址。保留该失败证据后，仅以同一 HAP 补跑 PC：PID19698/TID19763、6931 samples、零丢样本、前后校验通过。读取该进程 `/proc/maps` 被拒绝，没有提权；补跑的官方 perf 自身已包含正确映射，随后以本轮映射、worker TID、阶段时间和原库符号对应实际采样指令。未套用旧进程 ASLR 地址。原始数据和报告含 `pc-hevc444-profile-r2` 后缀。

已正向观测 AVC444 与 HEVC444 的计算 SIMD，但**仍有实际标量计算缺口**：H.264 `ff_h264_luma_dc_dequant_idct_8_c`，HEVC 16×16 planar/angular，以及 32×32 angular 中一个实际命中的标量分支。另一些 C 命名函数确实由编译器向量化，不能一概标成标量；也不把 wire/entropy 控制解析列为计算核违规。此结果收窄了后续优化范围，未满足全部生产软件解码计算路径的项目规则；生产 codec 和会话注册仍关闭。H.264 缺口优先，HEVC 按用户要求较后推进。

以上模式的宿主默认 suite 为 31 passed；旧正确性及初版测量 suite 各 38 passed，另有一个既有 panic 子进程 helper 按设计 ignored。profile 零工作拒绝经过先红后绿，最终 8 项测量测试通过；28 项特性配置、OHOS 默认和旧夹具 release 排除、三个显式优化包构建通过。此处记录测试与实测，不替代独立任务审查或整分支审查。

验证后恢复此前普通签名 HAP `35c53d04…`：手机 PID18452，最终 PC PID21584，官方 Emulator CLI 确认登录页和当前无已注册协议的提示。恢复包不是此次源码的生产构建；没有将合成出帧宣称为鸿蒙真实 Windows/Mac 登录。


独立实际指令审计再次核对已签 HAP 三库与原 `.text`，按各自 MMAP2、worker TID、profile 起止单调时钟筛选，再将 sample ELF PC 对应具体机器指令。代表性正向证据：AVC444 手机 `0x3d0b0 sub v5.4h,...`、PC `0x3d0cc trn1 v4.4h,...`；HEVC 手机 residual `0x5da14 sqadd v2.8h,...`、PC r2 transform `0x5ea50 smlal v30.4s,...`。水平插值、滤波和 32×32 planar 的对应向量 PC 也有记录。AVC444 算术滤波与 HEVC SAO 未观测，不据此声称它们不存在或已经优化。

标量缺口也有精确证据：两台 H264 luma DC 命中 `0xe7640 madd w13,w13,w2,w8`；HEVC 16×16 planar 函数没有向量指令，32×32 angular 的混合函数在 PC r2 命中 `0x1f5d00 mul w1,w14,w1`。原样 FFmpeg 在其他架构有相关实现，但不能直接拿 x86/MIPS 指令用于 ARM64。全部选定样本、PT_LOAD 换算、映射、哈希与窄反汇编保留为 `target/task4c4-runtime/profile-pc-evidence.json` 和 `profile-selected-disassembly.txt`。

`f70b381` 独立代码审查为 Spec compliant / Approved，无 Critical/Important。两项 Minor：夹具再生成脚本的 manifest 命名/路径归一化已由 `c9f5217` 修正，独立临时目录完整再生成的 JSON、两个 AU 与两个像素参考均和既有 pin 相同，主代理窄差异复核通过；Hvigor 既有额外源文件打包 warning 留待整包整理。unsigned 阶段 signingConfig warning 与最终官方签名结果分开记录。独立指令审计和代码质量审查职责分离，没有把测试完成标记当作指令覆盖证明。
