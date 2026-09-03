# Windows 浮动控制岛验收记录

日期：2026-09-03
分支：`codex/mac-baseline-rdp-integration`
实现提交：`eb1dd10fb92d4cc8382220618829a5399a25c10a`
Release SHA-256：`10A9923A564E71B92E212B6AA960766C31C07A138DDEC8B1B3381EF15138B73D`

## 验收范围

本记录只验证 Windows 客户端 shell 的浮动控制岛、原生窗口动作和输入隔离，
不证明 Apple HP、Apple Standard 或 RDP 的网络互操作。

## 自动化门禁

- `cargo fmt --all -- --check`：通过。
- `cargo test -p frd-ui-model -p frd-app -p frd-ui-egui -p frd-shell-desktop -p freeremotedesk-windows`：通过；分别为 12、75、27、164、12、2、2 项测试，零失败。
- `cargo check -p freeremotedesk-windows`：通过。
- `cargo build --release -p freeremotedesk-windows`：在 detached `eb1dd10` 干净工作树中通过；构建源树无未提交改动。
- 100%、150%、200% DPI 几何、窄窗口夹取、控制命中优先级、远程内容矩形不变、
  150 ms 显示和 700 ms 隐藏、减少动态效果/高对比度、拖动位置夹取、
  `Ctrl+Alt+Home` 输入释放、AccessKit 动作与合成事件拒绝均有确定性单元覆盖。

## Windows Release 可视验收

以唯一一个 `freeremotedesk-windows.exe --test-texture` 实例执行，系统缩放为 100%。

| 项目 | 结果 | 证据边界 |
|---|---|---|
| 默认隐藏 | 通过 | 启动后远程测试纹理占满客户区，只保留顶边半透明绿色提示线 |
| 顶边唤出 | 通过 | 指针进入顶边后约 250 ms 复核，圆角透明控制岛可见；实现门限为 150 ms |
| 自动隐藏 | 通过 | 指针离开后等待 900 ms，控制岛消失；实现门限为 700 ms |
| 远程内容空间 | 通过 | 显示/隐藏控制岛时测试纹理尺寸和纵向起点不变，没有常驻工具栏 |
| 最大化/还原 | 通过 | 最终干净 Release 的控制岛按钮完成 1102×704 与 1440×852 间往返；保留 `HTMAXBUTTON` Snap 命中 |
| 最小化/恢复 | 受限通过 | 前一候选 Release 由控制岛按钮最小化并恢复为同一实例；最终提交的相同命令路径由自动化测试覆盖 |
| 关闭 | 通过 | 控制岛关闭按钮结束唯一实例，进程窗口列表为空 |
| 控制岛拖动 | 自动化未下结论 | 坐标注入未形成可重复的可视位置变化；状态、夹取及命中隔离由单元测试覆盖 |
| `Ctrl+Alt+Home` | 自动化未下结论 | UI 自动化键盘事件会被产品按合成事件拒绝；真实事件路径及拒绝合成事件均由单元测试覆盖 |
| AccessKit 实时树 | 自动化未下结论 | 当前 Windows 自动化树只枚举原生标题栏；自定义根动作递归可达性和中文标签由单元测试覆盖 |

## 未验证边界

- 未修改 Windows 系统设置，因此 150%/200% DPI、高对比度和减少动态效果仅为自动化门禁，
  不是本轮人工系统级验证。
- Windows 触摸屏顶边唤出能力保持禁用，尚未做触控设备验证；当前只有鼠标顶边唤出。
- 未在浮动控制岛集成后执行同条件端到端远程会话延迟对比，因此尚不能证明“无可测量延迟回归”。
- macOS、Linux、Android 和 HarmonyOS NEXT 平台 shell 尚未实现，不能继承本记录。
- 测试纹理不包含真实网络、解码、音频、剪贴板或远端输入互操作结论。
