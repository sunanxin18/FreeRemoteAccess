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
