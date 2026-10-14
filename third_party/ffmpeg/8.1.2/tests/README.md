# AArch64 8-bit H.264 luma DC 私有验证

本目录验证固定 FFmpeg **8.1.2** 的局部核，不是软件解码资格、OHOS/HAP、
AVC444 双流或远程桌面互操作验收。仅支持 AArch64；macOS 不支持 Intel。
测试对象和辅助 ABI 函数只链接到私有可执行文件，不加入生产 export 或 HAP。

原归档 SHA-256：
`464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`。
同级 `../patches/0001-aarch64-h264-luma-dc-neon.patch` 只修改
`libavcodec/aarch64/h264idct_neon.S` 与 `h264dsp_init_aarch64.c`。
原 `h264idct_template.c` 不改，许可证为现有 LGPL-2.1-or-later。
新增代码根据原 C 的算术和稀疏布局编写，使用 FFmpeg 现有汇编入口宏。
源码 recipe/pin、bundle、package 的信任门由后续任务实施；本 runner 不替代它们。

## 构建与执行

准备两份独立解压树 `ORIGINAL`（只读）和 `SOURCE`（配置/构建），先验证归档。
在 `SOURCE` 用 `patch -p1 --dry-run` 验证补丁，再用 `patch -p1` 应用。
本例使用本机 macOS ARM64；所有路径由调用者给定：

```sh
# 在 SOURCE 中执行；不要指向已有接受/发布的 bundle 源目录。
./configure --arch=aarch64 --cc=clang --disable-everything --enable-decoder=h264 \
  --disable-programs --disable-doc --disable-network --disable-autodetect \
  --enable-static --disable-shared
make -j4 libavcodec/libavcodec.a libavutil/libavutil.a

# 回到仓库根目录。RESULT 必须是尚不存在的独立目录。
python3 tools/verify-ffmpeg-h264-dc.py --source "$SOURCE" \
  --upstream-source "$ORIGINAL" --output "$RESULT" \
  --target aarch64-apple-darwin --cc clang --execute
```

runner **只编译 fixture 与改名的原始 dispatcher**，链接已存在的 FFmpeg
对象，不调用 FFmpeg builder，不改源码，不下载，不操作设备/签名。
默认使用两份本地静态库，或通过重复 `--object` 指定完整的私有对象/库集合。
`--cc`、重复 `--cflag` / `--ldflag` 明确指定同目标工具链与依赖；配置、
源文件、被链接对象、核对象及测试二进制 SHA-256 和全部命令保存在 `results.json`。
源 hash 校验保证第一 oracle 仍是固定原 C，不以重新编写的 normalized oracle 代替它。

目标必须显式给出，支持 `aarch64-apple-darwin`、`aarch64-linux-gnu` 和
`aarch64-linux-ohos`。生成物必须是该种容器的 ARM64 二进制；不传 `--execute`
只编译。执行只允许匹配 OS 的本机 ARM64，OHOS 在此 runner 中始终只编译。
Windows ARM64 目标测试适配、Linux/OHOS 实机执行属于后续独立验证，不能由本机结果外推。

单独使用新的结果目录加 `--execute --benchmark` 执行吞吐测量，不混入 correctness
长跑。31 个批次，每批每核 262144 次调用，先各预热 65536 次，交替 C/NEON
批次先后顺序，计时外核对整个输出池 checksum。输出 ns/call median、p95（nearest-rank）
和 max。计时不锁 CPU 频率或绑定核心；这是同机/同对象/同工作负载的有界测量。

原始未打补丁对象的 RED 可用 `--cflag=-DFRD_PRISTINE_RED` 构建。该宏提供永不调用的
私有标记，测试真实 `ff_h264dsp_init` 不会选择它，因此在
`h264_luma_dc_selects_neon_8bit` 精确失败；它不是替代实现，也不得用于 GREEN。

## 覆盖边界

- 真实原 C、显式 uint32 模算术的 normalized oracle、新 NEON 三方逐字节比较。
- 119 个确定向量 × 13260 个 QP/scaling 组合；保留重复 qmul 的组合计数。
- 每个基位置 × 完整 65536 个 int16 幅值 × qmul `1/40/32768/3655680`。
- 7 个额外 qmul ABI 健壮性值、9 个显式舍入/加法回绕案例，固定种子
  `0x4c5a2026` 的 200000 个完整向量/全 int32 qmul 随机案例。
- 每次核调用比较所有 256 个输出系数。独立 fixture 检查输入/输出 redzone、
  输出地址 mod16 为 `0/2/4/6/8/10/12/14`，非别名输入保持原字节。
- 输入位于输出 index `0/16/128/240` 的别名全内存比较。
- 输入头尾保护页、只读输入页、输出头保护页与恰好结束于最后写位点的尾保护页。
- ABI wrapper 检查 x19–x29、v8–v15 低64位、x18 只读快照与 SP。
- NEON off/on × 8/9/10/12/14-bit × chroma 1/2/3，共30种真实调度。
  用真实通用 C 初始化加私有改名的原始 AArch64 initializer 构成未修改基线；
  只允许 8-bit/NEON 的 luma DC 槽不同，整个其余 context 必须逐字节相等。

这不是对全部 16 维输入的穷举，也不验证其他 CPU、OS、codec 或生产包。

## 私有平台接口契约

`h264-luma-dc-platform.h` 只承接保护页所有权与绝对 `CLOCK_MONOTONIC`
纳秒转换。POSIX 实现内联，不增加链接对象、系统库或编译参数；Windows
仅声明同名接口，独立实现尚未提供。原数值公式、RNG、5,974,518 cases、
30 dispatch contexts、ABI `.S` 和 benchmark 工作量均保持。
通用 runner 在编译前和结束后绑定 shared C、ABI `.S`、新 header 的 hash；
Linux qualifier 的既有 before/after 输入集合也包含 header。旧结果记录仍绑定
其原始源码字节，不能改写为新接口的运行证明。

`h264-luma-dc-platform-tests.c` 是独立 POSIX executable，不链接 FFmpeg，
不调用数值核、不消耗 RNG、不增加原 parity case 数量。它检查唯一 mapping
所有权、零/脏 owner、页大小乘三、mmap/mprotect/restore/munmap 失败、失败
不改写时间输出、纳秒溢出与实际单调时钟的前后包围。保护页测试使用隔离
child，要求 ready 标记及 SIGSEGV/SIGBUS；合法首尾 byte 必须正常退出。
只读输入写入、32-byte 输入与 482-byte 输出的两侧越界均必须实际失败。

下面是获准执行阶段后可使用的命令模板，SOURCE-READY 期间不执行：

```sh
# PLATFORM_RESULT 为新的私有目录，使用该 POSIX 目标既定 compiler。
"$CC" -O3 -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror \
  third_party/ffmpeg/8.1.2/tests/h264-luma-dc-platform-tests.c \
  -o "$PLATFORM_RESULT/platform-contract"
"$PLATFORM_RESULT/platform-contract"
```

确定性 RED 使用同命令分别追加 `-DFRD_DC_CONTRACT_NO_GUARDS` 和
`-DFRD_DC_CONTRACT_NO_READ_ONLY`，应分别在 `input start guard` 和
`read-only input write` 失败；任意编译错误、缺 ready 或其他退出不算预期 RED。
这些宏只存在于独立契约文件，不进入正常共享 parity executable。
契约 GREEN 后仍须复用同一份已冻结 shipping 对象完整运行原 parity；
POSIX 通过不能填补 Windows ARM64 编译、SEH、native guard/QPC、ABI、
shipping-object identity、parity、benchmark 或产品包/实时 RDP 门。
