# Apple 断开测试的请求归属（2026-10-15）

本记录只涉及测试观察与回归，不改变生产超时、关闭、加密、wire、取消或错误合同。请求归属修复保留；前两版测试资格被CI反例否定，最新方案改为准确的会话清理合同并补强真实大块写入失败。新方案本机验证、独立复审以及05d525的六类CI工作流均通过；具体目标和各阶段终态见末节。

## 原始失败和原因

提交6636020的通用CI run38704986760、macos-15 job116322173263，在 `production_session_disconnect_interrupts_blocked_writer_and_runs_cleanup` 失败。完整错误为“设置 Apple writer 写超时失败: Invalid argument (os error 22)”，并非该请求的write_all失败。原始输出保存在 `target/harmony-input-capabilities-20261015/fix1/ci-failed-full.log`。

该测试使用固定2秒写超时，没有absolute_deadline，排除本例零/极小deadline。Darwin在双向shutdown后拒绝后续socket option有[Apple固定XNU源码](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.101.15/bsd/kern/uipc_socket.c)支持；root本机真实TCP也记录同一合法2秒timeout在shutdown前成功、之后errno22，证据为该目录的`darwin-sndtimeo-shutdown.json`。这证明平台机制，不是CI精确syscall交错的抓取。

旧测试有独立sender循环：观察请求A的Started后等待150ms，sender仍可能在A成功后续发B。最终收到的第一个错误可能属于B，甚至发生在其Started之前。因每条请求只在Started之前设置timeout，CI这次timeout设置错误不能属于此前已经Started的同一请求。错误归属必须修复，不能放宽“写入失败”断言或忽略EINVAL。

## 测试修复

仅cfg(test)给Message增加可选请求观察通道，并提供返回该请求独立result RX和观察RX的薄helper。原runtime回归改由主线程每次只提交一条请求；完整消费成功轨迹后才能继续。一旦选定Started后150ms仍pending的请求，就停止续发，Disconnect后只检查该请求RX，同时要求Finished(false)和ResultDelivered来自其独立通道。

原3秒绝对清理界限、Closed、真实写入失败、唯一退出、MediaClosed/ConnectionShutdown/WriterShutdown顺序、Disconnecting先于AudioState::Stopped以及关闭后发送拒绝均保留。没有人工gate、nonblocking切换或探针字节。150ms仍只是同请求在途观察，不宣称线程已睡在内核syscall；同请求迟到成功仍会失败。

新真实TCP回归分别验证1/2字节请求的独立事件与结果，后续普通请求不会进入已完成请求通道，peer精确收到预期4字节。此用例验证观察隔离，不声称模拟内核阻塞。

## 已执行验证

macOS ARM64、隔离target，原runtime聚焦1项、connection聚焦10项通过；预定10次runtime重复全部exit0，无失败后挑选尾次。完整Apple crate为539 unit passed、11项既有ignored，另4+1 integration passed，doc0，进程exit0；两文件rustfmt与root diff检查通过。日志已复制到 `target/apple-writer-request-identity-20261015/`。最初`cargo +1.96.0`被本机非shim cargo拒绝，未启动测试；随后使用`rustup run 1.96.0 cargo`。后续调查确认该cargo仍来自Homebrew，子rustc沿PATH使用1.98.1，因此此前“固定Rust1.96”的表述撤回。这些本机结果保留，但不能作为1.96证据；后续背压修复使用绝对cargo/RUSTC/RUSTDOC路径重新验证。

最终源码SHA-256：connection.rs `2343f4bf3d0f7b4a40905b896ffd4369259f599f4cef3ef0884d7e2ef1d56901`；runtime.rs `9447fd98940af2cac6107391ae9b093082c667ce743230a76334e3c4964be901`。root重哈希与完整diff已保存；独立复审Spec PASS、Quality APPROVE；macos-15原并发CI尚待记录。本机通过不代表Windows/Linux原生cfg、真实Apple会话或Harmony键盘/PC验收。

## 新CI反证与继续调查

最终测试修复提交`01916bc`的run `38712979860`、macos-15 job `116348840251`真实失败：同一请求在`runtime.rs:2901`返回Ok，触发“同一在途请求在Disconnect后不应迟到成功”，Apple538通过/1失败/11ignored。ubuntu safe job116348840079通过，不能覆盖Mac失败。原始225495-byte输出及job步骤终态已保存到`target/apple-writer-request-identity-20261015/ci-failed-full.log`和`ci-macos-job-followup.json`。这次证明身份关联已生效，尚未证明150ms pending是真实阻塞；不能以本机10次和完整通过宣称竞态验收完成。

继续只读定位真实socket写就绪/背压与peer生命周期，保留同请求成功即失败、原三秒清理及所有错误断言，不直接重试CI直到绿。前版ebe8b4b通用CI曾成功，只能说明旧问题有调度敏感性，不能抹去6636020与01916bc两份失败。

## 背压判定与测试 peer 生命周期修复

后续候选仅修改 `runtime.rs` 的测试、Apple crate 的目标测试依赖及锁文件。保留真实 TCP 的只读 clone，以 Unix `poll` / Windows `WSAPoll` 做零等待写就绪查询；ERR/HUP/NVAL 优先归为关闭或错误。只有同一请求的结果和事件在查询前后都 pending，且 socket 此刻不可写，才选择该请求并断开。可写时等待本请求完成，消费完结果及事件后再发下一条。没有改变共享 socket 的阻塞模式、读写超时或内容，也不发送探针字节。

测试 peer 原来在 ready 后五秒自动关闭，会与主线程的准备和清理阶段竞争；现在由显式 release 或主线程退出导致的 sender drop 释放。主线程原两秒准备界限、三秒绝对清理界限、同请求迟到成功即失败、精确写失败、唯一退出和完整清理顺序均保留。依赖只增加 Unix `libc = 0.2.189` 与 Windows `windows-sys = 0.61.2` 的 dev-dependency 边，未升级包版本或改变生产依赖。

本机 macOS ARM64 使用绝对 Rust1.96 cargo/RUSTC/RUSTDOC 验证：原断开聚焦1项、真实TCP就绪回归1项、connection10项和预定10次重复全部通过；完整Apple540单测通过、11项既有ignored，另5项集成通过，doc0。新TCP回归验证初始可写、真实64MiB写入的背压、shutdown关闭分类和写失败；Unix额外确认阻塞flags及写超时未变。最终 runtime SHA-256 为 `7d96f03b8fad4c4402cc3e25068a30426d69fa87403b8f957644f71951d0608e`。

Windows/Linux各i686、x86_64、ARM64的最终完整helper及probe测试源码通过隔离cfg/type检查，未链接或运行。完整Apple Windows跨检查被 `fdk-aac-sys` 的目标C++头文件缺失阻断；早期Windows SOCKET类型不匹配的RED和误用Homebrew sysroot的失败均保留。不能据隔离helper检查称完整Windows crate已通过。证据及全部29份日志在 `target/apple-writer-backpressure-20261015/`。独立审查读取完整差异及日志，Spec PASS、Quality APPROVE；root核对四文件冻结身份及实际结果，新CI尚待执行。

零等待不可写仍不是原子证明writer已睡在内核：最后一批数据刚填满缓冲但Finished尚未发布的窗口仍可能存在。原迟成功失败断言继续捕获该反例；不通过放宽结果或重试至绿消除失败。本记录不证明真实Apple会话、Harmony键盘或PC登录已验收。

### 背压候选的新 CI 反例

`df0672b` 的 run38716468501、macos-15 job116360378045 于10:01:24 UTC结束失败：原断开回归在 `runtime.rs:3104` 再次触发同请求迟到成功，Apple539通过/1失败/11ignored。日志明确记录选中时“已完成请求数:0”；新socket probe测试通过仍不能证明这个60,034-byte请求有剩余数据尚未被内核接受。它进一步证实上述完成通知窗口仍会发生，不能把零poll资格当成测试问题已经解决。

完整225851-byte失败日志保留为 `target/apple-writer-backpressure-20261015/ci-macos-failed-full.log`，步骤终态为 `ci-macos-job.json`。首次job logs API读取失败也已记录；随后按具体job读取成功，没有重跑CI。继续只读设计更强的真实I/O准备条件；本机通过与独立审查通过只对应其已声明的范围，不能覆盖此次CI失败。

## 第三轮：修正测试预期并保留严格的真实 I/O 回归

源码及独立策略审查确认：生产writer没有追溯取消已经成功的`write_all`的合同。Started在syscall之前，Finished在其后，完成通知可能晚于Disconnect；无条件要求这个请求Err是过强的测试预期。root撤回此前“不接受同请求迟到Ok”的要求，并同步撤回runtime已证明内核阻塞取消的表述。Darwin独立实验还观察到SO_RCVBUF请求1024会增长到523648，瞬时poll或窗口/MSS快照均未给出未来全链路容量上界；这些调查未成为新生产依赖或固定五帧规则。

runtime回归更名为`production_session_started_request_disconnect_race_runs_bounded_cleanup`，单条60000-byte合法加密请求在同请求Started后提交Disconnect。结果为Ok时必须配对唯一Finished(true)，Err则必须是外层精确“写入失败（连接中断？）”、唯一真实I/O cause，且排除TimedOut/WouldBlock，并配对Finished(false)。两支均要求ResultDelivered、两个通道最终断开及完整共同清理，无提前返回。原2秒准备、同一3秒绝对清理界限、Closed、唯一退出、清理顺序、状态事件顺序及关闭后发送拒绝全部保留；peer只由显式释放/drop结束。

connection的64MiB真实TCP用例仍严格要求失败，更名为`shutdown_fails_large_tcp_write_and_releases_queued_callers`；增加请求独立结果/事件匹配，并同样拒绝无I/O cause、设置超时失败、TimedOut/WouldBlock。它保留两个queued callers、并发/重复shutdown和关闭后拒绝，接收结果共用三秒界限。自然2秒写超时不能冒充shutdown影响。成功隔离用例保留；新增错误判据负例测试，移除无consumer的poll helper/probe及其两条测试依赖边。删除1个probe、增加1个负例测试，单测总数净变化0。

最终绝对Rust1.96 cargo/RUSTC/RUSTDOC、新隔离target：四项focused各1通过；预定10组runtime与64MiB配对共20/20进程exit0；完整Apple540通过、11项原ignored、集成4+1通过、doc0。两文件生产前缀逐字比对一致，新增共享错误判据仅cfg(test)，Cargo.lock只撤Apple两条dev依赖边。当前connection SHA-256为`7344157eb0ab56715c22decd911e929ae139660ac83ef9322d7f9b0be63b607e`，runtime为`fb27a377617ed1688485509aa012ab743893c95ae5938a96847cb3fc9e351814`。完整源码、日志、负例、生产比对和三轮调查在`target/apple-writer-fix3-20261015/`及前述历史目录保留。

独立最终审查Spec PASS（补强B合同）、Quality APPROVE，root另行核对四文件冻结、生产前缀、20份配对终态及完整测试。提交前diff检查曾发现Cargo.toml新增EOF空行，原作者仅移除该空行，最终diff检查通过；依赖语义及其余源码未变，没有重复已通过的同源测试。提交前新CI尚未完成，当时不能宣称原macos-15场景已通过；提交后的确切结果见末节。

这些分层结果不证明特定加密runtime请求确定阻塞于内核时被取消，也不覆盖runtime同步等待writer而尚未dequeue Disconnect的路径，或其他系统相同负载的内核因果。真实TCP大块写入失败与生产session完整清理是独立证据，不能合称上述更强目标已完成。当前未改变生产取消语义；本机矩阵本身不等于macos-15或Windows/Linux原生CI通过，远端结果另见末节。

## 05d525 的远端 CI 终态

2026-10-15，提交 `05d525089323250363289ee32de6fa53cdd893d7` 的[通用 CI run38721492728](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38721492728) 已于10:42:52 UTC成功完成。Rust formatting job116376872870、macos-15 safe tests job116377000918、ubuntu-24.04 safe tests job116377000935 均为success；这是第三轮准确清理合同在这两个CI目标的通过记录，没有重跑前两轮失败任务。原始请求身份、迟到成功及背压反例全部保留，不被这次成功覆盖。

同提交Linux GTK run38721492734及GL run38721492666成功；本次读取时Windows包38721492735、macOS包38721492653、Linux client/FFmpeg包38721492649仍在运行，不提前宣称完整打包通过。精确head/job终态保存在 `target/apple-writer-fix3-20261015/ci-followup-2.json`。这些CI结果不扩大上述未证明的加密阻塞取消、跨负载因果或真实设备互操作范围。

后续同提交macOS ARM64 package run38721492653（job116376871652）于10:51:52 UTC成功；Linux client/FFmpeg run38721492649的i686 job116376903546、aarch64 job116376903600、x86_64 job116376903873均成功，run于10:54:38 UTC结束。Windows三架构包仍在运行。证据为 `ci-packages-followup-4.json`；macOS没有新增Intel目标。

最终[Windows package run38721492735](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38721492735)于11:20:01 UTC成功：x86 job116378650504、x86_64 job116378650953、ARM64 job116378651092三个包任务，以及Windows ARM64 native decoder runtime job116390403259均为success。元数据保存在 `ci-windows-followup-6.json`。至此05d525的通用CI、Windows包、macOS ARM64包、Linux client/FFmpeg、Linux GTK及Linux GL六类工作流全部成功；这是该精确提交的自动检查，不代表随后开发的IME桥或鸿蒙真实输入验收。
