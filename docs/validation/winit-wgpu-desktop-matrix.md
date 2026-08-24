# winit + wgpu 桌面客户端验证矩阵

## 2026-08-25 构建证据

- 提交：`b472a0f7eb8af97eb54d03d2180205c6fec0dc6f`
- 分支：`feat/five-platform-client`
- Rust：`1.96.0`
- GitHub Actions：[`32764244192`](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/32764244192)
- 结果：`verify`、`windows`、`macos`、`linux` 全部成功。
- 所有下列发布文件都使用下载工件内的 `.sha256` 文件重新校验，结果均为 `OK`。

| 客户端 | 原生 runner | 发布文件 | 字节数 | SHA-256 | 签名 | 启动证据 |
| --- | --- | --- | ---: | --- | --- | --- |
| Windows x64 | `windows-2022` | `FreeRemoteAccess-0.1.0-windows-x64.exe` | 26,841,600 | `0c83cacfb21dadcdad3342cba31ca2e0b272929e5140bc78a262d03c62b10fc5` | 未签名 | Windows 11 本机启动，窗口标题为 `FreeRemoteAccess`，进程正常响应 |
| Windows x64 | `windows-2022` | `FreeRemoteAccess-0.1.0-windows-x64.msi` | 9,166,848 | `c13139fa31567307925e171abe81fe60d7c67e57a73d00a8f510d76d3a7227cb` | 未签名 | WiX 构建和 MSI 内容检查通过；未执行安装写入 |
| Mac OS universal | `macos-15` | `FreeRemoteAccess-0.1.0-macos-universal.dmg` | 26,670,554 | `d6463831db9435685fefec1f3c05c4d325eca4f3512c60f74472e2a87b095ac0` | 未签名、未公证 | CI 原生打包成功；当前 Windows 开发环境不能启动 Mac GUI |
| Mac OS universal | `macos-15` | `FreeRemoteAccess-0.1.0-macos-universal.pkg` | 23,440,191 | `f4412eadbb1e5f6816ade4b958ac3a7e29a65109837b98d2c04e0a57c91d7a30` | 未签名、未公证 | 内含 Mach-O 已核验为 `x86_64 + arm64` universal |
| Mac OS universal | `macos-15` | `FreeRemoteAccess-0.1.0-macos-universal.zip` | 23,439,091 | `a7e7155e292bb90faebda4038bc8dbbaf25b9cbc5b15d721d9a9af13a68440b2` | 未签名、未公证 | 内含 Mach-O 已核验为 `x86_64 + arm64` universal |
| Linux x86_64 | `ubuntu-24.04` | `FreeRemoteAccess-0.1.0-linux-x86_64.AppImage` | 12,933,624 | `0c48cc2303f53649c912be63027ee402db6737f63b0b4a3a00c8794355f9ab21` | 未签名 | Ubuntu 24.04 WSLg 使用 `--appimage-extract-and-run` 启动并持续运行 5 秒 |
| Linux x86_64 | `ubuntu-24.04` | `FreeRemoteAccess-0.1.0-linux-x86_64.deb` | 8,469,572 | `9416a26451f982470641b0f1d4448097369a679edd043c81d0277014cbd03657` | 未签名 | 原生打包成功；未执行安装写入 |
| Linux x86_64 | `ubuntu-24.04` | `FreeRemoteAccess-0.1.0-linux-x86_64.rpm` | 10,092,164 | `92ee5f774eae0c13e4c3481222bd6f1b351ce795dba70238ad984474a4ddc0f4` | 未签名 | 原生打包成功；未执行安装写入 |

Linux AppStream 元数据另经 `appstreamcli validate --no-net` 验证通过。Windows Authenticode 检查明确返回 `NotSigned`。当前构建没有配置 Apple Developer、Windows 代码签名或 Linux 仓库签名凭据，因此未将未签名产物描述为正式发行版。

## 下载工件

- [Windows x64](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/32764244192/artifacts/9534147188)
- [Mac OS universal](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/32764244192/artifacts/9534191868)
- [Linux x86_64](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/32764244192/artifacts/9534110926)

## 协议互操作门禁

本次结果只证明共享 Rust 客户端、测试和三平台安装包能够在对应 runner 上构建，并包含有限的本地启动证据。它不等同于真实服务端互操作成功。

| 目标服务端 | 协议路径 | 当前结论 | 尚需验证 |
| --- | --- | --- | --- |
| Mac OS 原生屏幕共享 | Apple ARD 用户名/密码；标准 VNC 仅回退 | 离线实现和回归测试通过，当前网络不可用 | 非黑首帧、键鼠、动态分辨率 generation、断开、Mac→客户端音频 |
| Windows 原生远程桌面 | RDP + NLA/CredSSP | IronRDP 适配层和测试通过 | 对授权 Windows 服务端登录、画面、输入、尺寸变化和断开 |
| Linux 原生 VNC | RFB 3.x | Raw/CopyRect、输入和剪贴板测试通过 | 对授权 stock VNC 服务端登录、画面、输入和断开 |

P5（客户端麦克风到 Mac）继续 fail-closed：用户名/密码模式尚无 Apple 原生接收路径证据。Android 和 HarmonyOS 保留为第二阶段，既不是这次桌面安装包门禁的一部分，也没有被宣称为已经完成。

## 完成度审计

- 已完成：Flutter/minifb 清理、分层会话合同、统一 winit/egui/wgpu GUI、RDP/ARD/RFB 适配、Windows/Mac OS/Linux 原生安装包、离线测试和工件验证。
- 未完成：三类真实 stock 服务端的现场互操作门禁；因此桌面第一阶段当前是“实现和打包完成，现场验收待网络及目标机恢复”，不是最终发布完成。
