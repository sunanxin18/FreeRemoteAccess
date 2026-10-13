# Session 精确解码覆盖验证

2026-10-14，源码 `13f1bccfcee49429aca75afa796e0131b74a8143`。十一文件实现经独立审查 Approved；此门区分真实内容与展示损伤，尚未把新接口接入 adapter/baseline，完整首帧和生产注册仍待完成。

## 接口和行为

新增 `image::ActiveStageProcessResult` 及 `DecodedCoverageReceipt`，实际图形输出索引明确绑定其成功内容矩形；每个 GraphicsUpdate 有一个 receipt，光标和空操作的内容为空。既有 Stage/Processor 方法、enum 和默认选择保持，旧入口使用禁用 collector，不运行新增预算或严格几何门。

rich 路径使用同次实际 packed written/consumed、RDP6 完整源形状；成功欠填充在 image apply 前失败，decoder Err 的历史 warn/damage 不生成内容。完整 SW/SH、raw 对齐行及最终 padding、compressed tight 行、SH bottom-up 与逻辑左上 crop 经过检查，再复用原像素算法；不以 DW 重新切源行或以 DH 代替源翻转高度。Set/Stream 几何独立，默认和显式 native 均可处理合法大源裁剪。

独立网格测试发现原 Region union/intersect/simplify 可丢失单行离散区域或缩短不等高 band。rich RFX 的像素写入和 receipts 因此消费同一个有界精确 tile×clip 交集列表，保留重叠/重复，不经过有损合并。旧入口保留历史 Region 行为。cursor 擦除/重绘、位置/形状/cache/local input 不成为新内容。

单次 process 整批最多65,536内容矩形和65,536 receipts，checked/fallible reserve尽量先于对应apply。raw矩形payload至多512KiB，receipt头在64位ABI至多2MiB，RFX clip/交集及原decoder/framebuffer各有独立开销；不能把512KiB当整进程上限。Err不返回receipts，但之前成功像素/鼠标动作可能保留；调用者必须终止或重置，不是整PDU回滚。

## 宿主与来源

固定Rust1.96 actual-source默认debug/release各37项，真实QOI/QOIZ optional各52项，均通过窄严格clippy；四种非test分支编译通过。9个实际source故障变体在两个profile编译成功并被语义断言拒绝，涵盖bbox/cursor/SW-DW/SH-DH/len-as-written/预算/索引/有损writer。独立原版2,624-record图像fixture身份、原source/archive、22,337-record catalog及allowlist保持；新源post-hash和新增测试hash按原provenance机制记录。

独立审查完整201,144-byte冻结diff和十一文件hash，核对共享几何及actual-source harness，没有重复绿色套件。宿主日志位于 `target/task-4a28b/`；optional证据仅为feature范围，未批准其生产SIMD资格。

## 实际 OHOS ARM64 两设备

root独立git archive SHA256 `c6823f05915fdf1588a1281c011ce23d7808d72ebd71e2bfc10a57fbdc87bf4f`，逐项匹配十一源文件hash，排除并行Linux工具修复。固定Rust1.96、DevEco API26 Native SDK、offline/locked构建实际RDP/session/graphics库成功，39.36秒；Cargo JSON确定全部实际目标rlib与host proc-macro路径。

源码harness直接包含冻结实现及14项新增、23项既有测试。实际AArch64 ELF SHA256 `ead17f634afbe27c9078f203e5d523499aa256de82ca112247ba9b852b7dd1db`，动态依赖仅 `libc.so`。手机 `127.0.0.1:5555`、PC `127.0.0.1:5557` 分别发送/hash核对后运行，各37项通过，0失败，实际 `FRD_EXIT=0`。覆盖真实fast/slow报文端点gap、八格式大源裁剪、独立RFX像素/交集、不等高与重复clip、cursor/pending mouse、错误/欠填充、实际fragmented整批65,536/65,537预算和分配失败。完整身份、编译/依赖/设备日志位于 `target/harmony-decoded-coverage-20261014/`。

本次没有安装HAP、输入真实凭据、连服务器、修改平台签名或运行性能基准。adapter仍使用旧damage覆盖路径；必须迁移后证明gap不建立完整首帧、已知内容恢复和代际隔离。该目标运行不证明Windows/Linux或普通HAP/live/首帧/输入/完整decoder生产资格。后继保持两设备分别验证及既有桌面回归。
