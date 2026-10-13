# Portable 解码完成事实验证

2026-10-14，源码 `6543e077621212e23b275e1ce0f52097f3ef5d77`，六文件窄实现经独立审查 Approved。本门提供实际解码事实，尚未接入 session 的精确内容覆盖或 baseline，未改变默认 decoder、生产注册、wire 广告或 UI。

## 接口与兼容性

`rle::decompress_observed` 与原 public wrappers 共用同一次 scalar parser，返回真实源/目标 cursor 的 `consumed` 和 `written`。成功欠填充仍返回成功事实，不能从预分配长度或零像素推断完整。旧 helper 的 resize、错误优先序、私有前缀与历史成功结果保持。

`BitmapStreamDecoder::decode_bitmap_stream_to_rgb24_observed` 调用一次原 scalar decoder，仅在成功且正尺寸、checked RGB/平面上界成立后返回完整源形状。证明来自全部平面和 RGB writer 完成；它不是解码前资源安全门。未来 rich session 必须先检查几何、完整源资源与正确 crop，packed 成功欠填充须在 image apply 前失败，避免未知尾部覆盖已有内容。

宿主窄测试 debug/release/clippy 各5项通过；两种模式各53项独立registry原版快照一致，SHA256 `036a237fdc1d8116ae9e9b7416978342c781304e45319f5903b03f6cf5f0bee2`。实际cursor三种故障变体均被断言识别；旧packed7项、planar20项及原来源catalog检查通过。原oracle图、源码inventory、归档和夹具没有重生成。

## 实际 OHOS ARM64 手机和 PC

root从上述commit生成独立git archive，SHA256 `08cbe687e53cd390b302205da7c688f116bf935875439f47537ab4ccdf777668`，排除并行session工作树。固定Rust1.96.0、DevEco API26 Native SDK离线/locked构建实际 `frd-protocol-rdp` 及其graphics依赖成功，34.02秒。Cargo JSON确定实际目标rlib与host proc-macro路径，窄source wrapper直接包含该冻结源码和原测试，不复制解码公式。

实际AArch64 ELF SHA256 `6355e02081a263b699e7ebe1899888958c27cd47fbbd900d52b01a6a16d4f521`，动态依赖仅 `libc.so`。安装测试程序后独立检查设备hash，手机 `127.0.0.1:5555` 与 PC `127.0.0.1:5557` 分别运行 `completion_ --test-threads=1`，各5项通过且实际 `FRD_EXIT=0`。测试覆盖真实计数、欠填充/错误私有前缀、固定packed原版记录、RDP6平面/append与完成shape；不是人为注入成功计数。

首个私有harness编译因遗漏host proc-macro依赖搜索目录失败；保留该诊断后加入同次Cargo输出的host依赖目录，成功编译。没有重建另一份库、修改产品源码或放宽测试。构建/编译/依赖、source/rlib/ELF身份、设备发送/hash/执行记录位于 `target/harmony-completion-facts-20261014/`。

本次没有安装HAP、修改登录信息、连接目标、改变系统签名配置或执行基准。完成观察接口的目标测试不证明 session receipt、完整首帧、输入、生产软件解码SIMD资格、其他架构或真实Windows/Mac互操作；这些仍是后继独立门。
