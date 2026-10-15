# Apple 断开测试的请求归属（2026-10-15）

本记录只涉及测试观察与回归，不改变生产超时、关闭、加密、wire、取消或错误合同。源码、本机验证与独立Spec/Quality复审通过；新CI仍待完成。

## 原始失败和原因

提交6636020的通用CI run38704986760、macos-15 job116322173263，在 `production_session_disconnect_interrupts_blocked_writer_and_runs_cleanup` 失败。完整错误为“设置 Apple writer 写超时失败: Invalid argument (os error 22)”，并非该请求的write_all失败。原始输出保存在 `target/harmony-input-capabilities-20261015/fix1/ci-failed-full.log`。

该测试使用固定2秒写超时，没有absolute_deadline，排除本例零/极小deadline。Darwin在双向shutdown后拒绝后续socket option有[Apple固定XNU源码](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.101.15/bsd/kern/uipc_socket.c)支持；root本机真实TCP也记录同一合法2秒timeout在shutdown前成功、之后errno22，证据为该目录的`darwin-sndtimeo-shutdown.json`。这证明平台机制，不是CI精确syscall交错的抓取。

旧测试有独立sender循环：观察请求A的Started后等待150ms，sender仍可能在A成功后续发B。最终收到的第一个错误可能属于B，甚至发生在其Started之前。因每条请求只在Started之前设置timeout，CI这次timeout设置错误不能属于此前已经Started的同一请求。错误归属必须修复，不能放宽“写入失败”断言或忽略EINVAL。

## 测试修复

仅cfg(test)给Message增加可选请求观察通道，并提供返回该请求独立result RX和观察RX的薄helper。原runtime回归改由主线程每次只提交一条请求；完整消费成功轨迹后才能继续。一旦选定Started后150ms仍pending的请求，就停止续发，Disconnect后只检查该请求RX，同时要求Finished(false)和ResultDelivered来自其独立通道。

原3秒绝对清理界限、Closed、真实写入失败、唯一退出、MediaClosed/ConnectionShutdown/WriterShutdown顺序、Disconnecting先于AudioState::Stopped以及关闭后发送拒绝均保留。没有人工gate、nonblocking切换或探针字节。150ms仍只是同请求在途观察，不宣称线程已睡在内核syscall；同请求迟到成功仍会失败。

新真实TCP回归分别验证1/2字节请求的独立事件与结果，后续普通请求不会进入已完成请求通道，peer精确收到预期4字节。此用例验证观察隔离，不声称模拟内核阻塞。

## 已执行验证

固定Rust1.96、macOS ARM64、隔离target，原runtime聚焦1项、connection聚焦10项通过；预定10次runtime重复全部exit0，无失败后挑选尾次。完整Apple crate为539 unit passed、11项既有ignored，另4+1 integration passed，doc0，进程exit0；两文件rustfmt与root diff检查通过。日志已复制到 `target/apple-writer-request-identity-20261015/`。最初`cargo +1.96.0`被本机非shim cargo拒绝，未启动测试，之后使用明确`rustup run 1.96.0 cargo`；该环境错误没有被算作已执行的失败测试或成功结果。

最终源码SHA-256：connection.rs `2343f4bf3d0f7b4a40905b896ffd4369259f599f4cef3ef0884d7e2ef1d56901`；runtime.rs `9447fd98940af2cac6107391ae9b093082c667ce743230a76334e3c4964be901`。root重哈希与完整diff已保存；独立复审Spec PASS、Quality APPROVE；macos-15原并发CI尚待记录。本机通过不代表Windows/Linux原生cfg、真实Apple会话或Harmony键盘/PC验收。
