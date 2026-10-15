# MVS 完整记录与状态的 ARM64 差分

2026-10-15，状态 **实验性**。新增三个测试专用用例，默认标量构建与
`mvs-native-arm64`构建执行同一组完整记录、事务与发布序列，比较全部状态和
像素值。只增加`cfg(test)` helper与测试模块，生产函数、native数学与分派、
默认开关、wire、依赖和锁文件保持原文不变。不是鸿蒙Mac应用接线完成。

## 覆盖与比较

- 解码：7次type0、7次type1 commit，mode0–7，非对称AC/DC、相邻预测、
  量化表替换、真实Cr19/Cb14非EOB refinement、cache插入/重用/miss/wrap、
  scan引用、10×9裁剪和偏移子矩形、prepare丢弃与损坏/截断回滚。
- 事务：missing tables、await/reset、同代际foreign owner、兄弟preparation、
  full/opaque提交旋转、表替换和旧代际拒绝；来源和接收方均检查不变。
- 发布：真实type0/type1记录调用实际framebuffer/补丁入口；检查RGB/BGRX、
  全黑与尾部nonblack、重叠copy顺序、新RGB的X=0及已有高字节完整保留。

快照逐值保留两组全部量化表、records、surface尺寸/Option/RGB、全部tile
代际/系数/引用、cache容量及索引/计数/所有present项的全部99字节。None由
固定容量和索引补集表示，区别于Some全零；不比较Arc地址，也不用hash/总和
代替实际值。所有权通过真正接受/拒绝及两侧不变断言证明。

38条、70,802字节规范文本在宿主两种构建之间逐字相同。比较器要求三个指定
测试均实际执行并成功、每个marker恰好一次、完整顺序和值一致。八类变异
（pixel/cache/state单字节、缺失/重复/截断、零测试和非零退出）均拒绝。
这些变异是实际输出副本的负控制，不是八次真实libtest执行失败。

## 真实失败及修正

初版测试误用同一个decoder跨surface尺寸，另误判合法补丁数量，两项真实
失败保留，均修正测试前提，生产逻辑不变。独立审查随后发现截断type0负例
也先被尺寸拒绝，未触达数据解析。修正前先用具体错误期望取得exit101的RED；
随后改为同一10×9 surface内的合法8×8子矩形，精确断言mode5 Y DC→q0 present
位→bit offset8耗尽错误链，保留完整snapshot不变。不能由任意Err替代。

原源码/日志/两ELF/审查材料保留。fix1经独立Spec PASS / Quality PASS复审；
当前准入只采用fix1身份，原失败不重标为成功。

## 最终宿主及实际目标链接

固定绝对Rust/Cargo/Rustdoc1.96，`--locked --offline`，官方OHOS SDK。

| 最终fix1检查 | 实际结果 |
|---|---|
| default / aggregate Release聚焦 | 各3 passed / 0 failed |
| default完整Apple package | lib522、auth4、session1 passed；9 ignored、doc0 |
| aggregate完整Apple package | lib525、auth4、session1 passed；9 ignored、doc0 |
| 格式、diff及精确输出比较 | exit0 |
| OHOS default / aggregate Release libtest | 实际重新编译及最终链接，fresh=false、exit0 |

| 最终OHOS副本 | bytes | SHA-256 |
|---|---:|---|
| fix1-default-release-libtest.elf | 7,131,928 | `09139ab2c53218c42b8ee6e1e8edf44022e6d923a2242c672f14adb88e0f2a7a` |
| fix1-aggregate-release-libtest.elf | 7,142,624 | `7cbfc2ca807d828bd530aeba9b5cf4d1934b46455f0d02380a39809417754f0e` |

均为小端AArch64 PIE，解释器`/lib/ld-musl-aarch64.so.1`，直接依赖
`libc++_shared.so`/`libc.so`。SDK C++库沿用已核对的`f69947af…`。
真实FDK静态库与既有构建缓存复用已记录，没有stub或依赖替换。
主代理独立重算42个crate文件、两个最终ELF及38条实际宿主输出，身份和值一致。

## 手机/PC执行与限制

同一fix1标量与aggregate程序已在手机、PC两个API26 ARM64实例分别执行。
先list确认每份恰有三个测试，再各执行一次，4条命令共12次测试通过执行；
无失败/忽略/重试，四份实际host与独有guest退出均0。每份38条/70,802字节
完整记录，与同设备另一后端及对应冻结宿主输出全部值、顺序逐字相同。

每端只向新的任务目录暂存两个ELF和官方SDK C++库，使用命令局部环境；上传后
和执行后guest三文件hash均匹配。42份源码及主机输入前后不变。主代理独立
解析四份实际raw/退出标记/规范完整值，重算87份运行证据，接受记录在
`target/task5a3-root-20261015/actual-target-test-acceptance.json`。
没有改变应用、系统设置、权限或现有任务目录；暂存库不冒充实际映射库身份。

即使该序列通过，也不代表所有grammar/尺寸/损坏形式、65000次真实cache周转、
18条真实capture重放、完整MVS计算资格或性能。IDCT最终range映射、bitmap
像素热路径及目标性能仍需完成；Apple组合根、普通HAP与真实Mac会话未接入。
Windows手机真实会话是独立的[RDP证据](harmony-rdp-live-20261015.md)。

本地资料：`target/task5a3-mvs-pipeline-20261015/`；主代理独立材料：
`target/task5a3-root-20261015/`。不从编译、测试或暂存库身份推断真实应用能力。
