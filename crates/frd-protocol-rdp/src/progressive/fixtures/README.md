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
