# RDP 像素叶子与 Surface 长度门验证

2026-10-14。`6ad44a5`实现独立八格式像素行后端，`15b006c`修复其ABI测试的观察边界；`2770093`修复portable Surface可选header长度检查。三者均已完成独立审查。此记录覆盖明确的数值叶子与解析入口，不代表image/session完整接线、生产选择或真实远程桌面完成。

## 像素行实现与原版对照

`NativeKernels::convert_pixel_row`支持Index8调色板、RGB555、RGB565、BGR24、RGB24及八种32位格式。Raw32和Region32分别保持原版的alpha/X字节规则，不能互相替代。NEON/SSE2执行转换热路径；调色板索引在汇编中取RGB三个字节，最后一项不读取第四字节。合法短尾使用有界staging并调用同一向量核；同排列整行复制复用原ISA copy。能力、长度和调色板错误在输出写入前返回。

独立原版完整source/process生成FRDPIX01共176条记录，包含全部555/565字值、64种Raw32与64种Region32组合、两套非灰调色板及alpha锚点。原始fixture SHA256为`ee69627eb9513f5e71a7b310dd60b41ececf12ef72aa2be45562fc7cd0910f26`。宿主debug/release的相关parity、短尾、未对齐、错误零写入和真实保护页验证通过。八目标优化对象生成通过：Windows/Linux的i686、AMD64、ARM64，macOS ARM64与OHOS ARM64；对象生成不是这些系统的运行证据，macOS Intel不支持。

首次ABI探针使用Rust回调，可能在观察前恢复被破坏的x25，因此其完整证明声明已撤回。`15b006c`改为直接调用实际内核的汇编探针，在返回后、保存寄存器恢复前检查x19–x29和d8–d15低64位；实际Rust参数、x8返回缓冲和SP对齐经过debug/release反汇编核对。真实内核执行后故意破坏x25、x29的负控制均被检出，像素仍正确；独立复审Approved。此ABI证据仅限macOS ARM64和固定Rust1.96.0，不外推OHOS或Windows/Linux ABI。

## Portable Surface长度检查

固定`ironrdp-pdu=0.9.0`生产副本保留126个上游文件身份，仅`src/basic_output/surface_commands/mod.rs`修改原源码：可选24字节header的总长度用`checked_add`计算，溢出为既有`Other`错误，使Fast-Path保留source并向会话传播。无header路径、非溢出解析、borrow/消费位置和public API保持。

宿主debug/release的860条独立原版public EXT/Set/Stream/FastPath记录一致；实际shipping session畸形前门、image/cursor不改、handler/pointer未进入等验证通过。独立原版u32表达式的debug panic/release wrap与修复后精确错误是算术模型；Linux/Windows i686 debug/release四项PDU lib检查仅属编译，没有32位完整PDU/session运行证据。来源清单、原版oracle隔离、唯一生产path节点、716项lock限定变化及故障变体均通过独立审查。

## OHOS手机与PC实际执行

Root从`4492ebc`生成完整gitarchive快照，与并行消费者开发工作树隔离。官方API26 NDK和Rust1.96.0 offline/locked构建实际K及RDP库；RDP库构建32.45秒成功。Cargo JSON识别精确目标rlib和测试程序，五份ARM64 ELF均只依赖系统`libc.so`。测试入口使用实际public库；观察版session由同一冻结完整source生成，仅添加入口计数，不注入模拟错误。

| 测试程序 | 手机API26 ARM64 | PC API26 ARM64 |
|---|---|---|
| pixel guard pages | 1/1 | 1/1 |
| pixel parity | 5/5 | 5/5 |
| public Surface原版对照 | 2/2 | 2/2 |
| public session前门 | 1/1 | 1/1 |
| 实际source session入口计数 | 1/1 | 1/1 |

两台各10项通过，发送前后SHA一致，各程序实际`FRD_EXIT=0`。完整命令、源快照、rlib/ELF哈希、动态依赖和两端日志在`target/harmony-a27-frozen-4492ebc/`。本次不重复宿主已通过的宽套件，没有替换普通HAP或恢复开发验证入口。

## 性能与剩余门

作者在Apple M4的三次有界宿主row基准均保存1056组输入/输出SHA一致结果。最终4096像素Index8 native中位5442.5ns、原版3136.5ns，仍约慢1.736倍；RegionBgrA→BgrX为812ns对156ns，仍约慢5.205倍。部分555/24位转换更快，但没有整体提速结论。此为预分配单行对照，不是完整codec/网络/呈现延迟，也不是OHOS目标性能。本轮不以正确性测试替代每目标性能门。

A2.7B仍须完成真实written计数、原尺寸stride/crop/padding、Set/Stream几何、八格式image写入、cursor背景与无更新区域规则；之后还须每目标运行/性能/包门和生产选择。既有默认策略及codec广告尚未改变，Windows/Linux新像素后端实际运行未完成。
