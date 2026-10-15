# HarmonyOS 正常会话系统输入法（2026-10-15）

状态：开发中。正式会话系统输入法已完成主机测试、目标HAP构建与焦点修正后的独立复审。手机正常应用/NAPI上下文中的固定Unicode字素合同已实测通过，测试后已恢复签验的正式包；真实系统输入法与远端文字输入仍需独立验收。原生来源桥的既有结果见[来源桥验证](harmony-ime-owner-20261015.md)。

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

用户已确认 PC 数字输入恢复，同时明确“还在填写，暂不操作 PC”。因此本轮开发期间不读取或操作 PC 界面，不安装、重启或连接该实例。手机固定原生合同的后续结果见文末，不能替代真实IME、PC或真机验收。

## 首个最终候选与实际反例

2026-10-15候选的18个源/测试路径固定于 `target/harmony-system-ime-20261015/source-freeze.json`，清单SHA256为 `c3b02e7b71a013095d59768a14f93efa0323871384d935162537d4925103163f`，root逐项核对源码、作者报告和三个产物身份。实际Release双协议unsigned HAP为 `29a8d3bc34130eba909eedc063bcdbda643120fdfbc71422792a280518b7cc7c`。100项ETS consumer通过（原67项保留），Rust91项通过/1项既有ignored，Python49项中47项通过/2项既有双recipe跳过。官方SDK构建和精确依赖检查通过，ICU只由libentry引用，FFmpeg材料与INTERNET唯一权限保持。保留四项既有RDP helper dead-code与Hvigor未配置签名提示。以上是候选源码/主机/包结果，独立审查尚未批准。

首次候选前，root源码阅读已发现并要求修复“取消尚未实际attach的来源仍detach”和“detach永不完成使后续来源无限普通重试”两项问题；各自确定性RED保留，修正后重跑最终构建与上述测试。原临时报告、清单、日志和HAP保存在 `prefreeze-base/`，不能用其身份代替最终候选。

独立审查随后确认另一个焦点反例：同一存活XComponent在连接中已经获得真实焦点，page2→page3时会话岛epoch变化却清除这份焦点记录。触摸“键盘”请求同一个已聚焦控件时，不一定再有onFocus，实际consumer到2秒仍零attach。精确反例与新page3真实onFocus的成功对照在 `target/harmony-system-ime-20261015/review-focus/`；仍需修复和复审。

root还在手机模拟器后台执行了固定原生字素程序 `b4d711e132e8d499b3c9f78443468fcc5bc9afaae3ecc7b1e9d5c0b1d5061a4a`：host返回0，独有guest退出码为2，属于失败。上传ELF与所选SDK C++运行库哈希精确，API26/ARM64身份已核对。首次主机夹具对API版本末尾空白的断言过严，保留原始成功观察，修正解析后继续，未因此重发身份命令或改设备配置。数值诊断定位到第一个非空fixture的cursor0，helper返回invalid。

进一步仅调用公开ICU接口的独立程序确认系统版本74.2.0.0，root、空locale和en_US的`ubrk_open(UBRK_CHARACTER)`均返回error2且iterator为空。该probe的guest0只表示成功打印诊断，不能算字素创建成功。证据在 `target/harmony-system-ime-20261015/root-phone-native/`；尚待区分独立CLI进程与普通应用上下文的数据初始化要求，不预判为emoji规则或产品接口不受支持。没有读取/修改系统数据目录、调用私有初始化、安装辅助服务或操作应用界面/PC。


## 焦点修正后的源码/HAP复审

Fix1只修改Index与实际surface consumer，其余16项冻结源保持。原18源、报告、manifest、diff、HAP和审查结果精确归档在 `review-fix1-base/`。根因修复保留仍未blur的ArkUI节点记录，同时在窗口/会话边界关闭来源并撤销native输入授权；回前台不自动attach。原page2→3反例从零attach转为一次attach，不需要第二次onFocus；后台有/无实际node blur均有独立对照，真实blur继续等待真实新焦点。

作者先保留两个真实RED再转GREEN，最终受影响七套80项ETS通过（surface13、原mapping/login/island/fullscreen/orientation/display保留）；未重复无变化Rust/ICU/Python套件，先前91项Rust及49项Python结果仍只按其同字节源码/原日志记录。实际官方Release双协议HAP与精确checker再次exit0，已有warning仍保留。同一独立reviewer scoped复审为Spec PASS / Quality APPROVE，I1关闭，没有新增必须修复问题；旧fixture压缩排版及既有warning记录为deferred。

最终18源清单 `fix1/source-freeze.json` SHA256为 `8bc6ba72e55c473174d9152871e6c1e4674125fde175f0438d5c0dd2a13169f7`；unsigned HAP为 `a92d5fc302c8229dd5ea18fae9802faf92b777092c1f9ba7d3ced2a0797d0bbb`。root已逐项复核18源、完整修正差异、报告原prefix保持及三个artifact身份。截至Fix1源码复审时尚无签名、安装或正式应用系统IME运行结论；后续结果见下文。

公开ICU运行时审计已进一步核对官方固定源码：Ark首VM创建有独立的数据初始化，而公开ubrk wrapper只转发。该机制支持“普通应用与纯CLI上下文可能不同”的假设，不能证明当前镜像的确切原因或普通HAP已成功。公开NAPI Ark创建probe也在main前因runtime依赖加载失败（guest127），没有执行ICU，不再追加内部库或目录设置。下一步只在隔离正常HAP/NAPI测试片中运行同一helper/原固定expected，保持无开发GUI、无正文日志和原产品源码不变；原CLI gate保留失败。证据链与API边界由独立审计保存，未据此擅改产品helper。


## 手机正常应用中的固定字素合同

2026-10-15，Task5C2N隔离测试片经过独立Spec PASS / Quality APPROVE后，由root在手机API26 ARM64实例执行。没有修改正式18项源，也没有额外开发页面、权限、私有初始化、系统数据目录设置或后台服务。测试仅在正常`RegisterFrdApp(env,exports)`入口运行一次，直接include原八项positive、七项negative及全部cursor expected，并复用正式纯原生helper；Release下使用显式返回码，未依赖可被关闭的assert。

隔离unsigned HAP为`83f9c2664a5083aac73f2768159ca59512bbeb855cda8726cab0e33560bba9f3`；31条目中仅libentry与正式包不同，其余30条目解压字节相同。现有SDK调试身份的控制台签名、官方验签和双normal参数checker通过，signed HAP为`a1a5c022a9eb20497df28b3fa5bae8f1b1c2a3776049c24a0e2d5d829e4e17a8`。签名器增加`.pages.info`，原31条目解压字节全部保持；最初按全ZIP名称相等检查因此拒绝，随后明确比较新增签名元数据与全部原payload，不把该拒绝当作包损坏。

手机安装前专用tag日志为空；覆盖安装后启动得到新PID4968，设备epoch1792068659至1792068662之间，仅有以下本次入口/终态记录：

```text
1792068660.864 PID4968 FrdImeContract: runId=20261015-appctx-01 envNonNull=1 fixedContractStatus=-1 completed=0
1792068660.869 PID4968 FrdImeContract: runId=20261015-appctx-01 envNonNull=1 fixedContractStatus=0 completed=1
```

实际原始日志另存，没有只凭固定runId搜索历史PASS。本次同时满足env非空、原固定runner返回0、完成状态1；因此仅将“该手机镜像正常应用NAPI上下文中的固定helper合同”记为通过。原standalone guest2与Ark CLI main前guest127仍保留失败/阻塞。不同上下文结果支持此前初始化机制的解释，但不证明当前商业镜像唯一根因，更不等于真实候选时序、用户文本NAPI或Windows文字呈现通过。

随后停止诊断进程，以同身份`install -r`恢复正式unsigned `a92d5fc3…`派生的signed HAP `91a81d2deea551a69d905d729b80e2fdc06031e78f0be042ef371cea675e7acc`。官方签验/checker通过，原31payload全部保持，正式新PID5758没有诊断tag；日志中旧PID4968记录未被清空。普通最近连接仍在，选择后实际显示“已从系统安全存储载入密码”，未重新输入或导出密码。PC全程未观察或操作。

冻结片、审查与签验/执行/恢复证据分别位于`target/harmony-system-ime-20261015/app-context/`、`.superpowers/sdd/2026-10-13-harmony-clients/task-5c-ime-app-context-fixture-review.md`及`target/harmony-system-ime-20261015/root-app-context/`。其中`device/diagnostic-verdict.json`和`device/clean-restored.json`分别记录两个不同结论，不把诊断hook保留在正式安装包。


## 正式包手机真实会话：桌面成功，IME唤起未通过

同一恢复后的正式包中，用最近连接载入HUKS密码后通过原普通表单发起Windows连接。首轮随后回到表单且密码为空，未捕获中间错误，不能声称已修复或推定网络/认证原因。第二轮保留即时状态，20:55:08“正在连接”、20:55:11“等待首个完整画面”、20:55:28为2760×1256全屏XComponent和右下已收起浮动岛；20:55:38实际截图可见Windows PowerShell、任务栏及当时桌面，属于新正式包的有限真实登录/呈现结果。

20:55:54首次键盘操作引用了15秒前的浮动岛ID，而控件已经自动收起，不能据该次点击宣称按钮回调或IME失败。之后以同一脚本读取当前收起入口→展开→读取当前键盘ID→立即点击，20:56:37实际出现持续提示“系统输入法暂时不可用，请重新点击键盘”。20:57:44先真实触摸远程画面、再读取当前按钮并立即点击，20:57:49仍为相同提示，无系统键盘。该结果明确为真实IME唤起未通过，不能由此前固定ICU合同或mock SDK consumer绿覆盖。

当前提示合并多个失败分支，尚无SDK数值错误或阶段证据，不能将其直接归因于ICU、真实焦点、SDK订阅/附着或Owner来源确认。已转入只读审计以确定最小无正文诊断，禁止凭猜测放宽输入来源门或增加私有API。现存界面记录位于`root-app-context/ui/phone/`的`ime-open-*`和`ime-retry-*`；PC没有观察、点击或安装。

20:58:30通过当前浮动岛断开动作主动结束手机会话，20:58:32已回普通表单。既有generic输入法提示仍留在表单，后续重试是否产生新的失败不能单凭相同旧文案判断；真实候选与文字结果仍未通过。
