# 隔离 X11 输入及数字报告门禁

`run-x11-input.sh <probe绝对路径> <新报告路径> <scale>` 为每次运行新建 Xvfb、D-Bus
session 与私有 XDG_RUNTIME_DIR。调用者的 DISPLAY/WAYLAND_DISPLAY 被清除；所有
xdotool 命令仅在新 X server 中执行。脚本不接受复用已有 display 的选项，不操作
开发机当前 GUI。scale 仅接受 1、2；报告路径已存在时拒绝覆盖。

```bash
tools/frd-linux-native-shell-probe/run-x11-input.sh \
  "$PWD/target/linux-native-shell-probe/frd-linux-native-shell-probe" \
  target/linux-native-shell-probe/logs/x11-input-1.jsonl 1
```

除 probe 构建依赖外，运行需要 `xvfb xauth xdotool dbus-x11 python3 coreutils`。
外层 30 秒 timeout 覆盖整个隔离 session，探针自身 10 秒退出。脚本只终止本次创建
的 probe；Xvfb 生命周期和授权文件由 xvfb-run 管理，私有 runtime 目录退出时清理。

脚本用实际进程 PID、完整窗口标题与可见状态同时定位唯一窗口；使用没有 WM 也能
工作的 `windowfocus`，并读取实际 X11 input focus 核对。按实际窗口像素宽高选择
下半部内容区域，XTEST 移动、点击，然后发送固定 F8 按下/释放。不给键盘命令
指定 `--window`，避免用 SendEvent 绕过真正输入焦点。没有自由文本输入或记录。

`verify_report.py` 可以独立验证任一已有报告：

```bash
python3 tools/frd-linux-native-shell-probe/verify_report.py report.jsonl \
  --expect-backend x11 --expect-scale 1 --require-input
python3 tools/frd-linux-native-shell-probe/verify_report.py report.jsonl \
  --expect-backend wayland --expect-scale 2
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s tools/frd-linux-native-shell-probe -p 'test_*.py'
```

门禁要求精确一条 summary、精确一条 geometry_detail、三条有界驱动信息，拒绝重复
JSON key、未知/错误记录、非有限数字、错误字段类型、过大报告。它独立复核 backend、
scale、frame/error、mapped、标题栏与内容邻接、中心偏差、viewport/DPI、指针像素
比例，而非只读取 `passed: 1`。

`--require-input` 额外要求 motion、完整鼠标和键盘按下/释放、内容焦点和窗口焦点、
指针处于真实内容区，并确认标题栏测试按钮未被误击。它只允许 X11：Wayland 尚无
native injection，显式请求 Wayland 输入证明会失败。普通 Wayland 几何/GL 通过不
代表输入通过。所有结果也不代表 RDP、真实远程控制或硬件 GPU 通过。

16 项当前单元测试使用合成 JSON 证明拒绝策略，不运行窗口。实际 XTEST 与 GTK
controller 交付已有 a60 三架构 X11 1× 原始报告，修正规则后离线复验通过；
2× 与 Wayland 后续步骤未完成。本机 macOS 的合成报告与 shell 检查不增加原生证明。官方参考为随 xdotool 和 xvfb 发布的
[xdotool 手册](https://manpages.debian.org/bookworm/xdotool/xdotool.1.en.html)及
[xvfb-run 手册](https://manpages.debian.org/bookworm/xvfb/xvfb-run.1.en.html)。

固定 GTK CI 主题的边界验收允许标题栏向左、右、上最多外扩 1 个逻辑点。
内容的 x 原点必须为 0、y 必须非负，标题栏必须左右对称覆盖内容（报告保留两位
小数，对称差容差为 0.02），顶部坐标限于 [-1, 0]。仍保留标题栏与内容接缝、
中心偏差、viewport/DPI、焦点和输入配对检查。这是固定探针几何边界，不能作为
任意 GTK 主题或产品布局的保证；其他主题超出边界时需重新测量并审阅。

a60 x64 的 `x11-1.jsonl` 实测标题栏为 `(-1,-1,952,47)`，内容为
`(0,46,950,584)`，标题栏底边与内容顶边恰好相接。回归 fixture 保留该报告的
几何与输入计数；scale2 变体仅为合成比例验证，不新增 native scale2 证据。
新增 fixture 先在旧校验器因负坐标失败，再在有界规则下通过；过量负坐标、
不对称外扩、内容负坐标或偏移仍必须拒绝。
