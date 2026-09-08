# ClearCodec 有界性能采样（2026-09-08）

Apple M4，macOS ARM64，Rust 1.96.0 release。固定迭代、预热及 black_box；逻辑输出吞吐不是端到端桌面帧率，也不是其他目标架构证据。NSCodec 含分配，输入为微软官方 15×10 示例，计时前核对完整像素。

运行：

```sh
cargo test --locked --release -p frd-protocol-rdp bounded_decode_benchmark -- --ignored --nocapture --test-threads=1
```

原始测量：

```text
FRD_BENCH arch=aarch64 backend=neon kernel=fill iterations=256 logical_output_bytes=8294400 elapsed_ns=38889208 mib_s=52071.001 stride=0
FRD_BENCH arch=aarch64 backend=neon kernel=copy iterations=256 logical_output_bytes=8294400 elapsed_ns=54383125 mib_s=37235.815 stride=0
FRD_BENCH arch=aarch64 backend=neon kernel=expand iterations=256 logical_output_bytes=8294400 elapsed_ns=57035917 mib_s=35503.944 stride=0
FRD_BENCH arch=aarch64 backend=neon kernel=scatter iterations=10000 logical_output_bytes=4320 elapsed_ns=16382500 mib_s=2514.801 stride=7680
FRD_BENCH arch=aarch64 backend=neon kernel=nscodec-ms-example-15x10 iterations=100000 elapsed_ns=68594500 ns_decode=685.945 allocations=included
```

2 项 benchmark 通过。Windows/Linux i686、x86_64、ARM64 尚需目标执行；本记录不打开生产 gate。

## Linux 三目标原生／32 位进程验证

GitHub run `34185964017`，提交 `9813987`，三job成功。逐job日志确认相同target
ClearCodec/NSCodec 38测试通过、2默认ignored，随后显式运行2个release benchmark均通过。
i686在x86_64宿主运行i686目标进程，日志报告 `arch=x86`，不是x64结果替代。

| 目标 | fill MiB/s | copy MiB/s | expand MiB/s | scatter MiB/s | NS 15×10 ns/decode |
| --- | ---: | ---: | ---: | ---: | ---: |
| Linux i686 | 64513.492 | 56372.389 | 31636.742 | 1449.863 | 925.995 |
| Linux x86_64 | 27500.538 | 15120.264 | 16911.198 | 1952.675 | 952.616 |
| Linux ARM64 | 51075.738 | 31288.442 | 25516.951 | 4183.868 | 672.216 |

沿用上文固定工作量与单位。不同CI宿主CPU、频率和内存环境不同，不能据此排名架构。
这些是ClearCodec/NSCodec测试，不含随后新增Progressive，不是GUI或网络性能证据。
