# HarmonyOS IME 来源与 Owner 桥（2026-10-15）

状态：开发中。原生来源/操作桥的源码子步骤已通过独立复审、主机测试与实际HAP构建；本记录只覆盖这一步，实际 ArkUI 预编辑、系统输入法附着、浮动岛键盘动作和手机/PC真实输入仍为后续步骤。真实组合根继续使用 IMEUnavailable，未因这段源码存在而开启提交文本。Windows/macOS/Linux 的既有输入路径不变。

## 行为与边界

新增增量 C ABI `frd_ohos_app_ime_source` / `frd_ohos_app_ime_operation`，NAPI 对应 `appImeSource` / `appImeOperation`。旧 POD 和入口签名保持。来源 ticket 固定实际 windowStage/windowId、单调 editorSerial 和远端 session/generation；Owner 发布当前真实窗口及可附着状态，生产者不能自行创建窗口身份。当前窗口只保留一个高水位槽，后台、撤销、队列失败不清掉旧来源的 tombstone；实际新窗口确立后才允许换槽。

JSON v1严格校验版本、已知字段、字段类型、安全整数、规范十进制远端身份和UTF-8长度。已确认文字与十二类平衡控制键是两种互斥操作；不接收预编辑、能力位、坐标或任意HID/修饰键。相同文字的两个不同serial分别保留，重复或跳号使来源失效。普通未绑定ticket的文字入口在显式安装IME的路径中被拒绝，防止第二条提交来源。

同一有效来源在短暂几何重建期间最多等待32个操作、合计16384个UTF-8文字字节；完整画面实际呈现后，经当前InputAdapter/revision顺序发送。实际失焦、后台、撤销、窗口/会话/代际失效、协议文字能力丢失会清空等待。快照只有固定状态、来源和serial；文字及其长度不进入快照/日志。accepted表示Owner接纳，dispatched只表示SessionHost command channel接受，二者均不是远端ACK；没有自动正文重发。

物理键和IME安装资格独立，最终仍经过协议/平台/产品交集以及真实焦点和完整呈现门。共享controller只新增返回已有协商状态的只读getter，不新增影子协议状态，不改变Apple的text=false。实际OHOS组合根和默认构造器仍未安装系统IME；这一步不能证明键盘已弹出、文字到达远端或系统输入法已经可用。

## 已执行验证

首轮实现的主机结果：绝对Rust1.96工具链，frd-app86通过；OHOS FFI85通过、1项既有ignored，其中16项新增IME用例。双协议feature check和官方SDK C++严格syntax检查通过。全部初始RED、编译/夹具错误和平台进程固定存储根竞争记录保留在 `target/harmony-ime-owner-20261015/`；原有实体键consumer仅隔离测试存储，ABI、队列、Owner、SessionHost与实际ProtocolSession consumer断言保留，产品安全存储不变。

独立首轮审查发现三项必须修复：显示失效发送屏障、重复/陈旧generation事件及枚举字段类型。每项先有确定性反例：实际满64槽LocalDisplay producer在admission前发布DISPLAY_INVALID、三类被拒绝generation事件，以及真实C ABI对象枚举类型。五项回归先0通过/5失败，再5通过；实现分别纳入显示屏障、复用原有accepted-generation分支、要求字符串后精确枚举匹配。同一独立reviewer复审为Spec PASS / Quality APPROVE，三项全部关闭，初审和失败历史保留。

最终绝对Rust1.96完整OHOS FFI **90通过、0失败、1项既有ignored**，含21项IME测试；frd-app同字节源码的86项通过。双协议feature check、限定rustfmt、diff检查及官方SDK C++严格syntax通过；feature check仍有四条未改RDP scalar/oracle helper的dead_code warning，没有新增IME warning。Down成功但Up前通道恰好关闭的窄竞态未做确定性注入；实现对此尝试原有release cleanup，不把该代码检查称作远端一定收到Up。

最终13个源码固定在 `target/harmony-ime-owner-20261015/source-manifest-fix1.json`，SHA-256 `9ea7f1c290f5a2c04759124730747aa4e584e78a1d2473bfba875af30063bf1d`。root核对源码/证据hash、完整生产差异和修复差异，构建后13源码保持不变。原始与修正的报告、review、RED/GREEN及完整日志保留；复审仅覆盖源码桥，下面包验证由root独立执行。

实际官方OHOS ARM64 Release双协议HAP构建exit0，沿用固定FFmpeg bundle及manifest；checker验证现有原生依赖、许可/来源、INTERNET唯一权限和phone/2in1模块。unsigned HAP SHA-256为 `52ff9931fb4f7120f43b296ea2702ed3c2f264e913d1af6ccd11a75cb165f6a7`，22662895字节；包内FFI SHA-256为 `0708799dc6e05ce925055ed3efdf8cddff1cbe727e524885cfe3a7f91cee1ca9`。实际ELF确认两个新增C ABI均为已定义GLOBAL FUNC。31个ZIP条目与先前有限实体键包集合相同，只有FFI和libentry两个原生库字节变化，ArkTS字节码与其它条目保持。证据在该目录的 `root-build/build-result.json`、`build.log`、`post-build-verification.json`。没有签名、安装或设备操作。

这些是源码、主机consumer和目标构建/包身份结果，不是系统IME附着、wire/远端ACK、手机/PC运行或生产启用资格。

## 后续接线与运行门

正常客户端将使用本地有界UTF-16预编辑与系统InputMethodController实时提交。有效finish保留最终预编辑；`P→I→F`与`P→F→I`按[官方输入法合同](https://github.com/openharmony/docs/blob/f41b9345badd47c7ab0c263344cd7f4b5a549afb/zh-cn/application-dev/reference/apis-ime-kit/js-apis-inputmethodengine.md#finishtextpreview12)分别处理，不使用内容/定时器去重。局部字素删除/移动优先复用[公开NDK ICU4C](https://github.com/openharmony/docs/blob/f41b9345badd47c7ab0c263344cd7f4b5a549afb/zh-cn/application-dev/reference/native-lib/icu4c.md)，不假造远端全文。

现有Index的尺寸变化回调会调用focus(false)，必须在系统键盘接线时与真正blur分开；原生pending测试不能代替这项前端修复。手机与PC需分别验证真实focus/attach、候选/提交、退格/Enter/emoji、物理键单一来源、键盘resize、后台、断连和重连。PC当前由用户填写表单，本步骤未操作其界面、安装包或连接状态；也没有手机/PC输入法运行结论。
