# Apple MVS ARM64 色彩与像素转换

2026-10-15，状态为 **实验性**。默认关闭的
`frd-protocol-apple/mvs-native-colors-arm64` 实现颜色和像素转换；
`mvs-native-arm64` 聚合此项与此前已验证的[IDCT基础](mvs-idct-arm64-20261015.md)。
未注册鸿蒙 Apple factory，不能由此宣称普通鸿蒙应用已经可以连接 Mac。

## 契约与接线

新代码位于 Apple crate 私有模块，不改变 wire、cache、generation、窗口、
renderer 或 `SurfaceUpdate` 契约。编译门要求 ARM64、小端和 NEON 编译基线；
显式启用但条件不满足时拒绝编译，不静默回退标量，也不在提交后新增可失败的
CPU 探测。默认路径沿用原标量选择；macOS 只支持 ARM64。

实际调用点包括 full/mode-5 与 type-1 scratch 的64像素 YCbCr→RGB、
mode-4单色、type-0 RGB→BGRX与非黑判定、type-1 RGB替换、BGRX字节与
逻辑u32互转，以及 surface全量/局部发布。8像素批量色彩/打包核和4像素
字节/word核用于短尾时，先复制到固定栈数组，再执行同一向量核，不越界加载
或写回。类型化u32始终自然对齐。新RGB的X为0；既有u32转换保留全部四字节。

保留系数91881/116130/46802/22554、偏置32768、有符号shift16和0..255饱和。
完整u8输入域的带偏置分子绝对值小于15,000,000；加Y后的R/G/B分别落在
[-179,433]、[-134,390]、[-227,480]，i32可精确保留结果。原标量色彩函数体
除名称外逐字不变，作为默认实现和独立oracle。已提交IDCT实现/测试及锁文件
字节不变。

提交顺序、内存预算、失败不提交、resync和重叠copy暂存继续沿用原有逻辑。
初版在两个调用点把输出预设长度带入默认路径，主代理发现额外全缓冲清零后，
恢复默认type-0和surface原有reserve/with_capacity后append的写入方式。
只在新native路径预设输出长度，没有测量或声称默认性能曾发生退化。

## 已执行的主机与链接检查

使用绝对路径Rust/Cargo/Rustdoc1.96，offline/locked，官方OHOS SDK。

| 检查 | 最终结果 |
|---|---|
| 默认 / colors-only Apple debug全库 | 各519 passed / 9 ignored |
| IDCT+colors aggregate Apple debug全库 | 522 passed / 9 ignored |
| 三种配置Release `pixels_` 聚焦 | 各14 passed / 0 failed / 0 ignored |
| 顶层CLI默认 / 无默认特性测试 | 各166 passed / 2 ignored |
| 顶层两种构建、两项help、全仓fmt | 实际exit0，七个源码文件前后身份一致 |
| OHOS ARM64 aggregate Release libtest | 实际最终链接exit0，非rlib/check |

每次Release色彩对照穷举全部16,777,216个三元组，另加262,144个固定seed样本，
合计17,039,360组；每个64像素tile比较全部192字节。mode-4对照穷举
1,048,576组。debug分别使用524,288组色彩及65,536组mode-4输入。
另有2080组0..129像素×0..15字节偏移的尾长/canary/高字节测试，99组仅末尾
像素非黑的测试，以及三个真实发布路径集成用例，覆盖stride、裁剪、重叠copy、
高字节和type-0尾部非黑判定。既有完整记录/type-1/代际/失败回归期望不改。
9项既有私有/网络测试保持忽略；唯一warning为基线已有测试helper。

固定字面值先取得真实语义RED：色彩stub的[0,0,0]与期望[99,101,100]不符；
打包stub未检出最后像素且未保留四字节，均实际断言失败。第一次静态检查还因
Rust符号hash后缀与LLVM15指令格式解析错误而失败。四个符号原本已存在；
修正检查器后保留全部指令要求，初始ELF、旧检查器及原始失败均保留。

## 实际目标产物

最终官方OHOS小端AArch64 PIE测试程序7,073,200 bytes，SHA-256：
`ce9c89b3c74272ca6435e70b3e8283b0fa2969daf4ac9450f4d2e89898345f33`。
解释器`/lib/ld-musl-aarch64.so.1`，直接依赖`libc++_shared.so`和`libc.so`。
官方SDK ARM64 C++库1,262,504 bytes，SHA-256：
`f69947af100b652e349ae51e4a4888bcc7a327db9085272d318176ef1782bf0f`，
其额外依赖只有libc。没有stub或新依赖；SDK link-time libc不等于设备libc身份。

主代理从实际ELF独立读取符号和完整有界反汇编：

| 叶函数 | 实际区间 | 指令证据 |
|---|---|---|
| bytes_words4 | 0x19def8..0x19df04 | LDR Q / STR Q / RET |
| words_bytes4 | 0x19df04..0x19df10 | LDR Q / STR Q / RET |
| ycbcr_blocks8 | 0x19df10..0x19e008 | 向量MUL/MLA/SMLAL、算术移位、饱和窄化、ST3 |
| rgb_pack8 | 0x19e008..0x19e03c | LD3、ORR、UMAXV、ST4 |

四叶没有标量变换乘法或外部调用，没有新增内联汇编。索引、常量装载和读取
已经向量归约的nonblack布尔值仍使用正常标量控制代码。

## 目标执行和资格边界

源码、数学、调用点及实际ELF经独立审查Spec PASS / Quality Approved。
随后同一最终ELF和上述官方SDK C++库在手机、PC两个API26 ARM64专用模拟器
顺序执行。每端14项像素测试及3项精确消费者全部通过，共8条测试命令、34次
通过执行，无失败、忽略或重试；实际host退出和独有guest退出均为0。

每端均实际完成17,039,360组色彩对照及1,048,576组mode-4全域对照。
精确消费者覆盖非对称AC块方向、10×9四逻辑tile裁剪，以及type-1损坏输入
保持可见surface并请求resync。主代理独立复核全部原始输出、测试计数、退出、
七个源码身份及设备上传后/执行后的两个产物哈希，结果在
`target/task5a2-root-20261015/actual-target-test-acceptance.json`。

只向每端新的独立任务目录上传此ELF和C++库，使用命令局部LD_LIBRARY_PATH；
未修改应用数据、系统库、全局设置或权限。公开boot完成标记和uname确认环境，
不声称读取了唯一boot UUID，也不由暂存文件哈希推断映射库身份。这是CLI ELF
计算和指定消费者的运行证据，不代表普通HAP沙盒或真实会话。

| 平台 | 此后端当前证据 |
|---|---|
| macOS ARM64 | 宿主三种配置数值/回归；不是远程会话 |
| OHOS ARM64手机/PC | 同一最终ELF在两端各14项像素+3项精确消费者通过；含完整色域/mode-4，不含性能/HAP/真实会话 |
| Windows/Linux ARM64 | 仅源码架构边界，未验证该后端完整构建/运行/性能 |
| Windows/Linux x86、x86_64 | 显式新feature不可用，既有默认路径保留；提取的生产guard已作真实目标编译检查 |
| macOS Intel | 产品不支持 |

ARM64显式关闭NEON时guard实际拒绝；big-endian为源码门，未安装标准库作
完整编译。guard结果不等于这些平台的完整crate或产品资格。

整MVS状态/整管线双实现差分、目标性能和稀疏代价、其余像素热路径完整资格、
最终FFI/HAP/签名/沙盒加载、Apple组合根、HP/HEVC/AAC/输入和真实会话均待完成。
手机/PC真实Mac认证、首帧、持续刷新、输入和重连未取得证明。Windows目标
本轮TCP3389仍超时，真实RDP验收不能由本项替代。

本地原始资料：`target/task5a2-mvs-pixels-20261015/`；主代理独立材料：
`target/task5a2-root-20261015/`。上述结果只覆盖本次明确列出的测试与产物。
