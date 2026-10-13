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
