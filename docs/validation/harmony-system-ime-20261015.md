# HarmonyOS 正常会话系统输入法（2026-10-15）

状态：开发中。正式会话系统输入法候选已完成主机测试与目标HAP构建，独立审查与目标执行发现尚待关闭的问题，未签名或安装新包。构建成功不代表系统输入法、设备界面或远端文字输入可用。原生来源桥的既有结果见[来源桥验证](harmony-ime-owner-20261015.md)。

## 本次实现范围

沿用当前右下角浮动岛，增加正常“键盘”操作，使用系统 InputMethodController 和真实 XComponent 焦点。手机请求系统键盘；PC 可附着输入法而不强制显示软键盘。表单继续使用原生安全密码框，远程会话没有另开编辑器或手动发送步骤。

本地只保留有界的当前预编辑上下文；候选不发送到远端，确认文本及有限控制动作通过既有 ticket/serial 桥发送。两个相同文本的合法提交分别保留，不按字符串或计时器去重。局部编辑按 UTF-16 坐标校验，字素边界使用公开 NDK ICU4C；不假设客户端掌握远端全文或真实远端光标位置。

来源必须绑定实际窗口、编辑器和远端会话代际；SDK 附着成功后仍需观察 Owner 的来源确认才能提交等待中的动作。真正失焦、后台、断开和代际变化先撤销本地来源，再清理 SDK。单纯键盘避让或尺寸变化保持同一有效来源，呈现和输入继续共用同一内容矩形。实体键模式与系统输入法模式使用明确的单一生产者选择。

## 界面依据

本轮实施前检查了 [Material 3 按钮指南](https://m3.material.io/components/buttons/guidelines)、Google 官方 [Material Web 按钮文档](https://github.com/material-components/material-web/blob/main/docs/components/icon-button.md)、Apple HIG 的[键盘](https://developer.apple.com/design/human-interface-guidelines/keyboards/)与[虚拟键盘](https://developer.apple.com/design/human-interface-guidelines/virtual-keyboards/)指引。M3 正文要求 JavaScript，HIG 正文提取不完整，补充使用官方文档与官方检索摘要；不把检索摘要当作目标系统运行证据。

鸿蒙继续使用原生 ArkUI 控件和既有会话层级。“键盘”使用中文文字按钮，目标至少48vp，具备可访问名称、焦点与状态区别；不新增图标字体、不改变原生窗口控件、不保留额外工具栏。深浅色、字号、横竖屏和键盘避让的实际界面检查仍需分别完成。

## 待完成验收

- 源码与主机 consumer：预编辑/确认顺序、UTF-16 范围与字素编辑、相同文字两次提交、附着过程即时回调、Owner 来源确认、旧异步回调、队列与失败处理、真实焦点和尺寸变化、实体键与输入法单一来源，以及既有表单回归。
- 目标构建与包：官方 API26 ArkTS 类型检查、实际 OHOS ARM64 Release 双协议 HAP、公开 `libicu.so` 仅由 `libentry.so` 引用、固定 FFmpeg 材料和唯一 INTERNET 权限保持；独立复审与产物身份核对。
- 系统库执行：与正式入口共用纯原生字素 helper 的固定测试程序，分别记录目标、SDK、产物哈希、完整终态和预期边界；不能由头文件或交叉链接推定运行成功。
- 手机真实会话：系统键盘、中文候选/确认、英文、emoji、退格/Enter、重复提交、键盘避让、失焦、前后台与断开重连；同时检查浮动岛和全屏内容区域。
- PC 真实会话：待用户完成表单并交还实例后，独立验证系统输入法、实体键模式切换、窗口尺寸变化和会话控制；手机结果不替代PC结果。

用户已确认 PC 数字输入恢复，同时明确“还在填写，暂不操作 PC”。因此本轮开发期间不读取或操作 PC 界面，不安装、重启或连接该实例。当前没有新的手机/PC系统输入法运行结论，也没有真机验收结论。

## 首个最终候选与实际反例

2026-10-15候选的18个源/测试路径固定于 `target/harmony-system-ime-20261015/source-freeze.json`，清单SHA256为 `c3b02e7b71a013095d59768a14f93efa0323871384d935162537d4925103163f`，root逐项核对源码、作者报告和三个产物身份。实际Release双协议unsigned HAP为 `29a8d3bc34130eba909eedc063bcdbda643120fdfbc71422792a280518b7cc7c`。100项ETS consumer通过（原67项保留），Rust91项通过/1项既有ignored，Python49项中47项通过/2项既有双recipe跳过。官方SDK构建和精确依赖检查通过，ICU只由libentry引用，FFmpeg材料与INTERNET唯一权限保持。保留四项既有RDP helper dead-code与Hvigor未配置签名提示。以上是候选源码/主机/包结果，独立审查尚未批准。

首次候选前，root源码阅读已发现并要求修复“取消尚未实际attach的来源仍detach”和“detach永不完成使后续来源无限普通重试”两项问题；各自确定性RED保留，修正后重跑最终构建与上述测试。原临时报告、清单、日志和HAP保存在 `prefreeze-base/`，不能用其身份代替最终候选。

独立审查随后确认另一个焦点反例：同一存活XComponent在连接中已经获得真实焦点，page2→page3时会话岛epoch变化却清除这份焦点记录。触摸“键盘”请求同一个已聚焦控件时，不一定再有onFocus，实际consumer到2秒仍零attach。精确反例与新page3真实onFocus的成功对照在 `target/harmony-system-ime-20261015/review-focus/`；仍需修复和复审。

root还在手机模拟器后台执行了固定原生字素程序 `b4d711e132e8d499b3c9f78443468fcc5bc9afaae3ecc7b1e9d5c0b1d5061a4a`：host返回0，独有guest退出码为2，属于失败。上传ELF与所选SDK C++运行库哈希精确，API26/ARM64身份已核对。首次主机夹具对API版本末尾空白的断言过严，保留原始成功观察，修正解析后继续，未因此重发身份命令或改设备配置。数值诊断定位到第一个非空fixture的cursor0，helper返回invalid。

进一步仅调用公开ICU接口的独立程序确认系统版本74.2.0.0，root、空locale和en_US的`ubrk_open(UBRK_CHARACTER)`均返回error2且iterator为空。该probe的guest0只表示成功打印诊断，不能算字素创建成功。证据在 `target/harmony-system-ime-20261015/root-phone-native/`；尚待区分独立CLI进程与普通应用上下文的数据初始化要求，不预判为emoji规则或产品接口不受支持。没有读取/修改系统数据目录、调用私有初始化、安装辅助服务或操作应用界面/PC。


## 焦点修正后的源码/HAP复审

Fix1只修改Index与实际surface consumer，其余16项冻结源保持。原18源、报告、manifest、diff、HAP和审查结果精确归档在 `review-fix1-base/`。根因修复保留仍未blur的ArkUI节点记录，同时在窗口/会话边界关闭来源并撤销native输入授权；回前台不自动attach。原page2→3反例从零attach转为一次attach，不需要第二次onFocus；后台有/无实际node blur均有独立对照，真实blur继续等待真实新焦点。

作者先保留两个真实RED再转GREEN，最终受影响七套80项ETS通过（surface13、原mapping/login/island/fullscreen/orientation/display保留）；未重复无变化Rust/ICU/Python套件，先前91项Rust及49项Python结果仍只按其同字节源码/原日志记录。实际官方Release双协议HAP与精确checker再次exit0，已有warning仍保留。同一独立reviewer scoped复审为Spec PASS / Quality APPROVE，I1关闭，没有新增必须修复问题；旧fixture压缩排版及既有warning记录为deferred。

最终18源清单 `fix1/source-freeze.json` SHA256为 `8bc6ba72e55c473174d9152871e6c1e4674125fde175f0438d5c0dd2a13169f7`；unsigned HAP为 `a92d5fc302c8229dd5ea18fae9802faf92b777092c1f9ba7d3ced2a0797d0bbb`。root已逐项复核18源、完整修正差异、报告原prefix保持及三个artifact身份。当前没有签名、安装或正式应用系统IME运行结论。

公开ICU运行时审计已进一步核对官方固定源码：Ark首VM创建有独立的数据初始化，而公开ubrk wrapper只转发。该机制支持“普通应用与纯CLI上下文可能不同”的假设，不能证明当前镜像的确切原因或普通HAP已成功。公开NAPI Ark创建probe也在main前因runtime依赖加载失败（guest127），没有执行ICU，不再追加内部库或目录设置。下一步只在隔离正常HAP/NAPI测试片中运行同一helper/原固定expected，保持无开发GUI、无正文日志和原产品源码不变；原CLI gate保留失败。证据链与API边界由独立审计保存，未据此擅改产品helper。
