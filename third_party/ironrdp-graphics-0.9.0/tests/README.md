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
