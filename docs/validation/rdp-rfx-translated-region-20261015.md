# RDP RemoteFX 偏移区域裁剪回归（2026-10-15）

状态：**受限验证**。修复共享 IronRDP 会话层的确定坐标错误；不改变能力广告、软件计算内核或平台窗口。

## 根因与确定性证据

原 `checked_clipping_rectangles` 先给编码区域加上桌面 destination 偏移，再拿局部 channel 的 width/height 截断，混用了两种坐标空间。既有冻结的 `session-end-2970` 64×64编码夹具放到96×96图像的(16,24)位置时，本应4096像素，旧版只到(63,63)，丢2176像素。rich Compatibility 与 native 两条生产使用路径均能复现。该错误足以导致局部更新右侧/下方缺失，但不能仅凭夹具断言所有实机刷新问题都由它造成。

修复先在局部 channel 中求交，再使用u32平移，与 inclusive destination 及实际image求交；空交集跳过、最后检查窄化。tile边界的 `+64-1` 改为 `+(64-1)`，避免合法65535边缘的中间溢出。原非rich scalar历史参考保持；实际adapter使用rich coverage接口。

规范依据：[channel尺寸](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdprfx/4060f07e-9d73-454d-841e-131a93aca675)、[region](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdprfx/23d2a1d6-1be0-4357-83eb-998b66ddd4d9)以及[tile与region相交后blit](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdprfx/1efe8ac9-88dc-438e-9388-c4424b7a4fcf)。Stream Surface Bits外层右/下排除边界在现有入口已减一，本次没有再次减一。

## 验证

- 旧实现 RED：真实编码夹具、独立像素oracle和精确decoded coverage均报上述缺失；不是仅damage包围盒测试。
- 修复后原生回归runner：3 core + 15 session通过；覆盖正常平移、图像边缘、较窄destination、全离屏和大偏移，两种backend逐像素/coverage一致。
- `cargo test -p frd-protocol-rdp`：352通过、0失败、11既有ignored；doctest通过。独立审查PASS并独立重跑现有二进制5项native测试通过。
- 扩展coverage runner另遇既有两个 `#[global_allocator]` 的harness编译冲突，本次未更改runner，也不将该次运行计为通过。
- OHOS ARM64双协议普通Release HAP构建、官方签名/验签/checker成功；unsigned ZIP所有原条目签后逐字保持。手机覆盖安装成功，保留HUKS；未以源码测试替代设备验证。

unsigned SHA256：`06779659fb573678694519f975f486c1a2945d33e4f6fc253854d3b2b7f90022`。
signed SHA256：`298cfe7c3d1054400994b41c1bc2a5816b623c183bfcee9ddce3f80aba853a63`。

## 手机实际观察与边界

API26 ARM64手机模拟器用HUKS保存连接进入真实Windows桌面，2760×1256全屏保持。15:43:19第一次开始菜单截图仍在分批更新；15:43:34独立截图已经显示完整菜单到底部。15:43:54点击关闭后，15:44:12截图显示完整PowerShell桌面，没有此前局部菜单文字残留。中间截图不能冒充最终失败，最终完整也不能证明帧延迟已经合格。

没有从画面猜测Bitmap/RemoteFX占比，没有启用H.264/HEVC。当前普通组合仍为Unix/RequiredNative/LegacyOnly。精确codec耗时、网络/呈现延迟、真机、PC真实会话以及所有桌面架构实跑仍未在本次证明；共享代码没有新增架构分支，native/兼容路径均有离线回归，macOS仍仅ARM64。

本地证据：`target/harmony-partial-refresh-20261015/`；RED/final日志分别为 `target/rdp-partial-refresh-rfx-red-20261015.log` 与 `target/rdp-partial-refresh-rfx-final-20261015.log`。
