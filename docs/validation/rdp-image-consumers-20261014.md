# RDP 原生 image/session 消费者验证

2026-10-14。`848258c`接通严格Bitmap完成计数与八格式image、Bitmap/RDP6/RFX/Set/Stream显式原生消费者；`de6b2bb`修复独立审查发现的QOI/QOIZ分配前门并复审Approved。旧默认None、builder与生产工厂保持，以下结果不是普通HAP、真实Windows/Mac登录或完整codec资格。

## 接线合同

共享RLE parser成功返回真实written/consumed；旧入口丢弃计数，显式strict native要求完整源SW×SH×B并保留typed incomplete/原decoder/allocation source。新private buffer先checked geometry/isize/256MiB与TryReserve，再解析，失败不重试scalar。完整源预算独立于目的crop，支持4K/8K形状，不是2560×1440分辨率上限。

image行接口保留源SW/SH/stride/bottom-up，用SH翻转并按逻辑左上crop；raw整payload含末行padding严格验证，compressed使用tight源行。八格式Raw32与Region32的alpha语义分别保持，RFX只写真实clip∩tile；全部可预测row/格式/目标/能力预检在cursor begin前，一次begin/end。Set依据ext大小与left/top，Stream独立exclusive几何；CODEC_NONE当前tight bottom-up BGR/BgrX是baseline兼容约定，尚非完整wire/live证明。

native no-op/outside/marker/no-tile/empty intersection不伪造原点一像素；真实原点像素仍发布。pending local mouse在首header/bitmap失败前不消费，合法pointer事件顺序、server位置覆盖、cache-miss与既有逐更新副作用保留。不是整PDU回滚。

QOI从同一个真实Decoder读header，检查非零、channels、完整尺寸、ext逐轴一致、crop和预算之后TryReserve/decode_to_buf；QOIZ首次最多16KiB且每次增长先预算门。host独立workspace真实锁定qoicoubeh0.5.0、zstd-safe7.2.4、zstd-sys2.0.16+zstd1.5.7与原lock一致，root/oracle manifests/locks和原fixture不变。该可选codec算法本体仍无生产SIMD资格，产品feature保持关闭；没有证明任意尾随/串接frame完整wire consumption。

## 宿主与独立审查

原版独立process/registry-only graph冻结2624条图像、alpha/crop/RFX/cursor/pointer-order及旧错误记录，19,609,703字节，SHA256`c90e24ae846f89aabe58eebbfd2b9daa552b6787506c95a4db9d3fdb9743ec80`。host实际shipping source涵盖raw padding、RLE/RDP6完整源、64格式组合、clip与pending错误门，初始23session+1alpha-helper两profile通过，16个源码故障变体每profile均被拦截。

修复后真实qoi+qoiz源码debug/release各37/37（14optional+23既有），非cfg(test)library严格编译/clippy通过，8个实际源码故障变体各profile被拦截；此前三项RED在真正分配前由guard终止。旧848与新None的64wire pixels/state/updates digest两profile均为`8f223c2a4d58f0c5`。catalog只复核22337 frozen hashes/精确source allowances与12项inventory，未重跑整个宽codec。

初次审查唯一P2为可选分支先无界分配后检查；修复de6的八文件独立复审确认ADDRESSED，无新finding。默认代码prefix/suffix、manifest/lock、image/RFX/ActiveStage及原fixture逐字节一致。host报告与日志分别在`.superpowers/sdd/2026-10-13-harmony-clients/`、`target/task-4a27b-repair/`；审查批准限各自冻结实现层。

## root实际OHOS执行

冻结de6完整commit`de6b2bbeafea912d6018344e39e8d7f58dba69d3`gitarchive，SHA256`38a9e7466d5248741e02614ef4cde7aac8c1cc4280c910ebc1922910d3db7fc2`，排除并行Linux运行门与后续graphics观察接口WIP。Rust1.96.0、官方API26 linker、offline/locked实际frd-protocol-rdp库构建42.45秒成功。CargoJSON精确选择同次target rlibs；实际shipping session源码追加既有测试，另编实际image alpha-helper，两个AArch64 ELF仅依赖libc.so。没有mock decoder或用手写模型取代真实路径。

手机与PC分别stage、重算执行文件SHA并运行：

| 实例 | image/session消费者 | alpha-helper | guest exit |
|---|---|---|---|
| Phone API26 ARM64 | 23/23（0.86秒） | 1/1 | 两程序FRD_EXIT=0 |
| PC API26 ARM64 | 23/23（0.82秒） | 1/1 | 两程序FRD_EXIT=0 |

相同image/session执行文件SHA256`acdd8b6e417620a1aa27e9ab9d7e07d00cbc30d30163b4389fa0fbecbecd1b55`，helper为`eb03a5078971b6ffc39cf65b12ae5474c42bd172b9dc9508664b4239b5feedd5`。构建日志、实际rlib/源码/argv身份、ELF依赖、设备stage/test日志与结果JSON位于`target/harmony-image-consumers-20261014/`。可选QOI/QOIZ完整wire feature在这次OHOS库关闭；helper只证明像素alpha转换，不能冒充目标QOI解码资格。

## 剩余产品门

上述执行未做普通HAP/GUI/网络认证/密码保存/首帧/input/reconnect或新性能测量，没有改变已安装的普通登录包。Windows/Linux/i686/AMD64/ARM64目标消费者本轮运行仍未完成；此前K单行部分慢项也未被这些正确性结果推翻。

另有已确认阻塞：damage的包围union可以包括未解码间隙或cursor背景，不能充当exact decoded coverage。后续需要portable完成事实、独立内容receipts与baseline路由再审查/目标验证。旧cursor非RGBA及scalar compositor不因八格式内容行获得资格；当前产品软件cursor关闭。codec生产选择、完整包、目标性能及真实互操作保持未完成，完整Task4A2.7未勾完。
