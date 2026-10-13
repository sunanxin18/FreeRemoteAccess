# Task4C3A：独立 OHOS ARM64 FFmpeg bundle 实施回执

日期：2026-10-13。BASE `198829c6c14de1ea2d5a43f1bc6fda5b209cedec`。本任务在 `/Users/sun/FreeRemoteAccess` 工作区实施；没有 guest、签名、网络登录、remote Git、loader/FFI/HAP/ArkUI/protocol/CI/README/root-doc 改动，没有新增子代理。主代理的 `docs/validation/harmony-ffmpeg-bundle-20261013.md` 草稿未纳入本提交。

结论：独立 clean 构建、最终三库 ELF/来源门、21 个真实产物/编译输入测试、插件默认政策测试和 OHOS native-feature-off 交叉编译已完成。这是目标构建与静态产物证据，尚不能判定 DecoderReady 或普通 HAP 解码可用。实现只承诺固定来源的可重复 recipe；没有对同一最终 recipe 进行两次 clean rebuild/hash 比较，不声称 bit-identical。

## 改动范围和合同

- `tools/build-ffmpeg-ohos.sh` 独立入口；`tools/ohos-ffmpeg-bundle.py` 实现 build、verify、verify-inputs。不会调用 Linux/macOS builder，不借用或重命名桌面 ELF。builder 只支持 macOS ARM64 主机、Python >=3.12、官方 Native API26 `26.0.0.105` SDK 和 `aarch64-unknown-linux-ohos`。其他主机/目标显式失败。
- FFmpeg 8.1.2 固定上游 archive URL/SHA256；只启用 H264/HEVC decoder/parser，共享 PIC，固定逻辑 `/usr` + 私有 DESTDIR，无 GPL/nonfree/version3/network/programs/autodetect。清除环境注入的编译/链接 flags、host library 搜索路径及 Cargo wrapper；每次独立 Cargo target 目录。cargo、rustc、rustdoc 全部由 `rustup which --toolchain 1.96.0` 解析。
- 每次使用新的 `attempt-*` 私有目录，先编译/验证，再以一次目录 rename 发布新的 `bundle-attempt-*`。不存在隐式“最近一次”缓存选择，也不覆盖已验证包。失败保留诊断。安装时 FFmpeg 上游生成的标准版本 symlink 仅作私有编译输入；输出复制其目标字节为固定普通文件，不建立 symlink 兜底，不更改 SONAME 或 DT_NEEDED 命名。
- 静态 verifier 从实际 ELF 解析 ELF64/little-endian/AArch64/ET_DYN、SONAME、DT_NEEDED、定义的可执行 getter、RPATH/RUNPATH 和 GNU libc symbol version，固定三库闭包；同时核对 program/section dynamic/dynstr/dynsym 指向一致及 DT_NULL 终止，不能以伪造 section 或 DT_NULL 后条目隐藏实际加载合同。文件集合恰好为三库 + 四个配置文件 + 许可/NOTICE/SOURCE + manifest，所有条目必须普通文件。
- 来源 manifest 记录 SDK metadata、tool bytes/version/path、sysroot digest、公开 libc 输入、Cargo.lock/工作区依赖/plugin 源码/recipe digest、Rust tool bytes/version、完整 configure/Cargo command、headers/config、34 个 AArch64 编译对象和输出摘要。固定配置、许可与真实 ELF 字节独立核对；变异测试中的自洽 manifest 不能放行错误 ELF/配置。
- plugin `build.rs` 的 OHOS 分支显式检查 linux/ohos/aarch64 及精确 target；要求官方 SDK linker，调用 verifier 验证 SDK、headers、输入库、配置及当前源代码来源，强制 bridge 使用 SDK compiler/archiver，并只在 OHOS native 分支添加 `libfreeremotedesk_ffmpeg.so` SONAME。native-feature-off 提前返回保持不变。Windows/Linux/macOS 桌面 native 分支不增加来源门/SONAME 行为。
- `decoder.rs` 的 OHOS 软件策略固定 `(thread_count,thread_type)=(1,0)`，覆盖尺寸/CPU 条件；桌面仍保留原有 1440p+且至少两 CPU 时 `(2,1)` 的策略与三个回归测试。ABI v1 不变，C bridge 未修改。此策略禁用 frame pipeline，仍保留 NEON；B 帧重排属于码流行为，实际 1440p/4K 吞吐待目标 fixture/性能验收。

## 最终实际产物

最终成功 attempt：`/Users/sun/FreeRemoteAccess/target/ffmpeg-ohos/attempt-ptm9qh2e`。
最终发布目录：`/Users/sun/FreeRemoteAccess/target/ffmpeg-ohos/bundle-attempt-ptm9qh2e`。
编译输入 prefix：该 attempt 的 `dest/usr`。所有二进制、日志与产物留在 gitignored target；仅源码/测试/本报告提交。

| 普通文件 / SONAME | bytes | SHA256 | DT_NEEDED |
| --- | ---: | --- | --- |
| libavutil.so.60 | 672824 | `925ac0829cab1a8a6e246653e4dbf553db4d54e04d44bccc1f908351ffd3255a` | libc.so |
| libavcodec.so.62 | 2264656 | `84ca7851005b3dab87e5f3162d631b30726eab727d60397e10a130ac587b4a55` | libavutil.so.60, libc.so |
| libfreeremotedesk_ffmpeg.so | 508632 | `4e0f435d4e1fd3a85e53711ec15d30048bdf1c64d310a37d28932336d916cd05` | libavcodec.so.62, libavutil.so.60, libc.so |

三库均无 RPATH/RUNPATH，无 GNU/private-system dependency；getter `frd_ffmpeg_get_api_v1` 在 plugin 中为已定义、默认可见、可执行段内函数。三库没有 GNU Build ID note，manifest 如实记录空 build_ids，以 SHA256 作为产物身份摘要；不编造 Build ID。

最终 manifest SHA256：`ee65c35d0556ebdc9602438d20d702dc51dcbd8f49595f2a5788a830bc8cb511`。
固定 LGPL 文本 SHA256：`246041b6ecf9bc32d718a62c57877c78b5eb397b6467e74ed7ae2626ab189c30`；与 fresh 上游源 `COPYING.LGPLv2.1` 字节相同。

源码 URL：`https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz`；SHA256：`464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`。本次仅复用这个经过固定 hash 核验的 cached archive，每次 fresh extraction，不复用预检的 configure/make/library 输出。无 FFmpeg 源码 patch；包内 SOURCE 文本提供下载命令和 hash，configure/config/source 输入摘要随包。

SDK：`/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native`，Native API26 `26.0.0.105`；compiler 为 OHOS clang 15.0.4，Target `aarch64-unknown-linux-ohos`。metadata SHA256 `3dbc4a06ffd3b1be6b45c3c346a7900ca42f8a81a5b0454dc7f5cc1e5a9509a8`；sysroot 聚合 SHA256 `5144396933cfc6165edf29e35f509736ca2402bb92f8062d540f943918d6493c`。

**公开依赖白名单依据来自实际 link：** 初始未改 plugin 的真实 OHOS ARM64 link 已只产生两个 FFmpeg SONAME + libc.so；最终同样如此。因此唯一外部允许项为选定官方 SDK `native/sysroot/usr/lib/aarch64-linux-ohos/libc.so`，SONAME `libc.so`，SHA256 `2a4d19590f74d27cd701b8a8861d76a5816891dda41648949f3079b24d114f3b`。SDK libdl/libm 等为静态 archive；没有猜测放行 GNU libdl.so/libm.so、libstdc++ 或私有 codec。未来若实际链接闭包变化，必须重新审核，当前 verifier 不自动放宽。

Rust 实际版本：cargo `1.96.0 (30a34c682 2026-05-25)`，rustc/rustdoc `1.96.0 (ac68faa20 2026-05-25)`；三者路径均位于 `/Users/sun/.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin`，digest 详见 manifest。Cargo.lock SHA256 `1b3709ddd93d9ed86215faaae935b2067aad8bb28a66b7f431fba299e4981f50`。

config.h 的 `ARCH_AARCH64/HAVE_NEON/HAVE_NEON_EXTERNAL/HAVE_NEON_INLINE=1`；实际 34 个 AArch64 对象及其摘要随 manifest。实际 nm 核对 H264 loop-filter NEON 与 HEVC IDCT NEON 函数（例如 `ff_h264_h_loop_filter_chroma422_neon`、`ff_hevc_idct_16x16_8_neon`）；这是编译/符号证据，不是 guest kernel 执行、逐像素或性能证明。

## 执行命令与结果

所有命令 cwd 为 `/Users/sun/FreeRemoteAccess`。以下是最终接收的验证；早期迭代日志仍留在 target。

```sh
PYTHONDONTWRITEBYTECODE=1 tools/build-ffmpeg-ohos.sh \
  --archive target/ffmpeg-macos-architecture-test-1788805845/ffmpeg-8.1.2.tar.xz \
  > target/harmony-ffmpeg-preflight/task-4c3a-builder-final4.log 2>&1
```

exit 0。内部 configure/make -j2/make DESTDIR install/cargo build --locked --release --target aarch64-unknown-linux-ohos -p frd-video-ffmpeg-plugin --features native-ffmpeg 均 exit 0；完整 argv 在 bundle 的 configure-args.json/manifest.json。细节日志在最终 attempt 的 `configure.log/make.log/install.log/plugin.log`；官方 `llvm-readelf -h -d --dyn-syms` 的三份 `*-elf.txt` 同目录。

```sh
PYTHONDONTWRITEBYTECODE=1 python3 tools/ohos-ffmpeg-bundle.py verify \
  target/ffmpeg-ohos/bundle-attempt-ptm9qh2e \
  > target/harmony-ffmpeg-preflight/task-4c3a-verified-elf.json

PYTHONDONTWRITEBYTECODE=1 \
FRD_OHOS_FFMPEG_TEST_BUNDLE="$PWD/target/ffmpeg-ohos/bundle-attempt-ptm9qh2e" \
FRD_OHOS_FFMPEG_TEST_PREFIX="$PWD/target/ffmpeg-ohos/attempt-ptm9qh2e/dest/usr" \
python3 tools/tests/test_ohos_ffmpeg_bundle.py -v \
  > target/harmony-ffmpeg-preflight/task-4c3a-tests-accepted.log 2>&1
```

exit 0；**21 tests passed**。16 bundle cases含真实正例、错误架构、GNU/private dependency、错误 SONAME、额外/缺失库、缺失 getter、symlink、manifest hash/config、真实配置 bytes、RPATH、错误目标、section/program dynstr 不一致、DT_NULL 提前终止；5 input cases含 header bytes/major、SDK provenance、过期 plugin 源、错误库架构。多数 ELF/config 变异同步重写 manifest/hash，再由固定合同拒绝。

```sh
env RUSTC="$(rustup which --toolchain 1.96.0 rustc)" \
    RUSTDOC="$(rustup which --toolchain 1.96.0 rustdoc)" \
    "$(rustup which --toolchain 1.96.0 cargo)" test --locked \
    -p frd-video-ffmpeg-plugin --lib \
    > target/harmony-ffmpeg-preflight/task-4c3a-policy-tests-rust196.log 2>&1

env PATH="$(dirname "$(rustup which --toolchain 1.96.0 rustc)"):$PATH" \
    RUSTC="$(rustup which --toolchain 1.96.0 rustc)" \
    RUSTDOC="$(rustup which --toolchain 1.96.0 rustdoc)" \
    "$(rustup which --toolchain 1.96.0 cargo)" fmt --all -- --check \
    > target/harmony-ffmpeg-preflight/task-4c3a-fmt-rust196.log 2>&1

git diff --check
bash -n tools/build-ffmpeg-ohos.sh
```

exit 0；**5 plugin tests passed**：既有三个桌面尺寸/CPU政策回归、native-off unavailable，以及 OHOS `(1,0)` 显式政策（5种尺寸×4种CPU）。最初 shell cargo/rustc 指向 Homebrew1.98.1，早期默认测试也曾通过；最终接收记录如上重新以 coherent1.96.0 运行，builder/目标编译始终固定1.96.0。

```sh
env -u FFMPEG_DIR -u OHOS_SDK_ROOT \
  CARGO_TARGET_DIR="$PWD/target/harmony-ffmpeg-plugin-native-off" \
  CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER=/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native/llvm/bin/aarch64-unknown-linux-ohos-clang \
  RUSTC="$(rustup which --toolchain 1.96.0 rustc)" \
  RUSTDOC="$(rustup which --toolchain 1.96.0 rustdoc)" \
  "$(rustup which --toolchain 1.96.0 cargo)" test --locked \
  --target aarch64-unknown-linux-ohos -p frd-video-ffmpeg-plugin --lib --no-run \
  > target/harmony-ffmpeg-preflight/task-4c3a-native-off-ohos.log 2>&1
```

exit 0，实际 OHOS cfg 下 test executable 成功交叉编译；**未执行**。移除 FFMPEG_DIR/OHOS_SDK_ROOT 后 feature-off 仍可构建。旧 `from_trusted_paths` dead_code warning 来自 loader，未扩展范围修改它。

实际 native guard 失败验收：同官方 target linker + coherent1.96.0，以 `FFMPEG_DIR=$PWD/target/harmony-ffmpeg-preflight/stage/usr` 进行 `cargo build --locked --release --target aarch64-unknown-linux-ohos -p frd-video-ffmpeg-plugin --features native-ffmpeg`；exit101，因预检 prefix 无 `ohos-ffmpeg-inputs.json` 被拒绝。日志 `task-4c3a-reject-unprovenanced-input.log`。没有把预检库当产品输入放行。

失败发布验收：`tools/build-ffmpeg-ohos.sh --target aarch64-unknown-linux-gnu` exit1；对之前所有已发布目录每个文件的 SHA256 前后比对相同。日志 `task-4c3a-publication-failure-test.log`。一次迭代 clean attempt `attempt-z8yojfl2` 在 install 后发现 avutil upstream major 宏位于 version.h、并非 avcodec 的 version_major.h，因此失败关闭；没有发布目录或覆盖旧包，随后改为分别核对并成功 clean 构建。

NEON 编译符号验证命令：

```sh
/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native/llvm/bin/llvm-nm --defined-only \
  target/ffmpeg-ohos/attempt-ptm9qh2e/ffmpeg-8.1.2/libavcodec/aarch64/h264dsp_neon.o \
  target/ffmpeg-ohos/attempt-ptm9qh2e/ffmpeg-8.1.2/libavcodec/aarch64/hevcdsp_idct_neon.o \
  > target/harmony-ffmpeg-preflight/task-4c3a-neon-symbols-accepted.log 2>&1
```

exit0。未用任何目标执行 helper，未用 receive_one_frame 自动 flush 冒充 noEOS 输出。

## 自审和后续门

自审核对了产物命名/文件类型、源 archive/SDK/Rust一致性、native-off分支、桌面政策回归、OHOS专属SONAME/线程选择、异常发布旧包保护和全部负例。自审中发现 ELF dynamic 迭代应在 DT_NULL 结束、section 与实际 program 应一致，已加入精确失败注入并以最终 recipe 重新 clean 构建。范围内完成后停止写产品文件，交由主代理独立 review。

后续仍须普通签名 HAP 的安装 native-lib 命名/namespace、dladdr锚点/不可写代码根、ABI实际调用/全部callback归属、phone/PC分别真实fixture（首尾帧、大尺寸不依赖EOS、重排、flush/reclaim/destroy）、NEON逐像素/性能和授权live远程验收。当前没有注册任何 codec、没有将系统候选失败转换成运行可用声明。

跨平台影响：Windows x86/x64/ARM64、Linux x86/x64/ARM64、macOS ARM64 的默认解码策略和构建分支保持原合同；这里只新增 OHOS ARM64 的静态后备构建输入及固定线程政策。macOS Intel 不新增支持。原 README 矩阵及普通包后续集成由主代理按明确分工更新，本提交不扩大范围。
