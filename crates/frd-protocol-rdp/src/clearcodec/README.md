# ClearCodec 状态层（尚未生产接线）

`Decoder<K, N>` 接收像素内核和独立的 NSCodec provider。协议层复用 IronRDP
0.9.0 顶层、residual、subcodec 公共 parser；对 parser 不消费的短尾显式拒绝。
所有缓存写入、V-Bar 游标和序号在整个 bitmap 成功后才提交，错误不会污染状态。
输出为完整覆盖的 opaque BGRA 位图；residual 可以只提供前缀，其余由后续层补齐。
空 CACHE_RESET 控制消息只返回 `None`，不会伪造位图。

与上游 parser 的两个有证据的区别：

- SHORT_VBAR_CACHE_MISS 是低 8 位 YOn、高 6 位 YOff；上游 0.9.0 使用的
  `word >> 6` / `word & 0x3f` 与官方 Example 4 的 `00 0f` 不符。
- RLEX 即使只有一个 palette entry 仍有 packed suite byte；suite underflow
  必须拒绝，不能饱和减法恢复为 index 0。

规范来源（2026-09-08 检查）：

- [Bitmap stream、会话级序号、glyph 面积和 CACHE_RESET](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/f6c8a114-eaba-489f-9626-f41ad27a19b1)
- [Residual 像素数可小于原图](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/4e42306a-ff66-426f-95c4-d53ae5dba900)
- [Short V-Bar cache miss 布局和缓存更新](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/d9fee626-1a6b-44b9-a2a8-fbdfe6076f24)
- [Example 4 的 V-Bar 解码实例](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/266bbde5-a651-41a9-a5b7-721b7c4f2663)
- [Subcodec 长度上界和像素次序](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/4720bbf2-b369-43d5-8e50-975e4ab7e29d)
- [Microsoft MS-RDPEGFX 2023-09-20，RLEX segment 2.2.4.1.1.3.1.1.2](https://winprotocoldoc.z19.web.core.windows.net/MS-RDPEGFX/%5BMS-RDPEGFX%5D-230920.pdf)

这是依据公开协议重新实现的状态层，没有复制 IronRDP graphics decoder 的实现。
IronRDP 公共 PDU 类型和 parser 仍作为现有 Apache-2.0/MIT 依赖使用。
`tests.rs` 中的标量内核仅为 `cfg(test)` correctness oracle，测试数据均人工合成。
Limits 同时限制输入、输出像素、像素工作量和覆盖区间数量。后者避免高度重叠的
合法区域把像素工作量转化为过大的区间元数据分配。

接线要求：同一远程会话全部 ClearCodec 消息共用一个 decoder，不能按 surface
分别维护序号。解码失败后需要终止或显式重同步图形流，不能跳过失败消息后继续。
生产使用仍要求完整 NSCodec provider、各目标 SIMD parity/benchmark、EGFX 接线、
真实服务端首帧与持续更新证据。macOS 仅 ARM64；Windows/Linux 保留 i686、x64、ARM64。
