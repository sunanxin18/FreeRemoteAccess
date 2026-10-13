# Windows ARM64 固定 H264 核的来源与容器身份

2026-10-14。`9e773ce` 引入独立 COFF/PE 校验工具，`7c50370` 和 `42d3403` 修复两项独立审查发现；第二轮复审 Approved，开放 P2 已全部关闭。这是 Windows 新配方的先决接口，不是 Windows 修改版 FFmpeg 已构建或可发布的结论。既有 Windows i686、AMD64、ARM64 upstream 包和默认行为未改。

## 接口与固定身份

工具从实际 ARM64 COFF 符号、独立源文件端点证明、同次链接 map 与最终 PE RVA 关联私有函数。固定 `ff_h264_luma_dc_dequant_idct_neon` 为276字节，SHA256 `1e70d378b34fb196afd0942b9bd0c31c1715b4042c6c3e8f032dad5e76b42476`；没有修改源配方、原数学代码或核 pin。无法用真正 function Aux 证明范围时，使用固定原源码与受控编译器重放/私有起止标记证明；所有可执行 section 和原对象保持一致，不能把相邻符号距离或 section 大小当函数长度。

最终 PE 检查独立枚举 normal import FirstThunk、delay IAT/module handle/unload、TLS index 与已支持 DIR64 relocation 的静态写入范围，禁止与核重叠。IAT directory 缺失或错指不会绕过权威 descriptor。普通 TLS 和合法 zero-fill 数据保留；未支持的 load-config、CLR 等机制明确拒绝。此范围不证明任意 TLS callback/DllMain 的程序行为。

源输入门拒绝 label、semicolon、动态 macro 和 quoted opcode 中的外部 assembler 读取；quoted operand/comment 仍合法。CPP 前直接验证物理 config.h 的生成配置域，拒绝 raw 汇编、行号重定向及外部 CPP include/pragma。实际依赖来自同次受控 Clang 成功调用的 `-H` physical include trace 和主源码 argv，再逐一绑定配置/归档 bytes；不使用可伪造的逻辑 `#line` owner 或 record 自报的 trace 授权。

## 已执行的验证

- 初始30项 COFF/PE/source/map 合同测试及实际安装 Clang/lld/SDK llvm-ar 生成的容器夹具；只属交叉编译与格式证据。
- 第一轮41项受影响 suite，通过 normal/delay/TLS malformed/range/overlap 与正常布局，并先用真实 Clang 确认5种外部 reader 配置可编译后验证拒绝。旧 helper 复现9个应拒绝却接受的 subcase。
- 第二轮只运行14项实际受影响 assembler/source suite，38.308秒全部通过；旧 `7c50370` 复现4个 quoted opcode/owner spoof subcase。重复/guarded physical include、伪逻辑 owner、失败 CPP、合法 literal/comment、固定 source→object→map→final chain 均覆盖。没有为扩大计数重复数学或 PE 宽边界测试。
- Root 独立核对最终两个报告、三文件哈希和冻结归档；PE class 与第一轮快照逐字节相同。独立复审核对修复及其新增边界，没有替代或重复作者 suite。

冻结 `42d34030c0785c482a8cf69058de398f632b7cd2` 完整源码和身份清单位于 `target/windows-kernel-fix2-20261014/`。最终 helper SHA256 `4a6e2313d27df2803b0baac80949b44a6aff9ee7c7c4aa24f4379638771261a8`；tests `ef37444924063bb604193a066cb7e877d136873daca9d74091af6c13c0e9e9f6`。本轮未执行 Windows 原生代码、改签名、使用远端凭据或改变产品广告。

## 后继必须完成的工作

真实固定 llvm-mingw 编译器的 generated config、自然 AS argv、COFF、GNU link map、strip/installed/package DLL 必须形成同一次构建的实际链；遇到本工具支持域外的结构须保留失败并审查扩展。还须 Windows 私有保护页/QPC/非叶 ABI 与 unwind 夹具、同 shipping 对象的原 C 数值/30分派/ABI/基准、普通最终包真实装载与 codec fixture。Windows 包装/安装器受信 verifier 和对应源码材料尚未接入新配方，完整软件 codec 与真实协议互操作也未完成。
