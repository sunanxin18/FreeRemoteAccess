# MVS 有限整管线工作集与测量（2026-10-15）

状态：**实验性**。新增测试专用真实 wire 工作集和测量入口，默认/native 宿主资格与固定四轮有限计时完成，两个官方 OHOS ARM64 Release 测试程序已构建。手机和PC模拟器均已独立完成两份程序各32条资格；2026-10-16手机固定ABBA计时完成，PC正式计时仍未完成。不能由本项宣称生产解码资格或鸿蒙真实 Mac 会话已完成。

## 入口、工作集与正确性

两个显式 ignored 测试位于 `mvs_pipeline_measurement_tests.rs`，通过 `network_reader.rs` 私有测试 include 接入；没有修改生产函数、分派、依赖或已有 fixture。默认程序仅作为测试 oracle，不是支持平台的生产标量后备。

每个 case 固定4条真实 wire 记录，经原 `apply_native_mvs_frame`、`AppleSurfacePublisher`、`MailboxSurfacePublisher` 到真实 FrameMailbox：

| case | 表面与输入 |
|---|---|
| full-copy-720 | 1280×720，mode1/2 copy-heavy 交替 |
| full-copy-tail | 1366×769，合法6像素末列、1像素末行及逐行 source 策略 |
| clipped-overlay | 1366×769 中的10×16，overlay/committed 混合来源与 bitmap 重置 |
| local-copy | 1920×1080 中两个位置的64×64 mode1/2 |
| local-miss | 同上64×64，mode6/7 cache miss，保留 stderr 诊断成本 |
| local-type1-copy | 真实 type0 metadata seed → type1 opcode0/2，实际局部 patch |
| full-dense-y | 1280×720，每tile63个非零 Y AC，非零非均匀量化表，零 chroma |
| local-type1-refine | 非零 mode5 seed → opcode1 populate → opcode3 实际命中 |

不直接注入 decoder cache、像素或 prepared operation；使用原 parser 和事务。setup 保留不对称 bitmap 与已固定 AC literal 独立期望，不以两个后端相等作为唯一正确性依据。

两份独立 Rust1.96 Release 宿主程序各执行32条资格记录，32个完整 CPU framebuffer/patch/mailbox 输出与8个最终完整 decoder 状态，共 **386,897,350 bytes** 逐字一致。比较额外核对每一行 patch 等于 committed framebuffer 对应行、Damage→FrameBoundary 顺序、stride/长度/世代/修订；不是仅比较摘要。14,400个最终 dense-Y seed 每块63个AC全部非零，不能称三通道 dense。

像素、矩形、消息顺序和最终状态四种有限变异均被同一比较器拒绝。root 冻结后再次读取完整工件、执行全字节比较与负对照，结果一致。

## 时钟与观察边界

测量入口在真实 apply 前、apply 后、publish 后设置三个时钟；分别得到 apply组合段、publish段、总量。当前入口不能无侵入拆出 prepare/commit 两个独立数值。参考读取、完整比较、日志、mailbox drain、context/表/基线构造均在对应记录时钟外。

每process每case1个warmup cycle及3个sample cycle，每cycle4条；固定 default/native/native/default 四进程，后两进程倒序case。每backend共192条 measured、64条 warmup，不自动加倍或反复跑到稳定。所有实际逐记录值保留，短时间样本不删除、不按任意FPS阈值判通过。

计时包含既有 cfg(test) type-1 TLS/Vec 顺序记录、测试 wrapper 成功断言，以及复用 mailbox helper 的 `TracingProtocolWake` Mutex/Vec 开销。两后端共用这些测试开销；因此仅用于有限相对 CPU 管线对照，不能表示生产绝对耗时。共享宿主后台负载也须随执行记录。

观察仅覆盖 DisplaySurface 构造次数、framebuffer 地址/长度稳定、patch数量/字节和mailbox queuedbytes；每context一次surface构造、8entry/16MiB预算、每记录排空。没有测量所有allocator次数、私有Vec capacity或RSS。网络/TLS、服务器采集编码、GPU上传绘制、ArkUI/FFI/HAP及真实远端端到端延迟均不在本项范围，不能把耗时倒数宣传为实际帧率。

## 当前工件与检查

| OHOS ARM64 Release 测试程序 | bytes | SHA256 |
|---|---:|---|
| default | 7,291,896 | `ec3c3829bea79a9d87263ae6f76c21a829c6d7925f9f9b6031de06339559d7ca` |
| native | 7,291,480 | `d5603fbdf9f11fb1da98ac40bdd623f4ba220ea6d88f42bea47deccee00e2132` |

两个程序使用 coherent Rust1.96、官方 API26 clang/clang++/ar/sysroot、locked/offline 实际构建，opt-level3、debug_assertions=false；真实 Cargo artifact 为 fresh=false，依赖缓存复用如实记录。实际 FDK archive 为 `fef9d3e5…`，并非假定与另一个构建目录相同。root 重新核对45个Apple源和76个传递输入、实际ELF/SDK工具摘要与架构。

实际 native ELF 中确认新测试→apply→publisher，以及现有 IDCT/copy/RGB pack 原生调用；测量函数顺序为 clock→apply→clock→publish→clock→drain。编译器把 Instant 内联为 Timespec::now，不以高级符号名缺失判实现失败。作者首次提取脚本错误期待被内联的type1方法名，保留失败后仅修证据脚本，在同一ELF上重新确认调用；没有改产品源码或补跑资格掩盖失败。

独立审查为 Spec PASS / Quality PASS，无需修正源码；重新核对四个二进制、完整输出、45+76个源和实际native调用。审查确认现有测试唤醒跟踪也进入发布时钟，后续按上文共同测试开销披露。

本地证据：`target/task5a6-mvs-pipeline-20261015/`；root接收与后续执行：`target/task5a6-root-20261015/`。新测试均显式ignored，普通cargo/CI不会触发性能或生成大文件。手机/PC目标资格及手机正式测量已由后续记录补充；PC正式测量、HAP/真实Mac、其它架构和完整生产性能仍为独立未完成项。

## macOS ARM64 固定四轮实际结果

root 在源码/工件冻结、独立审查之后，顺序执行 default-forward、native-forward、native-reverse、default-reverse 四个独立 Release libtest 进程。四次实际退出0，各1个测试通过，分别完成96条measured/32条warmup；合计384条measured，每backend192条及64条warmup。每条计时后完整输出、每cycle最终状态均与已接收参考逐字比较，通过且源输入前后未变。未自动重跑或调整工作集。

宿主程序 SHA256：default `43c5102c3899e4b18890a1b6b7e99a2d256233ec2e83dd57b8d037f2e72a650e`，native `3b2d3ffff6fd02092e97117ec95e318bda24002426449c21809b1c2c141aac88`，均为Mach-O ARM64。每case/每backend有24条逐记录样本；下表是apply+publish总量的中位数，不含计时外初始化/比较/日志。

| case | default 中位µs | native 中位µs | native/default |
|---|---:|---:|---:|
| full-copy-720 | 2,069.355 | 989.604 | 0.478 |
| full-copy-tail | 2,448.167 | 1,187.854 | 0.485 |
| clipped-overlay | 2.605 | 3.208 | 1.232 |
| local-copy | 14.188 | 9.292 | 0.655 |
| local-miss | 25.980 | 21.834 | 0.840 |
| local-type1-copy | 14.458 | 11.250 | 0.778 |
| full-dense-y | 8,885.834 | 12,198.271 | 1.373 |
| local-type1-refine | 46.667 | 65.229 | 1.398 |

这组有限宿主对照中，大画面copy路径原生更快；dense-Y和局部refine原生更慢，clipped-overlay也未显示收益。不能笼统宣称NEON全面提速，不能以assembly要求为由忽略变慢结果；后续应在对应目标复验并优化明确的热点，不能静默切回标量生产实现。µs级小case、混合opcode、共同libtest开销和有限24点均限制统计解释。

执行前后保存宿主版本和后台负载；当时DevEco/模拟器等存在CPU占用，本项目没有并行编译或UI测试，也没有停止其它任务。此记录不代表独占环境或稳定尾延迟。`host-runs/{environment.json,*.invocation.json,*.result.json,*.stdout.log,*.stderr.log,all-records.json,summary.json}` 保留四个实际命令、逐值apply/publish/total、六组sample、min/median/max及后台观察。RSS/私有capacity/总allocator仍未测。


## PC 模拟器有限目标正确性

同一两份冻结OHOS ELF在API26 ARM64 PC模拟器，分别只执行一次精确wire资格测试，各1项通过、32条record、8state及DONE，实际host退出0且各有唯一guest退出0。官方HDC取回各41个完整文件；原始输出/SDK库/工件hash/权限/来源均保留。root另行核对23条实际成功命令，其中19条HDC、13条guest shell的不可重复退出marker均为0，及82个取回文件当前身份、45+76源和四工件/SDK前后恒等。

PC default与PC native、PC default与宿主default、PC native与宿主native三组各32完整输出+8state、386,897,350字节逐字一致。root重新执行三组完整比较及各自4负对照，均通过；含patch每行与committed framebuffer相等、消息顺序、实际dense-Y seed等检查。这是目标正确性，未执行PC正式计时。

只在独立0700临时目录使用CLI程序，没有网络、输入注入、HAP安装/启动/停止、界面截图或密码/HUKS访问。原等待人工安全输入的PC表单保持。该PC步骤当时没有启动phone，Mac当时锁屏。用户随后手动解锁，后续phone独立资格见下节；这些历史状态不代表当前仍锁屏。

首次公开参数检查因系统返回boot/API尾空格而触发本地helper断言STOP；两条实际公开命令的host/guest已成功。root读取原始输出后，只允许按位置strip并严格白名单核对公开字段，从已有成功receipt接续未执行步骤，未重复原命令、未覆盖STOP或更改产品源码。该历史失败不是客户端或设备加载失败。

实际证据 `target/task5a6-mvs-pipeline-20261015/pc-runtime/`，root接收 `target/task5a6-root-20261015/pc-acceptance/`。正式PC计时仍保留OPEN，不能由其中一个实例推定另一个通过。


## 手机模拟器独立有限目标正确性

用户手动解锁Mac并恢复API26 ARM64手机实例后，使用与上节相同冻结ELF/官方SDK库，严格通过官方HDC指定手机目标。default/native各仅执行一次精确资格测试，各1passed/0failed、32record+8state+DONE，host0和各唯一guest0。各取回41文件；root另行重新核对82文件身份、45+76源及四工件/SDK输入，重新执行三组完整比较与每组4负对照：phone default/native、phone default/host default、phone native/host native，**每组386,897,350字节逐字相同**，14,400个dense-Y seed检查通过。

实际24个command的host均0；14条guest shell有13条唯一guest0，以及保留的一条公开观察guest127。初次inventory helper误要求只出现手机、第二次公开内存观察误用guest不存在的awk，均立即STOP。root分别读取原始记录后只释放清单解析修正及新增一次`cat /proc/meminfo`；没有重发旧inventory/uname/params/df，没有覆盖原STOP/raw或把guest127改写为成功。启动前公开可用内存2,630,264KiB、可用磁盘1,808,680KiB满足资格前置阈值，不能视为进程RSS或分配实测。

没有操作产品UI/账号/HUKS/网络或PC；没有构建、签装或执行正式计时。证据 `target/task5a6-mvs-pipeline-20261015/phone-runtime/`，root独立接收 `target/task5a6-root-20261015/phone-acceptance/acceptance.json`。手机和PC的有限CPU正确性现已分别通过；本节当时尚未执行正式计时。手机计时见下节，PC计时、HAP/GPU/网络及真实Mac性能仍未完成。


## 手机模拟器固定四轮计时（2026-10-16）

root 在不操作PC的前提下，沿用已经分别完成正确性资格的两份原始OHOS ELF与官方SDK C++库，顺序实际执行 default-forward、native-forward、native-reverse、default-reverse。四个独立进程各仅运行一次指定测试，host与独有guest退出均0、各1passed，全部96条measured/32条warmup完成。合计384条measured、128条warmup；每backend192/64，每case/backend24条计时样本。没有增加样本或重跑计时进程。

每条记录在计时后逐字比较完整输出，每cycle比较完整状态；四轮全部通过。执行前后guest两个ELF、SDK库和41个reference文件摘要不变；参考文件也与已接收phone native及host default全部41文件摘要一致。host保存的原始计时文件、DONE和所有命令/实际退出码均单独保留。下表是apply+publish总量中位数，单位µs；各段全部原始值及min/max保存在summary.json，不按结果删除慢记录。

| case | default 中位µs | native 中位µs | native/default |
|---|---:|---:|---:|
| full-copy-720 | 3,533.229 | 2,523.334 | 0.714 |
| full-copy-tail | 4,199.376 | 2,913.312 | 0.694 |
| clipped-overlay | 6.125 | 5.458 | 0.891 |
| local-copy | 27.145 | 22.271 | 0.820 |
| local-miss | 64.667 | 63.124 | 0.976 |
| local-type1-copy | 26.604 | 25.812 | 0.970 |
| full-dense-y | 10,839.604 | 12,754.834 | 1.177 |
| local-type1-refine | 114.854 | 140.438 | 1.223 |

手机同样显示大画面copy收益，以及dense-Y/refine变慢，不能宣称NEON整体提速或由此启用生产后端。局部case含混合opcode、共同测试跟踪/断言开销，且为共享Mac上的模拟器；宿主DevEco、两个Emulator及Codex等后台CPU活动与load average约2.99–3.22被记录，没有停止其它任务或操作PC。没有本项目并行编译或UI测试，**不代表独占宿主、硬件手机表现或稳定尾延迟**。这仍是CPU apply/publish有限对照，不是实际FPS、HAP绘制、网络或远程延迟。

### 原始候选与当前代码边界

这些固定ELF来自 `fb78d4e4347464ee5e58e34d5ccb7fa74aa606d2`，不是当前 `c2e6f90` 重建。root重新逐项将原freeze中45个Apple与76个transitive条目同原Git对象比较，121次校验全部一致，**两组重叠45条，实际76个不同路径**。当前有5个原输入路径变化，且有后续新增文件；保存了实际diff和当前输入摘要，不假定变更与热路径无关。因此本节结果只归属原fb78候选，不授予后续当前源码、安装HAP或真实Mac性能资格。本次各命令前后同时核对当前76个输入及原二进制/参考文件，执行期间没有变化。

本次root辅助脚本有三个执行前停止，均保留：最初误将121次校验当121个不同路径；随后小写文件名白名单误拒必需的DONE；两次均在任何设备命令前退出。修正后公开设备观察host0/guest0，ARM64/API26、MemAvailable2,473,376KiB、磁盘939,268KiB；脚本沿用资格阶段写出大文件的1GiB磁盘门槛再次停止，尚未运行测试。root明确按本阶段每process仅写records.txt和DONE改为128MiB余量，复用15分钟内已成功原始公开观察、不重发该命令。原脚本/STOP全部保留，新的execution-v4只执行剩余步骤；不删除或覆盖guest参考/旧工件，不以这些工具错误或门槛调整增加计时次数。

证据根 `target/task5a6-root-20261015/phone-measurement-20261015/`：admission-ruling.json、current-input-drift.diff、两项工具修正与formal-output-budget-ruling.json；正式执行在execution-v4/{STARTED.json,*.invocation.json,*.result.json,*.stdout.log,*.stderr.log,*-data/,all-records.json,summary.json,COMPLETE.json}。手机只运行原生CLI，不访问产品UI、账号/HUKS、网络、HAP生命周期或PC。PC仍在用户填写阶段，未观察、重装、点击、连接或执行计时；Task5A6整体仍OPEN。
