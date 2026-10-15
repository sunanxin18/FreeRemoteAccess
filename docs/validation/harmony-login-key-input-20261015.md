# 鸿蒙登录按键分发回归（2026-10-15）

状态：**受限验证**。当前问题是登录输入被应用远程按键拦截器截断，并非地址字段过滤数字。

## 证据与修复

旧包在PC API26 ARM64模拟器，使用官方Emulator同一公开IP fill操作，15:41:28截图只留下三个中文句号，数字全部消失。Index地址TextInput没有inputFilter，FormEdit与Rust draft没有删除数字逻辑。

本机官方SDK `ets/component/common.d.ts:18689–18704` 规定，注册 `onKeyEventDispatch` 后事件不会分发给子组件，返回true才额外阻止向父冒泡。旧根Stack注册回调，page0返回false仍无法让TextInput获得数字等原生按键。输入法可独立提交字母，产生“字母可以、数字不行”的现象；本轮不据此推断所有输入法内部实现。

将唯一dispatch绑定到远程XComponent，根Stack改普通 `onKeyEvent` 冒泡观察：本地TextInput/按钮先处理，消费的远程按键不重复冒泡发送，原会话尾Up清理仍保留。密码框的onSubmit、IME判断与按住周期去重路径保持。

## 分层验证

SDK真实ArkTS AST检查绑定范围，并执行从实际Index抽取的回调：数字/字母/标点/Meta/Tab本地事件、旧来源尾Up、密码Enter单次提交、浮动岛Escape/CtrlAltHome，初始5项通过，随后追加地址输入类型回归共6项通过；旧源绑定位置先RED。独立审查PASS；既有23浮动岛、11方向和10全屏测试通过。

首个修复包真实Release构建、签名/验签/checker、原ZIP条目签后逐字核对成功；PC覆盖安装。15:48:55同一IP操作所有数字恢复；15:49:10混合输入完整保留所有0–9与字母。没有输入密码、保存测试记录或提交测试连接。

同时发现中文输入法把ASCII句点转换为U+3002。首修复包只是修复数字分发，不能把这些截图视作精确ASCII IP通过；系统菜单选英文后该标点行为仍存在，失败断言原样保留。地址输入语义的后续验证见下方结果，不通过静默替换密码或用户名解决。

初始数字修复HAP：unsigned `56efb0c45e9fc9a424adf9d3ea1091a3242b0b46bc6cadea1de02c5356bef908`，signed `7f73df243df5e054ad1d2824b174e11506a613c643b3a3a465301942fadefb50`。
本地证据：`target/harmony-login-key-input-20261015/`；旧包反例在 `target/harmony-partial-refresh-20261015/pc/numeric-before-result/`。

本轮不声称PC真实Windows认证、远端物理键盘/IME、所有输入法或真机已通过。修改仅在OHOS ArkUI适配层，Windows/macOS shell没有改变。

## 地址输入语义

地址TextInput追加 `InputType.URL`，向系统提供地址输入模式。官方SDK `text_input.d.ts:152–160` 明确该模式无特殊字符限制、API12起支持；保留主机名/IPv4/IPv6原有语义，不把地址改成Number，不替换用户字符串。新增AST检查确认端口Number、密码Password及普通用户名输入类型不受影响。最终两文件冻结为 `72a9b2e3` / `420b0a12`。

最终Release HAP构建、签名/验签/checker成功，原ZIP条目签后逐字保持，手机/PC覆盖安装并保留应用数据。unsigned `0b44186a202f2bbedc4ffbe26ae22d59eac7f05f475cf619dec21e1ac07e78a0`，signed `b61ed8b2abd270fc9ce45dc13e8ef2f9c0a63f9020927f4d9480bb2611c9c83a`。证据在 `target/harmony-login-key-input-20261015/url-input/`。

URL模式本身未消除当前模拟器的中文标点：15:54:36仍产生U+3002，原失败断言保留。15:56:18系统设置自带键盘测试框输入 `1.2` 也变为 `1。2`，排除FreeRemoteDesk字符串改写。最终通过可见小艺状态栏的标点控制切换为英文标点（语言选择与标点独立），新输入产生ASCII点。官方fill会在末尾追加、不会替换选区，因此重启应用清空未保存测试草稿后再做同一IP输入；15:59:23完整截图及内存精确字符串比较确认9个数字、3个ASCII点、与预期地址完全一致。回执只保存布尔/计数，没有密码内容。PC留在填写有效地址的登录页，未填写密码、保存测试草稿或尝试认证。

手机最终包仅安装/启动；本次远程RFX画面观察对应之前单独RFX包，不能冒充最终键盘包已做手机完整真实键盘验收。实体键盘、更多IME、深色/大字和真机不由上述CLI输入/源码测试自动覆盖。

## 用户复核

2026-10-15用户在后续询问中明确回复“现在已经能输入数字”。这是当前PC登录字段的人工反馈，补充此前官方CLI的0–9/ASCII IP证据；并未提供主键区/小键盘分别验证，不能据此宣称远程实体键或全部IME已验收。用户同时回复仍在填写表单并要求暂不操作PC，因此不重装、点击或读取其当前表单。
