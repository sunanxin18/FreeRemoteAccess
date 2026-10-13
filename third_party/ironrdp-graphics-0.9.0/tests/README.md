# Bitmap parser native consumers

这些测试直接编译生产 `src/rle.rs`，并执行该文件中全部既有 upstream RLE tests；没有 parser 测试副本。
新增 public `rle::decompress_native` 与原 `decompress` 共享 parser；native 仅通过显式 capability object 选择。

在仓库根，固定 ARM64 Rust 1.96.0 后运行：

```sh
export PATH=/Users/sun/.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin:$PATH
python3 third_party/ironrdp-graphics-0.9.0/tests/run_bitmap_tests.py --clippy
python3 third_party/ironrdp-graphics-0.9.0/tests/run_bitmap_tests.py --release
cargo test --offline --locked -p frd-rdp-legacy-kernels --test bitmap_parity --test bitmap_guard_pages
cargo test --offline --locked -p frd-protocol-rdp
cargo fmt --all -- --check
rustfmt --edition 2024 --check third_party/ironrdp-graphics-0.9.0/src/rle.rs third_party/ironrdp-graphics-0.9.0/tests/bitmap_wire.rs
python3 tests/legacy-oracle/test_catalog_production.py
python3 tests/legacy-oracle/verify_catalog.py
python3 tests/legacy-oracle/verify_sources.py
```

`run_bitmap_tests.py` 通过 Cargo JSON artifact 选取本次真实 kernel rlib，记录 Rust/LLVM/host 与精确编译命令。
Cargo build 使用 `--offline --locked`；不改变 root 或 vendor manifest/lock。graphics 是 excluded patch package，
root `cargo test -p ironrdp-graphics` 不允许该非 member 的 dev tests，而独立 graphics manifest 的 `bmp` dev dependency
在当前缓存缺失。因此此有界 harness 使用 `rustc --test` 对原生产模块测试，shipping graphics 由 RDP suite 实际编译。
release 以 Rust `opt-level=3/lto=thin/embed-bitcode=yes` 接收 root thin-LTO kernel bitcode，避免交给不匹配的 Apple LLVM linker。

`bitmap-original-wire.bin` 从 A2.3 独立 oracle 的冻结 catalog 逐字节提取全部 **16721** Bitmap 记录。
其格式是 `FRDBMP01`、LEu32 record 数、每例六个 LEu32-length 字段：name、LEu32(width,height,bpp)、wire、initial dst、
原版 output/partial、原版 Result 的 Debug 字符串。20 个 code families × 8/15/16/24bpp、直接/扩展/mega、首行/后续行、
跨行、全部 mask/tail、empty/incomplete、nonempty dst、源/输出错误优先序均来自原版，绝不从 patched decoder 生成。

`bitmap-original-malformed.bin` 的 **7616** 记录由 hash 固定的 registry 原版 `rle.rs` 单独编译执行，
包括各 code/depth/first-next/direct-extended 的每字节截断及 1×1 输出空间组合，另覆盖所有 0xf0..0xff header 的
0/1 长度和截断。原版 source SHA-256 必须是
`4a0a5132460e4890cf272743c0702890172d08a0336cca5f39e3800b44048fcb`；该 runner 无 Cargo patch/kernel dependency。
重现两个 fixture（仅主动 provenance 核验时运行，不在日常测试重新生成 expected）：

```sh
python3 third_party/ironrdp-graphics-0.9.0/tests/freeze_bitmap_wire.py \
  --catalog tests/legacy-oracle/fixtures/wire-catalog.json.gz \
  --original-rle /Users/sun/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ironrdp-graphics-0.9.0/src/rle.rs
```

文件 hashes 见 `../FRD-MODIFICATIONS.json`。fixture 核验包括旧/native 结果、精确 error variant/fields、全部 partial bytes，
malformed 每例再以目标 offset0..15 检查 prefix/suffix canary。native 叶子成功返回处的 test-only counters 证明真实 parser
期间调用 fill/copy/XOR/mask/dither；80 个 family-depth 与224 个 header/row variants 均受断言覆盖。没有 decoded-output 重放。

观察范围仅本机 macOS ARM64 NEON：这些测试不证明 session native dispatcher、最终 RGBA conversion、其他 ISA/ABI、
OHOS package/runtime 或 live RDP；默认 session 仍调用原 entrypoint。没有启用 whole-codec 能力。

A2.5 新增 `run_planar_tests.py`：以固定 Rust 1.96.0、offline/locked Cargo JSON artifact 编译实际 shipping
library，并由实际 `rdp6/rle.rs`/`bitmap_stream/decoder.rs` source wrapper 检查私有 plane consumption、scratch
和仅 `cfg(test)` 叶子计数。未复制 decoder 实现。vendor 自带 `bmp`/`expect-test` dev dependency 在本机离线缓存缺失，
因此不能声称完整 upstream suite；wrapper 的 `expect!` 适配器只做严格 pretty-Debug/deindent 相等断言，
所有原有15个 rle/encoder 测试仍执行。它不提供 snapshot 更新或忽略断言功能。

```sh
export PATH=/Users/sun/.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin:$PATH
python3 third_party/ironrdp-graphics-0.9.0/tests/run_planar_tests.py
python3 third_party/ironrdp-graphics-0.9.0/tests/run_planar_tests.py --release
python3 third_party/ironrdp-graphics-0.9.0/tests/run_planar_tests.py --clippy
python3 third_party/ironrdp-graphics-0.9.0/tests/test_planar_mutations.py
```

`freeze_planar_wire.py [--original-source <未修改crate目录>]` 仅在主动验证 provenance 时运行；先校验整个原 registry source inventory，再提取原版
catalog 的全部4,972个 Planar records（包含六组原bin的既有oracle RGB bytes）；另将六组固定24-bit原BMP像素转换为独立RGB expected fixture，附加4,181个 records
来自独立 hash-pinned 原版 actual-source process。其中2,237个 private plane cases覆盖256个control byte的
各截断、16/32特殊run全部raw field、row-last reset、partial/error四类及short dst；1,944个 bitmap cases
包括552个成功原用例的private scratch、所有header的截断和12个零几何及4个原版溢出panic用例。公共 bitmap API 的消费字节
始终没有测量值；只有私有 plane 成功返回usize时记录实际消费。失败只对比error/scratch，不虚构消费量。

原decoder零几何的接受/错误/可能panic与native安全拒绝分开；native在PDU之前拒绝零尺寸、RGB乘法/isize/append
溢出；需要header的alpha/chroma总长检查在PDU之后、reserve/scratch写入之前完成，合法尺寸保留PDU/read/segment/raw-plane错误顺序。坏plane可修改私有scratch，RGB输出仅在全部plane
成功后追加，既有dst前缀保持；rawalpha跳完整尺寸，RLEalpha始终解码消费，CLL0+CS保持ARGB。kernel错误不重试scalar。

计数仅存在于 `cfg(test)` 路径，实际parser解码时计数copy/fill/delta/RGB/YCoCg并对全部64个header检查color dispatch；
private plane case逐offset0..15验证canary。mutants仅存在target临时copy，验证alpha消费、奇数chroma ceil和CLL7
断言能杀死行为变异。release harness明确启用opt-level=3、thin LTO与embed-bitcode，使LLVM22对象经过固定rustc处理。
观察仅本机macOS ARM64 NEON，未验证OHOS/其他平台执行、session registration、生产eligibility或live RDP。

四个overflow records的dims为u32且mode=2明确标记64bit原版panic。64bit比较该原panic；i686字段可表示，
但原release算术wrap可能改变分类，因此只执行同4项native InvalidDimensions/prefix/scratch安全断言并显式计数，
不将其宣称为64bit原版panic exact parity。其余正常wire/error/plane fixtures逐条运行。
