# 固定生产副本

来源：crates.io `ironrdp-graphics-0.9.0`；原始 archive SHA-256 `c7493e426b6a8104cd497e518ba7781a9c7fc9a5f58a9cc0c1111e6d66f63bcc`。
`UPSTREAM-SHA256SUMS` 永久记录复制时全部原始文件，不随私有接线重写。
许可证沿用 crate 内 LICENSE 文件。

A2.3 仅适配 Cargo.toml 的本地 kernel 依赖；A2.4 在 `src/rle.rs` 增加显式 checked Bitmap native 入口、
共享 parser 的 materializer、native leaf dispatch 与测试导入。原 `decompress` 及各 bpp entrypoints 继续走 scalar materializer，
保留原命令级 first-line/insert-FG、FG 无条件源长度检查、empty/incomplete 接受、nonempty dst 和错误顺序。
没有 session dispatch、PDU、RDP6、媒体、平台壳或 kernel 数值变更。

`FRD-MODIFICATIONS.json` 给出这两个原文件的 upstream/post SHA-256 与新增测试/fixture identities。
`tests/legacy-oracle/verify_catalog.py` 仅接受这两个明确原文件的精确 posthash；其他原文件仍必须与原清单匹配，
不能向记录追加任意 source path 扩大豁免。原 registry source/archive 验证不受此例外影响。
新增 wire fixtures 只来自冻结 A2.3 catalog 或 hash 固定的原版 registry rle.rs，生成/测试方法见 `tests/README.md`。

A2.5 另在 `src/rdp6/rle.rs` 与 `src/rdp6/bitmap_stream/decoder.rs` 接入显式 checked native RDP6入口。
wire/plane controller由旧scalar和native共享；copy/fill/delta与RGB/YCoCg叶子直接用于实际解析，
所有RLE alpha仍解码消费，CLL0+CS保持ARGB，全部平面成功才append RGB。Kernel失败没有scalar重试。
旧入口/default及encoder/PDU/session不变。native另做零尺寸、产品/isize/append算术预检与reserve失败映射，
RGB/append的unsafe geometry在PDU之前拒绝，header-specific alpha/chroma总长在PDU之后但写入之前拒绝；原版的零尺寸接受/报错、overflow panic不属于native兼容承诺。
私有scratch可在parser失败后变化，已存在dst前缀保持且失败不追加RGB。

修改清单现在精确允许manifest、Bitmap source、这两个RDP6 source共四个posthash；其他原文件仍严格核验。
新增fixture只提取冻结原catalog，或通过hash-pinned原版actual source process产生，未用patched实现生成期望值。
六组bin/BMP与许可证沿用原 `UPSTREAM-SHA256SUMS`/隔离oracle source inventory，未复制修改或重写原始清单。
public bitmap无bytes-consumed字段；测量只来自private plane原版成功返回值。
