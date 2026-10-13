# IronRDP PDU 0.9.0 原版身份与窄修复

2026-10-14，A2.7P。生产副本从本机已验证的 crates.io cache archive
`ironrdp-pdu-0.9.0.crate` 提取，SHA-256
`4ccd1179a4d106df1930347701388b5c79bc3725fa7dab4438d57db0d9d19347`。
126 个 regular member 逐一与 registry 和独立 oracle 的原始
`SOURCE-INVENTORY.json` 核对一致；registry 未修改。

`UPSTREAM-SHA256SUMS` 保留全部126个原始 hash。上游 MIT/Apache-2.0
license、normalized Cargo.toml、Cargo.toml.orig、Cargo.lock、README、
CHANGELOG、原源码和原单测全部保留。上游 `.cargo_vcs_info.json` identity 为
commit `11a0810cfbbabd8b8023875a05e3041216d4b01b`，path
`crates/ironrdp-pdu`。没有重写上游 package/version/features。

唯一修改的原始文件是 `src/basic_output/surface_commands/mod.rs`：现有
`cast_length!` 结果显式标为原本要求的 `usize`，optional header 合计改用
`checked_add(BitmapDataHeader::ENCODED_SIZE)`。overflow 在 header/data 借用前
返回现有 `other_err!` 的 `DecodeErrorKind::Other`，description 为
`bitmap data length including header exceeds usize`。不使用会被 session
FastPath 兼容分支吞掉的 `InvalidField`。非overflow输入保持合计检查次序、
typed错误、header端序、borrowed data 和原 public API。

根 workspace 仅新增 PDU exclude/patch 条目；root lock 只移除同版本0.9.0的
registry source/checksum。实际 root 图中 session 使用唯一的path PDU；独立
oracle 保持 registry 四pins，原 manifest/lock/inventory/expected不改。

新增 focused tests、原版 freeze、mutation 和证明层边界见 `tests/README.md`。
这些测试使用真实 public rlib 和临时 actual-session-source wrapper，既不复制
decoder，也不改 session/graphics 生产源码。无需网络、设备、服务端支持程序、
credential、codec advertisement、ISA 分支或benchmark。
