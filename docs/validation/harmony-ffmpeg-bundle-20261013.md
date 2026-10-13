# 鸿蒙 FFmpeg 后备包构建验证

日期：2026-10-13。系统解码器按精确格式和远程桌面延迟要求优先；当前两台模拟器的系统 AVC 静态帧输出未通过，原因与边界见[系统验证](harmony-system-codec-20261013.md)。本记录仅覆盖 ARM64 OHOS 独立后备包的构建与静态检查，尚未将该包集成至普通 HAP、运行目标解码或注册协议。

## 目标边界

构建器使用官方 API26 Clang/sysroot、Rust `aarch64-unknown-linux-ohos`，固定 FFmpeg 8.1.2 源码归档 SHA-256 `464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`。启用 H.264/HEVC decoder/parser、共享 PIC 与 ARM64 NEON；禁用 GPL/nonfree/version3、网络、程序及 autodetect。固定逻辑 prefix `/usr`，以独立 DESTDIR 暂存；不重命名 Linux 产物冒充鸿蒙。

插件仅在 `target_env=ohos` 选择 `thread_count=1, thread_type=0`，避免 frame-thread 管线需要后续输入才能释放静态帧；Windows、桌面 Linux 与 macOS 原策略保持。该策略保留 NEON，但不消除码流自身的 B 帧重排，也不证明 1440p/4K 吞吐已满足要求。

仅包含三个固定原生库：`libavutil.so.60`、`libavcodec.so.62`、`libfreeremotedesk_ffmpeg.so`。随包保留许可、NOTICE、对应源码取得说明、精确 configure 与 provenance。实际插件链接依赖为上述 codec/util 加公开 NDK `libc.so`；不存在对 GNU libc、宿主库或私有系统 codec 库的依赖。没有修改插件 v1 ABI。

## 验证状态

独立构建与 22 项实际 ELF/输入变异与目标链接测试已执行，独立审查发现无版本链接别名与已验证版本库未绑定，已于 `8343bd3` 改为精确版本链接，修复通过独立复审，没有新的 Critical/Important 问题。构建产物留在忽略的 `target/ffmpeg-ohos/`；生成二进制不入库。不能据 configure 的 NEON 开关宣称实际设备执行了全部 SIMD 热路径。

后续独立检查包括普通签名 HAP 的依赖包装、应用自身模块锚点、不可写安装目录和实际加载身份、ABI/回调生命周期、单帧及最后一帧在无 EOS 时输出、逐像素比较、1440p/4K 性能、协议协商和真实 Windows/Mac 桌面。当前手机/PC 普通包仍为空协议目录。


最终干净构建尝试为 `target/ffmpeg-ohos/attempt-qjsezbh7`，经校验发布到 `bundle-attempt-qjsezbh7`；没有覆盖此前的有效包。库 SHA-256 为：

| 库 | SHA-256 | DT_NEEDED |
| --- | --- | --- |
| libavutil.so.60 | `925ac0829cab1a8a6e246653e4dbf553db4d54e04d44bccc1f908351ffd3255a` | libc.so |
| libavcodec.so.62 | `84ca7851005b3dab87e5f3162d631b30726eab727d60397e10a130ac587b4a55` | libavutil.so.60、libc.so |
| libfreeremotedesk_ffmpeg.so | `4e0f435d4e1fd3a85e53711ec15d30048bdf1c64d310a37d28932336d916cd05` | libavcodec.so.62、libavutil.so.60、libc.so |

默认插件测试 5 项通过；OHOS 禁用 native feature 时，未设置 FFMPEG_DIR/OHOS_SDK_ROOT 的测试二进制成功交叉编译，链接器仍使用官方 SDK，未执行。旧预检目录缺少新来源合同，实际 Cargo 构建会拒绝它。宿主构建日志保留已有 `from_trusted_paths` dead_code 警告，没有把它混称为纯净无警告。没有进行目标解码、SIMD 热路径执行或吞吐基准。

实现提交 `9c2cd42`、精确链接修复 `8343bd3`，独立复审已通过；既有 loader dead_code warning 保留至后续加载器切片处理。最终 manifest SHA-256 `c9515bdaa88c062c231d72ebb22db7d19d4ecabca6463d2da1ac8eb030e973bc`；主代理另行重算三库与 manifest 哈希，一致。当前承诺固定来源和可重复构建步骤，没有以同一最终 recipe 的两轮干净构建证明 bit-identical。


独立审查 I1 的实际复现是：`verify-inputs` 检查带版本名的库，但原 `-lavcodec/-lavutil` 链接读取未检查的无版本别名。修复后 OHOS 使用 `dylib:+verbatim=libavcodec.so.62` / `libavutil.so.60`，桌面保留原链接方式。新增测试在临时 prefix 将 `libavcodec.so` 错指向 util60，并删除 `libavutil.so`，随后实际 OHOS 编译成功，验证精确链接指令、依赖清单及插件 bytes 与正常构建完全相同。它证明别名已不参与该平台链接，不声称畸形输入的所有场景均已覆盖。
