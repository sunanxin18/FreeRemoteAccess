# 桌面默认配方托管回归

2026-10-14。所有运行固定在 `37ca646e015834ee98e14c4587c3f48d01283180`，来源为已推送的 `codex/rdp-egfx-hevc-preference` 分支。各架构结果分别记录；此记录不是之后 `6543e07` observer 或未提交 rich coverage 的回归证据，也不是真实远程登录或GUI交互验收。

| 工作流 | 实际完成范围 | 结果 |
|---|---|---|
| [Windows38452186370](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38452186370) | i686/x86_64/ARM64包构建、验证与来源产物；x86、x64目标fixture/既有codec回归；独立原生Windows ARM64加载同run下载包、包验证及decoder/既有codec回归 | 全部成功 |
| [Mac38452184265](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38452184265) | 仅macOS ARM64包与对应源码材料 | 成功 |
| [Linux38452184404](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38452184404) | upstream i686/x86_64/AArch64三库、完整客户端包、相应target步骤及源码材料 | 三架构成功 |
| [GTK38452184156](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38452184156) | i686/x86_64/AArch64；独立X11正式入口与X11/Wayland adapter | 三架构成功 |
| [GL38452184224](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38452184224) | i686/x86_64/AArch64 GL renderer目标工作流 | 三架构成功 |
| [CI38452184218](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38452184218) | 格式、Ubuntu24.04/macOS15安全构建测试 | 成功 |

root取得每个run的匹配headSha、完整jobs/steps状态与artifact索引。Windows ARM64独立job `115500666860` 使用 `windows-11-arm`、固定原生Rust1.96.0及 `aarch64-pc-windows-msvc` 目标，验证并加载同run的ARM64包。实际日志记录6/6 packaged Main444解码fixture；ClearCodec/NSCodec39项通过（2项基准明确ignored，随后独立运行2项），Progressive61项通过（1项基准ignored，随后独立运行1项）。没有将ignored项目算成普通测试通过，也没有将target构建当成运行结果。

Mac/GTK/GL/Linux每条成功范围以对应工作流的实际步骤为边界；host支持和离线fixture不证明任意远端、窗口交互、硬解、完整软件decoder资格或本轮Windows新H264核接入。macOS Intel不属于产品目标，本轮没有该job。

单独显式修改配方 [Linux38452572725](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38452572725) **失败**：native builder/bundle与6项解码fixture已通过，但qualification误拒正常SONAME安装别名，未执行到同对象数值/ABI/基准及完整修改包。默认upstream三架构成功不能替代此失败门，详见[Linux专项记录](linux-h264-recipe-20261014.md)。Windows修改配方也尚未实现。

匹配状态、artifact索引、失败artifact、专项日志与完整Windows ARM64 native日志保存在 `target/ci-h264-linux-20261014/`。没有改默认GH账号、remote、系统签名配置或服务器，也没有在这些运行中输入真实远端账号密码。

## 后续冻结 6e075c4 托管回归

2026-10-14重新观察同一批实际运行，全部成功结束：Windows `38467005504` 三架构包、macOS ARM64 `38467005270`、默认 upstream Linux 三架构 `38467005318`、GTK 三架构 `38467005498` 与通用 CI `38467005418`。各 run 的 head 均为 `6e075c48b5f47965af7c38123b95324e53196daf`；本轮没有新触发 GL workflow，不将历史 GL 证据记作该 head 的回归。

Windows ARM64 原生 job `115550527102` 完整149,424-byte日志保存于 `target/ci-h264-linux-gcc-20261014/windows-native-job.log`：原生 Rust 1.96 / `aarch64-pc-windows-msvc` 加载同 run 下载的 upstream 包，Main444 fixture 6项、ClearCodec/NSCodec 39项与独立运行的2项基准、Progressive 61项与独立运行的1项基准全部通过。该 head 尚不含 `2015400` 的 native factory 策略或 Windows 新 kernel helper；不能作为后续源码的验证。Linux 显式修改配方 `38467007932` 的独立成功范围见[专项记录](linux-h264-recipe-20261014.md)。

## 后续冻结 ccd4ad5 托管回归

2026-10-14，匹配 `ccd4ad5aeb10cee8c47fe8acfe0a4c47982640c0` 的五条实际工作流均成功结束：

| 工作流 | 本轮实际完成范围 |
|---|---|
| [Windows38477613709](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38477613709) | i686、x86_64、ARM64 包及独立 Windows ARM64 原生解码运行 |
| [Mac38477613802](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38477613802) | macOS ARM64 包；无 Intel job |
| [Linux38477613812](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38477613812) | upstream i686、x86_64、AArch64 三架构客户端与 FFmpeg 包 |
| [GTK38477613904](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38477613904) | i686、x86_64、AArch64 adapter |
| [CI38477613902](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38477613902) | 格式及 Ubuntu/macOS 安全构建测试 |

Windows ARM64 原生 job `115584007364` 使用原生 Rust 1.96 / `aarch64-pc-windows-msvc` 和同 run 下载的默认 upstream 包。实际日志确认 Main444 6项、ClearCodec/NSCodec 39项及随后单独运行的2项基准、Progressive 61项及随后单独运行的1项基准均通过。完整日志221,697 bytes，SHA256 `4ee6c8bdb60567c5fde7a78f2e947c5d2e1911c49122a291311257d9d867c321`。

全部 run/head/jobs 状态、完整原生日志和摘要保存在 `target/ci-native-factory-and-windows-identity-20261014/`。此 head 含 `2015400` 显式 native factory 和已审查 Windows 核身份接口；本轮仍使用默认 upstream Windows 包，未执行 Windows 修改配方的私有 ABI/数值资格，不证明 RequiredNative 所有热路径、待提交整管线测试或普通鸿蒙 HAP 的生产注册与真实登录。本轮没有重新触发 GL，也没有重复已通过且来源链未变的 Linux 显式修改配方资格。日志保留 CI 的 Node.js 20 action 被托管环境转为 Node.js 24 执行的弃用警告，未将成功状态描述为无警告。

## 冻结 f475cf2 桌面回归

2026-10-14，匹配 `f475cf28ec6fcc65fd8c17984ee88b23a4a16fcd` 的四条实际工作流
成功结束：Windows38513518674（三架构包与独立ARM64原生运行）、macOS38513518793
（仅ARM64）、Linux38513518716（三架构默认包）和通用CI38513518664。此次没有
对应GTK/GL工作流，不将旧head结果混入。原始状态保存于
`target/desktop-ci-f475cf2-20261014/`。

Windows ARM64原生job115696620466加载同run的默认包；原始日志223683bytes，
SHA-256 `58ffb9b7081bf2a989f0b4ebc606fe46f5ef09d0dc0dee64973a7a07f5c710db`。
日志确认ClearCodec/NSCodec39项通过（2项ignored后独立执行通过），Progressive61项
通过（1项ignored后独立执行通过）；包解码夹具步骤成功。这是既有默认包/子系统
回归，不是新Windows私有ABI/数值资格，亦非HEVC私有内核生产注册或鸿蒙真实登录。
后续122f44e的新专项编译结果另见Windows私有平台记录，两个head的证据独立。

## 冻结 122f44e 的回归与运行器失败

2026-10-14，通用 CI `38516856108` 对提交
`122f44e9a1c691a723cdb0229d7aa40b41ae123b` 成功结束。Windows 包工作流
`38516856165` 的第 1 次尝试失败：x86、ARM64 包任务成功，x64 任务
`115698712834` 在准备 Ubuntu 24.04 WSL 环境时失去托管运行器通信，尚未进入
FFmpeg 或客户端编译。GitHub check annotation 明确报告 hosted runner lost
communication；不能据此断言具体宿主资源或网络根因，也不能把它记作源码回归。
该失败使依赖的 Windows ARM64 原生解码任务被跳过，本提交尚无完整 Windows
回归通过证据（第 1 次尝试的状态）。

原始 run/job/annotation JSON 保存于
`target/desktop-ci-122f44e-20261014/`；读取失败日志返回 `log not found`，没有用
其它任务的日志替代。仅对同一 run 发起失败任务重试，命令终态 exit 0，记录于
`rerun-failed-receipt.json`；重试期间仍保留上述失败和跳过状态。

后续同一 run 的第 2 次尝试成功结束：x64 包任务 `115716170441` 完成，三架构
包及依赖的 ARM64 原生任务 `115727145778` 均为 success，head 仍为上述
122f44e。该原生任务加载同次默认包，ClearCodec/NSCodec 39 项通过，2 项
ignored 随后独立执行通过；Progressive 61 项通过，1 项 ignored 随后独立
执行通过，包解码夹具步骤成功。完整原生日志 224305 bytes，SHA-256 为
`e5b1eeb9517860f77158c19f9984520b1c92fcb7fa2d475d56808f0843754780`。
第 2 次尝试 run JSON 与原始日志保存为 `windows-run-attempt2.json` 和
`windows-native-attempt2.log`，没有覆盖首次失败证据。

这补齐了本提交的既有 Windows 回归；上一提交 f475cf2 的记录及本提交 Windows
私有对象专项结果仍各自保持验证范围。未提交的 C contract 驱动和色彩优化
不属于本次源码，仍无它们的 Windows 原生或完整产品验证。

## 冻结 8ea234d 与 f556b7b 的后续状态

2026-10-14，提交 `8ea234d7cbd0b4ba5a41c3da673ab7c38924aee6` 的
[通用CI38529778194](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38529778194)、
[macOS38529778107](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38529778107)、
[Linux38529778152](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38529778152)
与 [Windows38529778085](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38529778085)
均实际结束为 success。它们是该 head 的既有构建/包回归，不替代新色彩候选的
OHOS 运行、完整性能资格或真实会话。

下一提交 `f556b7b493d292a6e6c9b68c6a7776ed94246efc` 的
[Windows38531352483](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38531352483)
也成功结束：三架构包及原生 ARM64 job115767634407 全部 success；完整日志确认
同 run 默认包 Main444 6 项、ClearCodec/NSCodec 39 项与独立 2 项基准、
Progressive 61 项与独立 1 项基准通过。原生日志 SHA-256 为
`0627346f6c36cdb4316a77679a2316135ef0c4236400fbc1a6c67f7ba39de7ec`。
这仍是默认配方回归，不是 Windows 修改配方私有 C/ABI/SEH 资格。

该 head 的 [通用CI38531352370](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38531352370)
失败：格式与 Ubuntu 安全测试通过；macos-15 job115743825354 在 Apple crate
`production_session_disconnect_interrupts_blocked_writer_and_runs_cleanup` 的
“写入失败”字符串断言处失败，该 crate 为 507 passed、1 failed、9 ignored。
实际错误链未打印，根因未知；不能记作无害调度波动，也没有通过重试覆盖失败。
诊断源码切片仅为保留原条件并打印完整错误链，不修改生产代码或 3 秒清理边界。
当前没有修复通过的执行结果。

诊断切片经独立源码审查后，宿主固定 Rust1.96.0 单次聚焦运行实际通过：
目标 unit test 精确 1 passed、0 failed/ignored，测试报告 0.90 s。
调用使用 `rustup run 1.96.0-aarch64-apple-darwin cargo test --offline --locked`
及原测试名；首次直接 cargo 的 `+toolchain` 入口不受支持，在编译前 exit101，
该入口失败和修正后的实际运行分别保留于
`target/apple-disconnect-diagnostic-20261014/`。此单次通过未复现 hosted macOS
并发失败，仍不证明根因已修复；新提交的实际 CI 结果需另行取得。

原始 Windows run/jobs 和两条完整日志保存于本地
`target/desktop-ci-f556b7b-20261014/`；macOS 失败日志 SHA-256 为
`57d2a79f9e654ff896097081f0151b85a9b10b121f8ddcc03107016728cc5b75`。
同 head 的新私有 C/PE 专项也失败，原因及 partial 编译范围独立记录于
[Windows私有平台验证](windows-private-platform-20261014.md)，不混入默认包结论。
