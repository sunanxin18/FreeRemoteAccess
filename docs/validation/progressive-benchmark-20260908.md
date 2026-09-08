# Progressive 目标运行与内核基准（2026-09-08）

提交 `d597212` 的 GitHub run `34188365266` 整体成功。三个Linux目标分别运行
Progressive/邻近EGFX 50项测试通过、1项默认ignored，随后显式release基准通过。
i686是x86_64宿主上的32位目标进程，不以64位结果替代；ARM64在原生ARM64 runner执行。

| 目标 | job | Normal µs/tile | ReduceExtrapolate µs/tile |
| --- | --- | ---: | ---: |
| Linux x86_64 | 101941170321 | 26.38 | 28.60 |
| Linux i686 | 101941170430 | 26.11 | 28.70 |
| Linux ARM64 | 101941170550 | 23.15 | 24.77 |

计时范围为每次一个分量的64×64 inverse DWT加BGRA转换，2000次循环；
不含完整三分量熵解码、所有分配、网络、画面发布和GUI呈现。不同runner不能直接用于架构排名。
同次run的固定FFmpeg plugin 11项测试与6项decoder fixture亦全部通过。

该证据包含RLGR末符号修复，不包含随后FIRST标志、可选context、缓存生命周期等更改。
后续更改仍需目标复跑；Windows/macOS的对应run完成前不得推定七目标全部通过。

## macOS ARM64

同提交的run `34188365267`、job `101941170014`整体成功。
Progressive50项通过、1项默认ignored，release benchmark显式执行通过：
Normal15.41 µs/tile，ReduceExtrapolate16.14 µs/tile（计时范围同上）。
固定FFmpeg6项fixture，以及macOS合成签名包正例和9项拒绝场景均通过。
macOS仅覆盖ARM64；没有Intel构建或验收。

## Windows x86 / x86_64

同提交 run `34188365225` 的 x86_64 job `101941170330` 和 x86 job
`101941170762` 均成功；分别运行 Progressive/邻近 EGFX 50项通过、1项默认 ignored，
随后显式 release benchmark 通过。x86 使用 i686 MSVC 目标进程。

| 目标 | Normal µs/tile | ReduceExtrapolate µs/tile |
| --- | ---: | ---: |
| Windows x86_64 | 27.11 | 28.80 |
| Windows i686 | 25.68 | 28.97 |

计时范围仍为单分量 DWT + BGRA；不是完整协议或端到端延迟。两项 package job
成功不等于 Windows GUI 真机连接验收。ARM64 原生 runtime job `101947137153`
仍在执行，暂不将其记为成功。上述目标也必须复跑 d597212 之后的行为修复。
