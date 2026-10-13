# HarmonyOS 手机登录页布局修正

2026-10-13，用户反馈手机登录信息无法一屏完整显示，视觉层级松散。本改动仅修正 ArkUI 登录表单布局、最近连接入口和开发验证入口，复用既有原生组件、平台字体和明暗色令牌。

## 设计依据与目标

遵循官方 [Material 3 文本字段指南](https://m3.material.io/components/text-fields/guidelines)的持久标签与字段分组原则、[Material 3 自适应布局](https://m3.material.io/foundations/layout/canonical-examples/overview)的紧凑宽度适配，以及 [Apple HIG 布局](https://developer.apple.com/design/human-interface-guidelines/layout)和[文本字段](https://developer.apple.com/design/human-interface-guidelines/text-fields)的安全区、平台输入与标签关联原则。主代理在实施前完成相关官方指导审阅。

视觉方向为克制的原生工具表单：中文任务标题优先，产品名作为较小的次要标识；字段成组，主要“连接”动作清晰，开发工具属于独立且可关闭的暂态面板。无审美例外，也不引入新图标或字体。

正常手机竖屏、键盘收起、默认字体下，在已观察的 359vp 宽、722vp 高可用区域中显示目标系统、主机地址、端口、用户名、密码、安全保存选项和连接按钮。无最近记录的基础布局目标约 650vp；最近记录数目不增加页面高度。不能通过隐藏错误、缩小触摸区域、取消字段或取消滚动达成目标。

## 布局与交互

- 宽度小于 600vp 或高度小于 500vp 进入紧凑间距：外侧 16vp、上下 16vp；较宽桌面上下与外侧 24vp。表单继续水平居中，最大宽度 440vp，保留平台原生窗口控件与安全区。
- “连接远程桌面”紧凑布局 24fp、较宽布局 28fp；产品名 12fp。开发验证的 48vp 原生文字按钮放在标题旁，现有 `native.pocAvailable()` gate 控制是否出现。
- 字段标签 14fp，字段内标签与输入框间距 6vp，字段组间距 12vp。输入文字 16fp，目标系统与端口同排；目标系统分配剩余空间，端口宽 96vp。主机地址与两项凭据保持整行。
- 输入框、选择器和按钮最小高度均为 48vp；只约束最小高度，较大字体可使内容自然扩展。页面一直保留 Scroll，键盘仍使用 `KeyboardAvoidMode.RESIZE`。横屏、较小窗口、大字和更多错误信息允许安全滚动。
- 最近连接改为单个原生 Select，菜单列出地址、端口与用户名，菜单由原生组件限制高度并可滚动。选择继续沿既有 native profile action 和 snapshot 确认路径恢复字段，并以 `passwordEpoch` 重建密码输入。
- 安全保存复选框视觉为 24vp，文字和视觉状态位于一个最小 48vp 高的原生按钮内。整行只有一个操作回调；视觉复选框不独立接收命中或键盘焦点。整行提供 checkbox 可访问角色、名称及 checked 状态，原生按钮承担指针和键盘激活。
- 开发验证面板包含既有离线图形与 HUKS 操作及实时验证状态。使用 ArkUI `bindSheet`，有原生关闭、文字关闭和返回关闭，内部 Scroll 最大高度为可用表单高度的 70%，避免横屏和大字导致操作不可达。
- loading、安全存储载入提示、协议不可用说明、表单错误、操作 notice 与 bridge notice 仍在表单中持续显示；字段错误使用现有 showError 路径。新增显示既有 target/protocol/profile 错误字段。

## 语义边界

保持目标系统更换时默认端口、FormEdit、appAction 编号、profile 选择确认、passwordEpoch、安全存储读写与授权语义。密码只由现有 native.editPassword 收到，未增加 ArkUI 密码状态、日志、快照或普通文件存储。密码 onChange、输入法合成批次、Enter 按键去重和 onSubmit 的回调正文保持原样；连接按钮继续调用相同 submit 路径。

远程会话 chrome、渲染、输入、协议、FFI 与 native 安全存储实现不属于本改动。新增返回处理仅优先关闭开发验证面板，然后执行既有页面返回逻辑。

## 验证与验收

源审查核对当前 DevEco SDK 的 `SelectOption`、Select 字体/菜单宽度、`bindSheet`/`SheetSize.FIT_CONTENT`、ButtonStyleMode、HitTestMode、AccessibilityRoleType.CHECKBOX 和 checked 属性，避免猜测组件 API；`git diff --check` 只证明文本差异无空白问题。

主代理负责隔离 Hvigor 编译、签名、安装和实际手机/PC 截图验证。要求分别确认普通竖屏全部登录信息可见、键盘出现后字段可达、横屏/大字可滚动、明暗外观、原生焦点/键盘按钮激活、标签/状态可访问、最近连接恢复、开发面板关闭与实时状态。编译、截图、生命周期/交互和真实远程互通是不同证据层，不得互相替代。

实施阶段不伪造镜像布局的单元测试，也不从估算高度声称设备验收已经完成。最终实测与剩余限制由主代理在该变化的 README 矩阵和验证证据中记录。
