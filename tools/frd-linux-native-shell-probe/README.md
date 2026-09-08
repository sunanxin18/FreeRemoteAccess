# Linux 原生窗口与 GL 内容区域技术验证

这是独立技术验证程序，**不是 FreeRemoteDesk 产品客户端**，没有网络、远程登录、
凭据、RDP、codec 或音频路径。它验证 GTK4 原生 HeaderBar 与 GtkGLArea 在同一窗口
中的可行接线，不修改 Windows/macOS，不参与 Rust workspace。

GTK 原生窗口管理标题栏和按钮，不覆盖 `gtk-decoration-layout`。中心产品控件只是
“本地测试画面”与“测试操作”fixture；按钮只有计数效果，文字标签自带可访问名称与
tooltip，目标至少 44 逻辑点。标题栏和 GL 内容区是分别分配的原生 widget；不会在
GL 内容上叠加或额外绘制第二条工具栏。GTK 保留标题栏空白拖动及宿主按钮行为。

GLArea 创建自己的 framebuffer。GL shader 直接生成棋盘格与移动竖线，然后由 GTK
作为纹理合成，不调用 `glReadPixels`，没有 CPU framebuffer readback/upload。
该探针采用原生 OpenGL 3.3 或显式 GLES 3.0，尚未证明与产品 wgpu renderer 的连接。
**Mesa llvmpipe/swrast 通过只代表原生 GL 调用与软件驱动路径；不是硬件 GPU 证明。**
输出始终保留 `hardware_verified: 0`，不根据驱动名字猜测硬件已验证。

## 构建与有界运行

仅 Linux 构建，要求 C11 编译器、pkg-config、GTK4 >= 4.6、libepoxy 开发包。
Ubuntu/Debian 对应 `build-essential pkg-config libgtk-4-dev libepoxy-dev`。

```bash
tools/frd-linux-native-shell-probe/build.sh
GDK_BACKEND=x11 target/linux-native-shell-probe/frd-linux-native-shell-probe \
  --expect-backend x11 --seconds 3
GDK_BACKEND=wayland target/linux-native-shell-probe/frd-linux-native-shell-probe \
  --expect-backend wayland --seconds 3
```

`--gles` 请求并核验实际 GLES 上下文。`--seconds` 只接受 1..300。
必须显式提供 `--expect-backend`；程序核验实际 `GdkDisplay` 类型，不以环境变量本身
充当 X11/Wayland 运行证据。`CC` 指定单个编译器可执行路径，`PKG_CONFIG` 指定工具；
`FRD_LINUX_NATIVE_PROBE_OUTPUT_DIR` 可改变输出目录，默认 `target/linux-native-shell-probe`。
`CC` 不解释 shell 命令字符串，交叉链接器选择由 CI 单独配置。

X11 CI 可用 Xvfb；Wayland CI 需要独立运行的 Weston 等 compositor，保留各自日志。
探针不自行启动 compositor，不安装或修改桌面设置，不连接服务器。外层 CI 应再设置
进程超时，防止图形驱动/事件循环异常阻塞定时退出。

CI 的 Wayland 进程另受15秒外层超时约束，5秒后强制结束；stderr逐个scale保存，
Weston日志在回收私有runtime目录前复制到artifact目录。X11继续使用独立驱动的
30秒外层限制。内部定时退出不替代外部进程超时。

## 输出与成功边界

stdout 为 JSON 行：有界、ASCII 清理后的 `GL_VENDOR` / `GL_RENDERER` / GL version；
其余为数字。不会输出按键值、按键名称、输入文本、剪贴板、用户名或密码。
`backend` 为 1(X11)、2(Wayland)、0(未知)。

定时结束只有同时满足以下条件才 exit 0：实际后端匹配、窗口 mapped、GL 成功绘制
至少一帧、无记录的 GL 初始化/绘制错误、真实标题栏和内容区无重叠、中心控件相对
标题栏中心偏差不超过 1 逻辑点、GL viewport 非零。提前手动关闭不计通过，exit 1。
参数错误和非 Linux 构建 exit 2。驱动错误只输出固定数字错误码：

- 1：GLArea 上下文不可用；2：shader 编译失败；3：program 链接失败。
- 4：framebuffer 不完整；5：绘制 GL 错误；6：实际显示后端不匹配。
- 7：实际 GL/GLES API 不匹配；8：render入口目标查询或实际current校验失败。

`summary` 输出 frame/error、窗口映射、标题栏/内容 allocation、中心偏差和 viewport。
`geometry_detail` 输出 resize、focus、按当前内容 allocation 与 GL viewport 比例计算的
指针像素坐标。指针、按键、焦点和标题栏动作各自计数；默认无交互 CI 中计数可以为零，
**因此默认成功不证明鼠标、键盘或真实远程输入通过**。

## 后续原生 GUI 验收

人工或授权本机 UI 工具分别验证 X11/Wayland：浅深色、100/150/200% 缩放、改变按钮
左右布局、窄窗口、拖动、最大化/恢复、全屏、右击/双击标题栏、Tab/Space焦点、内容区
点击/按键以及标题栏操作不增加内容区输入计数。检查真实截图没有第二标题栏、中心
没有偏移，checkerboard 与指针像素映射一致。该阶段未执行时不得称 GUI 已验收。

## 官方依据与未完成的视觉审阅

已通过官方文档正文核对：

- [GtkGLArea](https://docs.gtk.org/gtk4/class.GLArea.html)：独立 GL framebuffer、render
  callback 内既有 viewport、texture 合成、realize/unrealize生命周期。
- [GtkWindow.set_titlebar](https://docs.gtk.org/gtk4/method.Window.set_titlebar.html)：
  显示前设置自定义原生标题栏，避免额外 WM 标题栏。
- [GtkHeaderBar.set_title_widget](https://docs.gtk.org/gtk4/method.HeaderBar.set_title_widget.html)。
- [GtkSettings.gtk-decoration-layout](https://docs.gtk.org/gtk4/property.Settings.gtk-decoration-layout.html)：
  由宿主决定原生按钮及左右位置。
- [GNOME Header Bars](https://developer.gnome.org/hig/patterns/containers/header-bars.html)：
  少量控件、中心对齐、tooltip、保留拖动空白。
- [Apple Toolbars](https://developer.apple.com/design/human-interface-guidelines/toolbars)：
  本项目选择标题栏整合形式，不新增内容区工具条。

[M3 App Bars](https://m3.material.io/components/app-bars/overview) 与
[Apple Windows](https://developer.apple.com/design/human-interface-guidelines/windows) 的正文抓取
只返回 JavaScript required。本轮尝试 CUA 浏览器渲染，iab 不可用且浏览器清单为空。
**完整官方页面视觉审阅仍未完成，这份探针不是 GUI 设计批准或产品迁移验收。**

## 实际 GTK 目标数字诊断

每次 render 入口（GTK已经绑定FBO、尚未改动GL状态）采集目标，并在结束时输出
精确一条 `target_observation`。字段只包括实际current匹配、GL/ES版本/core、FBO是否
非默认、完整性、draw-buffer0及额外draw-buffer计数、附件类型/颜色编码、尺寸是否
已知、实际texture target、尺寸/内部格式/samples、viewport、查询错误、兼容判定，
以及观察次数和变化次数。FBO/附件名称仅供进程内检测变化，不输出名称或指针。

renderbuffer用正确类型查询并恢复绑定。纹理仅在desktop GL4.5+通过DSA读取真实
`GL_TEXTURE_TARGET`，确认2D才查询对应level尺寸/格式；其他target或缺少DSA时用
零尺寸哨兵并标记不兼容，不试探绑定未知纹理。此保守探针不会把GL3.3可工作直接
等同于已测得GTK目标兼容。查询依据为Khronos官方
[glGetTexParameter参考源](https://github.com/KhronosGroup/OpenGL-Refpages/blob/main/gl4/glGetTexParameter.xml)。

strict verifier要求新记录唯一、字段精确、观察次数匹配frame，并独立重算兼容：
Linux后端所需desktop core、sRGB、实际2D纹理、单一draw-buffer0、无MSAA、附件像素
尺寸和viewport匹配才可为1。LINEAR或renderbuffer测量可通过数字报告验收但必须
compatible=0；不代表查询失败。未知枚举/格式、矛盾哨兵、查询错误或伪造compatible
均拒绝。旧报告缺少目标记录，不能作为新目标门禁的通过证据。095/a60等历史报告须使用
对应revision的verifier复验，只能保持当时的GL/几何/输入范围；不得用新verifier
宣称这些旧报告已经通过目标查询门禁。

20项Python合成测试包含新记录完整性、合法不兼容目标、旧GL无DSA哨兵及伪造拒绝。
本机macOS不运行原生C探针；新增GL查询仍待Linux CI运行。此次仅增加诊断，没有
GTK产品接线、真实呈现ACK或硬件GPU证明。
