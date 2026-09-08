# RLGR1 独立合成 fixture

`freerdp_rlgr1_synthetic.bin` 由 `freerdp_rlgr1_oracle.c` 生成，包含完整4096系数：
前4000项为 `(i % 17) - 8`，随后95项为0，末项为1。无远程截图或会话数据。

C oracle 抽取 FreeRDP commit `54a873e2710710841c6ec2b756df64285ec6e29a`
`libfreerdp/codec/rfx_rlgr.c` 的原始编码函数，保留 Apache-2.0版权声明。
测试 shim 仅提供独立MSB bit-writer、标准类型和内存分配；不调用本项目Rust decoder。

重现（在本目录）：

```sh
cc freerdp_rlgr1_oracle.c -o /tmp/frd-rlgr-oracle
/tmp/frd-rlgr-oracle /tmp/freerdp_rlgr1_synthetic.bin
cmp freerdp_rlgr1_synthetic.bin /tmp/freerdp_rlgr1_synthetic.bin
```

该fixture证明完整非零分量的独立编码parity，不证明Windows终端零游程语法。

原始 pinned `rfx_rlgr.c` SHA-256：
`33e1feacef7e2ca0c43f755442f7f429a2f34286d6dbc9c1f0db54d52549cc26`。
生成的2269字节 fixture SHA-256：
`97a56f1abf1d79a08dd2f86dd12c7a10ebeaa478b7be735e1aa7a725fd5a1b9b`。

## 终端零游程的只读证据（2026-09-08）

微软公开测试实现固定为 WindowsProtocolTestSuites commit
`2447a665072cf6849a9bc88eb70c7fa4b618ea7d`。以下文件已用精确 commit URL
重新下载，与初次读取内容逐字节比较一致：

- [RLGREncoder.cs](https://github.com/microsoft/WindowsProtocolTestSuites/blob/2447a665072cf6849a9bc88eb70c7fa4b618ea7d/ProtoSDK/MS-RDPRFX/Codecs/RemoteFXCodec/RLGREncoder.cs)，SHA-256
  `1f1ac6f0a1f369879320ae098f5e7073d1664a8bc0373708ea44065d4bfc533a`。
- [RLGRDecoder.cs](https://github.com/microsoft/WindowsProtocolTestSuites/blob/2447a665072cf6849a9bc88eb70c7fa4b618ea7d/ProtoSDK/MS-RDPRFX/Codecs/RemoteFXCodec/RLGRDecoder.cs)，SHA-256
  `7723874446372881a00323faf96b0f55c9f71dafb8467fd27c604e668ebc1d2a`。

| 实现 | 终端零游程与停止行为 |
| --- | --- |
| 微软 encoder，第217–250行 | 统计全部末尾零，写游程终止位和余数；只有随后输入非零才写sign/GR。因此完整零游程后的short形式具有公开微软实现证据。缓冲区初始化为零，只返回有效位数向上取整的字节数。 |
| 微软 decoder，第193–230行 | 剩余系数数目归零后不再读取sign/GR；按输出系数数目停止，不验证剩余位或字节。WriteZeroes也按剩余输出裁剪。 |
| [FreeRDP pinned encoder](https://github.com/FreeRDP/FreeRDP/blob/54a873e2710710841c6ec2b756df64285ec6e29a/libfreerdp/codec/rfx_rlgr.c#L684)，第684–718行 | 末尾零计数不包含最后输入项，随后仍写sign=0与GR(0)；其decoder将最后符号解释为+1。这不是“完整4096零游程后附加dummy”的独立生成证据。 |
| FreeRDP pinned decoder，第200–383、570–580行 | 完整读取游程、sign和GR后裁剪写入至输出长度；输出已满后不验证残留，输入不足时还会补零。不能照搬这些宽松行为作为生产边界。 |

[MS-RDPRFX RLGR概述](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdprfx/0a20d8cf-588a-4194-9133-35a47e2766f6)
明确每个tile分量有4096系数；本次访问
[RLGR1/RLGR3伪代码页](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdprfx/6dacc5a6-c6d9-4259-abf0-0a27b1f48bac)
只返回标题，未取得正文，不能据该页声称规范要求终端dummy或允许任意尾部。
MS-RDPEGFX的SRL/RAW升级位流规则不能替代首次RLGR流的终止规则。

真实Windows失败分量的尾部仍未知。`entropy trailing data`可能来自额外终端符号、
额外完整字节、非零对齐位，或前面算法偏离导致提前到达4096系数。
官方decoder容忍残留只证明实现兼容差异，不授权忽略任意尾数据。
后续必须用实际失败位位置和独立系数parity区分这些情况，再制作无远程数据的最小结构fixture；
不得隐式补缺失系数，也不得把上述假设标为live实证。

当前没有安装可用的dotnet/csc/mcs，因此本轮只读审计微软C#源码，未运行微软oracle，
未安装额外依赖。现有C oracle的运行证据仍只对应上面的FreeRDP合成fixture。
