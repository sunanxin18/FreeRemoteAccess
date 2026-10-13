# 正常 HAP 的显式合成 codec 测量

以下 Cargo features 只生成验证包，默认关闭，并在 debug/release 中互斥；不能与 `normal-hap-codec-fixture` 或 `native-presentation-fixture` 同时启用：

- `normal-hap-codec-benchmark`：Rust release 优化的有界同步 submit 基准。
- `normal-hap-avc444-profile`：独立 256×144 AVC High444 I/P 工作负载，供官方应用范围 profiler 采样。
- `normal-hap-hevc444-profile`：独立 256×144 HEVC RExt 8-bit 444 I/P 工作负载。

原 `normal-hap-codec-fixture` 仍严格限定 `debug_assertions`；默认 release 不包含正确性或测量 worker。三个新增开关是刻意允许优化构建的验证能力，不能作为生产 codec、RDP AVC444 双流合成或 HEVC wire 注册的证据。正常产品协议目录仍为空。没有运行时路径、环境、参数或文件选择入口。

worker 在应用上下文建立后异步启动，每进程至多一次。UI 销毁仅协作置取消标志，原生同步调用之间检查取消和期限；线程自己拥有并释放 backend/decoder/frame，UI 不 join 未结束调用。一次 case 最多 10 秒，全 worker 最多 45 秒；同步调用不能强制抢占，缺失结束 marker 或超期均为 incomplete。日志 `FrdCodecMeasure` 使用固定合成 run ID、case、mode、真实 TID 和公开 CLOCK_MONOTONIC 纳秒时间，禁止输出路径、地址、秘密和原始 codec 错误。

基准有 10 个固定 case：256×144 AVC420/444、2560×1440 AVC420、3840×2160 AVC420 各测 AnnexB/长度前缀；tiny HEVC444 和新增 256×144 HEVC444 各测 AnnexB。前后逐可见行比较小输入参考，或比较大输入独立可见平面 SHA。每 AU 必须立即输出恰好一帧，且身份、代际、时间戳、尺寸、格式正确；不用 EOS、flush、dummy packet 或后继输入释放最后一帧。

一次冷创建及完整首序列单列，随后 4 次完整序列预热，再 3 批各 20 次完整序列。所有重放从 reset/new generation 后的 IDR 开始，然后喂依赖 P。每个 AU 在计时前构造；计时区间只包住生产 `VideoDecoder::submit`，包括生产 framing、plugin send/receive 和 owned frame copy，排除身份检查、日志、hash、oracle、reset、frame drop 及析构。reset、frame drop 与最终 decoder/backend 析构另外报告。统计输出实际 n、整数纳秒 median、nearest-rank p95、max、sum；compute-only fps/pixels/sec 只表示指定合成输入的同步调用成本，不是 kernel 时间或真实桌面持续帧率。无有效样本的组报告 n=0，不补零成本帧；部分序列不增加完成序列数。

profile 模式每进程只选择一个 codec/profile，完整序列重复约 7 秒，保留 case 剩余期限供最终 oracle 与清理。`profile_begin`/`profile_end` 的同一 TID/monotonic 时间用于采样归属；性能验收须使用独立 benchmark 包并关闭 profiler/断点。当前三库、loader、线程策略和 plugin ABI 不变；HEVC 独立参考来源见 `tests/codec-fixtures/hevc-measurement-provenance.json`，其输入不证明线上 HEVC 协商。
