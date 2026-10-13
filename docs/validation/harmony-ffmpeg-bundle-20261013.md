# 鸿蒙 FFmpeg 后备包构建验证

日期：2026-10-13。系统解码器按精确格式和远程桌面延迟要求优先；当前两台模拟器的系统 AVC 静态帧输出未通过，原因与边界见[系统验证](harmony-system-codec-20261013.md)。本记录覆盖 ARM64 OHOS 后备包构建、模块加载器实现、普通 HAP 包装及两台模拟器安装启动；尚未从应用进程调用该加载器、运行目标解码或注册协议。

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

## OHOS 应用模块加载器

`4f5089e` 增加 OHOS ARM64 专属加载分支：用私有模块标记的 `dladdr` 定位本应用 Rust 动态库，再从同目录加载三个固定后备库。公开 AbilityKit `OH_AbilityRuntime_ApplicationContextGetBundleCodeDir` 提供应用代码目录边界；不接受调用方路径、环境路径、桌面 Linux 的 executable 路径或私有系统 codec 库。

实现保留目录/文件句柄和 dev/inode，拒绝符号链接及应用可写对象，并检查实际版本入口、getter 和六个 ABI callback 属于预期映射，完成检查后才转换函数指针。加载使用 NOW|LOCAL，句柄按依赖逆序清理。所有权与不可写检查依赖正常系统安装这一前提；`dladdr` 只给出路径和基址，不是映射 inode 或签名证明。

固定 Rust 1.96 下宿主模块 47 项测试通过，覆盖路径/权限拒绝、文件替换、callback/ABI、依赖清退及 decoder/frame 生命周期。官方 OHOS ARM64 rlib 编译无 warning；独立 cdylib 链接探针确认为 ELF64/AArch64，公开 NDK 依赖包含 `libability_runtime.so` 与 `libc.so`。该探针未在设备执行。旧 `from_trusted_paths` warning 通过限定其真实 macOS 调用范围消除，桌面行为保持。

正常 HAP 内的代码目录与实际模块路径是否一致、整个目录链是否满足保守所有权/权限假设，仍待两台模拟器分别确认。调用须在应用上下文建立后进行；当前没有后备库注册、真实出帧或远程互操作验收。独立审查通过，无 Critical/Important；审查记录一项非阻断测试改进：文件替换用例应精确断言身份错误，避免权限拒绝掩盖身份检查退化。正常 HAP 和解码门仍未完成。原始测试与链接产物在 `target/task4c3b-loader-check/`。

## 普通 HAP 包装、签名与安装

普通 Debug/Release HAP 均已构建；构建入口显式接收已验证 bundle 和独立 manifest SHA-256，纳入三个带固定版本名的 FFmpeg 库与许可、来源材料。最终包精确包含桥接库、Rust FFI、SDK C++ 运行库及三项后备库。Hvigor 使用针对后备库的 strip 排除规则；Debug/Release 三库均与上表字节一致。最终 module.json 显式 `extractNativeLibs=true`、`compressNativeLibs=false`。桥接库构建时的 RUNPATH 已去除。

33 项 HAP/暂存检查通过，包含重复/额外/缺失文件、架构/SONAME/依赖错误、损坏字节、许可缺失、签名前后内容比较、暂存符号链接及部分失败清理。实际错误 manifest 构建被拒绝，已有输入 bundle 和已生成 HAP 哈希未改变。受控 ZIP 测试完成后，主代理另行执行真实签名与安装，见下文。

未签名包：Debug `ede7490ca3e03622da1ab2b3ae7f2a15ec5b69acf5cb391686f0e74ee4a3690c`，Release `2a5559a21eca4bc86da03874e930b0c7f09a88335018d870f137dfca1a9addea`，位于 `target/task4c3d-hap-bundle/`。没有通过此次包装注册 codec 或协议；普通包仍为空协议目录。独立审查及修复复审已通过；实际库加载仍为后续门。

同日主代理沿用既有 normal 开发身份执行官方 sign-app/verify-app，随后产品 checker 再次逐项比较 signed/unsigned 六库、八份材料和 module.json，通过。签名 Debug SHA-256 `35c53d0472e3467c7b3e716fb6970778a5e45e21c482f5b4251a128911e31481`；密码仅经关闭回显的交互 console 提交，未进入 argv 或日志。

两台专用模拟器均以该同一 HAP 完成覆盖安装和新进程启动，官方 Emulator CLI UI 树显示 FreeRemoteDesk 登录表单及“此设备尚无可用的远程连接协议”。这只验证安装与启动；此包尚无 loader caller，不证明后备库被实际加载。两台 bm 元数据仍报告 `isCompressNativeLibs=true`/模块 `compressNativeLibs=true`，尽管 HAP 的 module.json 为上述显式配置；不将该字段当成实际提取路径或压缩行为的结论。代码根、版本名库提取与运行映射留待应用内 fixture 验证，未采用 shell 权限绕过。签名、安装、布局和筛选后的非秘密安装元数据保存在 `target/task4c3d-hap-bundle/`。

包装实现 `22513b2` 的独立审查发现 ownership 清单直接写正式路径，半截 JSON 会阻断清理和重试。`dc9419b` 将清单完整写入并关闭私有临时文件后无覆盖发布，失败时只清理本次已发布对象，保留未知清单。10 项定向、38 项范围测试通过，独立复审确认 I1 已解决且无新增阻断项。既有 Hvigor SemVer warning 延后处理。该修复只变更暂存工具与测试，未重新构建 HAP；上列签名安装证据对应 `22513b2` 的产物，不能描述为一次 `dc9419b` 新构建。下一步是普通应用内加载与解码 fixture。
