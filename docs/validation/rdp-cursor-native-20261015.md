# RDP 软件光标的显式 SIMD 处理路径

2026-10-15，状态为**开发中**。本次补齐显式 `RequiredNative` 路径的软件光标
像素展开、背景保存/恢复和合成，并接到真实 RDP 会话处理链路。默认
`Compatibility` 保持原实现。普通鸿蒙 HAP 尚未注册远程协议，本记录不代表
手机或 PC 已能登录 Windows/Mac。

## 实现范围

协议中立的 `NativeKernels` 增加两个受检查的行操作：`decode_cursor_row` 和
`composite_cursor_row`。数值后端为 ARM64 NEON、非 macOS 的 x86/i686 与
x86_64 SSE2；检测、封闭参数及长度检查继续在原接口内，不允许静默标量回退。
macOS 产品仍仅支持 ARM64。各架构的目标运行资格分别记录，不能互相替代。

New、Color、Large 光标复用原 bpp/stride/mask 校验与行布局，支持原有
1/16/24/32 bpp；padding 不传入行叶子。颜色光标的 XOR 和 AND 均 bottom-up，
mono 保持 top-down，硬件光标输出的 checker 使用输出行号。新增入口不修改
wire parser，也不新增颜色格式或协议广告。

同一 retained kernel 选择传到快/慢路径、缓存、pending 和服务器位置、隐藏/
默认光标、即时本地鼠标输入，以及 Bitmap/tile 更新前后的背景恢复和重新合成。
替换 Processor 继续沿用原选择传递。图像操作先检查所有有关几何、完整行范围
和分配，再更改图像/光标状态；背景搬运走已有 `copy_u8`。光标只产生 damage，
不增加已知桌面内容，也不建立 FullBaseline/Startup。

本步骤保留固定原版的 release 像素行为：普通合成显式 modulo-256，保留背景
第四字节；透明和 invert 的处理与旧实现一致。它也保留原版的平方和通道解释，
不是对 RDP alpha/颜色规范正确性的验收。旧 debug 在部分合法字节上加法溢出
的差异单独保留，没有将其当作 native 错误或改用饱和算术。

## macOS ARM64 的实际验证

所有有效验证显式指定 Rust 1.96 的 Cargo、`RUSTC` 和 `RUSTDOC` 绝对路径，
并核对实际编译命令。最初仅使用 `rustup run … cargo` 的尝试实际调用了 PATH
中的 Homebrew rustc1.98；原版 generator 因 E0514 失败，记录保留。修正入口后，
generator 的整数类型推断 E0689 以 `1usize` 单行标注修复，不改变数据或算法。

| 验证范围 | 实际结果与限制 |
|---|---|
| 行叶子 parity | debug 6 项、release 7 项通过；包括完整565域、mono mask 对、短尾、未对齐、partial alpha、wrap 与第四字节 |
| 行长度与保护页 | debug/release 各一项长度、一项保护页父测试通过；有效子进程和上下越界故障负控制均执行 |
| 完整原版行输出 | 独立 registry/原版 source 生成432条、529984像素；实际原生行输出逐条通过 |
| session consumer | debug 8 项、release 9 项通过；完整原版 image/compositor 对照、八格式、边缘/尺寸变化、背景恢复、pending/error 和分配失败 |
| graphics consumer | 实际源与固定完整原版源对照5项通过；包括两种 target、padding、XOR/AND 翻行判别 |
| actual factory/Stage | cursor 筛选5项通过，其中2项新增真实 RequiredNative factory/Stage 测试，其余是既有内容与缓存回归；不包含网络 |
| 来源检查 | 14项窄检查及固定22337条 catalog 记录身份核验通过；没有重新解码全集 |
| 语义判别 | MSB 行变体、5个消费者变体均在编译后被实际断言拒绝；首次存活的 AND 翻行变体保留，并用逐行黑白锚补齐判别 |
| 格式 | workspace 默认 `cargo fmt --all -- --check` 和指定 vendor 文件检查通过；完整原版 source/excerpts 未格式化 |
| 完整 RDP 包回归 | `cargo test -p frd-protocol-rdp --offline --locked` 实际352 PASS、0 failed、11 ignored；doc-tests为0，不计额外覆盖 |

原版432条 fixture SHA-256：
`d3e47bdd87edf120436042cd87d2adb188e16200cbd1d766fed90c7735b8becd`。
原版依赖来自独立 workspace 的锁定 registry 闭包，47包/2006源文件身份已核对。
消费者完整 source 参考与独立进程 fixture 的证据层分别保留，没有把二者混同。

ABI 探针直接包围实际数值 core/函数表目标，在恢复调用者寄存器之前检查。
它依赖当前 Rust ABI，所以默认 `#[ignore]`；执行必须先检查当前实际产物的
四个签名、隐藏结果位置、全部调用者栈范围和探针恢复次序。不能把普通测试
显示 ignored 当作 ABI 通过，也不能沿用另一个构建的地址放行执行。

最终格式后的实际 release unit SHA-256 为
`64db1e3becffbbc2a834694f7e79bcb18e47043acd3ed2c53420d540b3205df7`。
重新反汇编确认四个目标、八个调用点、隐藏结果 x8 和调用者栈范围后，精确
`--ignored` ABI 测试实际 1 PASS；故意破坏 x25/x29 的负控制也在同一测试内。
消费者的独立 Spec/Quality 审查通过，没有 P0/P1/P2 问题。

固定行微基准只执行一次，14组数据完整保留。decode 的1/3/4/7/16/33像素宽度
比参考慢，64才略低；composite 的1/3也较慢。每64调用的 batch median 只有
167–7250 ns，首调用出现0 ns，不能形成整体或稳定提速结论。首调用不是冷
cache；两端使用同尺寸的独立预分配缓冲。日志 allocation=0 是代码结构打印，
没有 allocator 观测，不视为实际分配计数证据。

同日用官方 SDK 和显式 Rust1.96 构建当前 `frd-protocol-rdp` 的 OHOS ARM64
release 库通过，原始日志 SHA-256 为
`4748fdaf011ca86694ad7473769f3d8ac909f8123d8763e6ac61d9d40093b2e3`，
位于 `target/harmony-rdp-build-20261015/`。这包含当前消费者及网络/认证依赖的
编译，但不是最终 FFI/HAP 链接或设备运行证明。

完整 RDP 包回归原始日志位于
`target/task4a210b2-full-rdp-regression-20261015/raw.log`，SHA-256 为
`7365c18743a9e7f9956206ceb248fc12c1f117771976eb44e567f4f149cc3f52`。

本地原始记录和 source/argv/exit/hash 索引分别位于
`target/task4a210b1-execution-rust196-20261015/`、
`target/task4a210b1-final-execution-20261015/` 和
`target/task4a210b2-consumers/`。此前失败日志保持原样。

## 剩余工作

OHOS 手机/PC 的同源运行验收继续独立推进。Windows/Linux 各架构、OHOS 的实际 ABI/ISA/性能和
软件光标规范数学尚未因此完成；测试专用 library `.o` 是 LTO bitcode，不能
当作已审原生机器码对象。

真实编码到显示的目标性能、bulk/ZGFX、SSE2 publication、普通 HAP 协议与
解码器注册，以及手机/PC 真实登录、首帧、持续刷新、输入和重连仍按完整计划
推进。现有离线、宿主或包验证不替代这些结果。
