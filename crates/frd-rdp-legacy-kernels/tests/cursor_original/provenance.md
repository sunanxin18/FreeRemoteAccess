Task4A2.10B1 SOURCE freeze, 2026-10-14. 这些文件尚未编译、import 或执行。

所有完整来源从 `/Users/sun/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` 读取：

- `ironrdp-graphics-0.9.0/src/pointer.rs`: `1df842235dc4ef9ea3d621f6e863ee0f8e07625a7eeaf6132140bf908ca506a3`。
- `ironrdp-graphics-0.9.0/src/color_conversion.rs`: `ddcc027718044e0bee6b98c1bf660ebe3cbcfeb45e5ee15cae5ca6b9911f5c17`。
- `ironrdp-session-0.11.0/src/image.rs`: `61cb9451695daa51e4e9bfe05ee7fae3faf746ae5275cb178b8a1f09c5cb5499`。
- registry `.crate` archives: graphics `c7493e426b6a8104cd497e518ba7781a9c7fc9a5f58a9cc0c1111e6d66f63bcc`，session `3f1a5a20c24d8ebf47477f9f513201aeb894e7c7f779ea633ece65e42a9f0652`。

`rgb565_original.rs` 是 color_conversion 原版 101..112 行原样摘取，SHA256 `4eb39b9b985520cff3e91e46e154c50dfbd4eacd0b6f26fad546c820d167fcce`。
`copy_original.rs` 是 image 原版 75..143 行原样摘取，SHA256 `0471f3f2b1d6cc8cfc572aa41be1de00a33118756661ad466991046bcd26e66f`。
`common/cursor_reference.rs` 的 decode 是原版 materialization 分支的单行适配；它并非完整 original decoder 的执行证据。它不导入或调用 SIMD formulas。
其 wrapping_reference 明确使用 `wrapping_add`，是 root 冻结的 release 合同参考，而不是把旧 debug panic 变成已通过 parity。

`generator.rs` 原样 include 完整 pointer source，只用原版 core/PDU rlibs；无 K 或 modified vendor dependency。fixture 计划产生 FRDCUR01 共432行 records。
该 generator 尚未编译，原版 fixture 尚未生成，依赖来源/最终 profile/rlib hashes 必须在 root 释放时重新检查并记录。
现存 `target/legacy-oracle/debug/deps` 仅用于未来精确编译命令的路径定位，本 SOURCE 阶段未 import、执行或认定其当前依赖闭包独立。
原版 composite release reference 必须明确 `overflow-checks=false`；debug partial-alpha 溢出差异仍保留。

源完整副本是 test provenance，不是生产替代实现；完整 vendor consumer、wire、几何、cache 与平台资格均 OPEN。
