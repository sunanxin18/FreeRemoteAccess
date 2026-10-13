# RDP 精确内容首帧与恢复接线验证

2026-10-14，源码冻结提交 `fa825f113d513668830ee12abf043a12c5a8b294`。两文件改动经独立审查 Spec/Quality Approved，无P0–P2发现。当前结论是共享adapter实现及OHOS目标测试受限验证，不是普通HAP或真实Windows连接验收。

## 实现行为

真实服务器和本地输入调用使用 `ActiveStage` rich处理接口。私有借用信封绑定执行解码的实际图像和调用前generation；路由没有可替换的第二个图像参数。整批收据索引、唯一一一关联、图像/损伤/内容边界及独立65,536项预算先通过，才允许响应写入或覆盖记账。过时显示的损伤和内容一起丢弃，响应/控制顺序保持；退出时释放输入不发布临时图像。

Presentation damage与实际decoded content独立。初始首帧只由实际内容并集达到完整图像后建立；端点更新即使产生全宽bounding box也不能证明中间像素。光标/no-op不建立内容。恢复完整快照要求已有完整内容知识，提取/入队/结束标记不能创造知识；保持1MiB patch分条、同一revision/一个FullBaseline、最多两次尝试。背压丢失呈现时保留同图像内容，恢复失败不继续增量；新generation及同尺寸重激活清除知识。

共享surface成功接受EGFX更新后只提高revision高水位并使legacy呈现失效，不把EGFX像素算作legacy图像内容。legacy接管必须发布自己的已知完整快照。私有drain的整批结构先检查：独立Reset可用，Damage必须紧邻同session/generation/revision的Boundary；半帧、孤立Boundary和坏尾部在任何发布前拒绝。实际producer在同一锁下成对入队并drain全部，因此没有新增跨批次等待状态或wire/fallback策略。

合法pair中途端口失败保留已经接受的效果，runtime仍错误终止，无回滚或自动重试该会话的承诺。任意反复EGFX/legacy混合切换未获本次资格；后续external producer非递增revision仍按原门拒绝。完整live/production接入需独立验证该边界。

## 固定源码与实际构建

root从精确提交导出完整gitarchive，SHA256 `e3db747bf3c47084c713549f39a79c8967dabe1fb7130f8bf7720e4decf4ff1f`，不包含并行Linux/Windows工具工作树。两源码blob分别为：active_session `552e9b37eaa39a26114d15e0d6f1b9f67dc82a78443e0127fbfbc991723c209f`；baseline `218f95f63eae5c94f4f0e5cbee31ac3647233af96c8bd04b0d711764ca0842e8`。独立审查从冻结副本复算匹配。

使用显式Rust1.96.0 PATH/RUSTC/RUSTDOC与官方SDK API26，offline/locked `cargo test --no-run -p frd-protocol-rdp --target aarch64-unknown-linux-ohos --message-format=json` 在42.15秒完成。测试程序来自Cargo实际test executable输出，ELF64/AArch64，仅依赖`libc.so`，SHA256 `a5bb879afc2acb71afca8defb47218b14f492062a597d10734dea3b11db17984`；不是重写被测逻辑的独立模拟程序。

## 两端执行结果

| 客户端目标 | 实际执行范围 | 结果 |
| --- | --- | --- |
| OHOS 手机 API26 ARM64（独立5555实例） | `decoded_coverage_` 20项 + `baseline::tests` 17项 | 37通过，0失败，两组`FRD_EXIT=0`，设备staging SHA匹配 |
| OHOS PC API26 ARM64（独立5557实例） | 同一冻结程序、同样两个filter | 37通过，0失败，两组`FRD_EXIT=0`，设备staging SHA匹配 |
| macOS ARM64 host | 作者最终active-session39项debug/release、baseline17项debug/release及8个语义mutant | 受限验证；正式Rust1.96，非GUI/远端会话；404项受影响全套属于最后最小结构补充前 |
| Windows/Linux i686、AMD64、ARM64 | 共享portable代码，未增加ISA或平台shell分支 | 本次新adapter目标回归待CI，不用此前b93/37构建替代 |
| macOS Intel | 产品不支持 | 未新增构建或验收 |

实际编码fast/slow Bitmap、完整源crop的raw/packed/RDP6、原始disjoint RFX夹具到真实FrameMailbox/FrameTransactionCompiler，验证端点空洞→中间内容→唯一Startup/后续Incremental。真实光标/输入/no-op、失败解码、整批坏收据、stale display、同尺寸重激活、两次队列恢复失败后再恢复、EGFX接管、坏半帧及合法pair中途端口失败均执行。合成PDU与真实目标本地运行不构成网络登录证据。

全部身份、Cargo输出、构建脚本/日志、ELF动态依赖、两端stage/test日志及guest结果位于 `target/harmony-adapter-coverage-20261014/`。既有Clippy/dead-code告警保留，两个修改文件无新增诊断。

普通HAP没有重建或替换，产品协议目录仍空、连接不可用；没有读取凭据、认证真实目标、改变签名、安全存储、GUI、生产decoder/factory或codec广告。目标性能、其它架构、新包、真实TOFU/HUKS/首帧/刷新/输入/IME/断开重连和系统后端资格仍是完整Task4未完成项。
