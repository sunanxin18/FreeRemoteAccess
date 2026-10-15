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


## RDP真实文字序列化与writer回归

Task5C2R仅在既有`active_session.rs`测试模块追加310行，生产前缀和全部原测试字节保持。独立Spec PASS / Quality APPROVE，三组新consumer实际经过SessionCommand、ProtocolRuntime身份门、command drain、固定IronRDP FastPath encoder、output router及OrderedRdpWriter，最终检查RecordingIo字节，expected不使用同库decoder或生产UTF16转换。

固定`A中😀`、两个相同独立提交、空串和NUL/Tab/CR/LF保持预期Unicode字节；长ASCII/CJK/emoji及127ASCII+emoji四命令FIFO分别129/43/65/2PDU，255事件满包769字节，所有码元顺序、声明长度、pending排空及空stop无额外释放均验证。另一session及不匹配generation被拒；该负例实际generation2、当前1，不称为“旧generation”，作者冻结报告原措辞作为非阻塞Minor保留并在此澄清。

Rust1.96 macOS ARM64离线聚焦3项、完整RDP362项通过，11项既有ignored未启用；限定格式及root Git diff检查通过。最初新增夹具导入SessionInput错误E0603保留，修正后通过，无生产缺陷修复。冻结源码SHA256 `49592625f89d8d7fb25347a9ab34b0cc7192300c65efaf625637f4bb2a5e6857`，证据在`target/harmony-rdp-text-wire-20261015/`；root与独立reviewer分别核19项artifact、真实base及原源码字节重建。这只增加宿主序列化/writer层证据，不证明HAP/native、真实socket、系统IME、服务端Unicode能力或Windows应用文字结果。


## Ability几何生命周期与阶段诊断（Fix1复审通过）

Task5C2P1候选只改鸿蒙三个ETS源和三个测试文件。EntryAbility的键盘避让/窗口尺寸通知不再伪造失焦，捕获真实窗口/stage拒绝旧窗口通知；实际窗口失活、后台、销毁和XComponent blur仍撤销来源。现有native drawable尺寸不匹配拒绝、Index onAreaChange→Owner set_geometry释放held/revision通路保持，不从几何通知或轮询伪造focus=true。实际Ability→Index→adapter消费者分别保存avoid/size/旧窗口RED，再转GREEN；原生endpoint是已读合同模型，不声称本轮执行了Rust/native输入。

内部`FrdIme`专用tag每个显式request有界记录状态跃迁和首终态，区分焦点/来源门、controller、14项订阅、SDK attach、C source、Owner exact ticket、selection及关闭。仅固定数值、门位/状态位、比较boolean和经过signed32安全整数检查的SDK.code；不记异常正文/stack、输入正文或长度、完整snapshot/ticket/context、账号/地址或凭据。诊断callback错误隔离，不改变输入或释放SDK串行链；重复poll和正常日常selection不刷日志。新请求/断开只清属于IME的旧notice，保持无关持久错误。

首轮十套ETS121项通过。最终订阅错误处理由throw改为显式boolean后，adapter26、surface13、lifecycle7重新通过，另75项使用同轮未变路径记录。首个正式build1成功但新增SDK may-throw警告；build2直接重抛和build3 BusinessError cast均被官方ArkTS limited-throw规则拒绝（exit255），失败日志和实际/重建源码身份明确保留。最终boolean冻结安全code/index、禁止继续attach，build4及独立精确双normal checker均exit0，没有新增ArkTS警告；四项既有Rust及材料/签名配置警告仍保留。

冻结20源清单SHA256 `d08508bfd525bb2a2aabe8d51dccd43b1e6aec22cce87429e4b0c55fbee1c23c`；六文件差异为`55fc979cf14a556899f6c8bc2d214b6ad5de4a3cf449866b5a8fc84e7c13b982`。最终unsigned HAP `35bb471874e101473575fd5ea71f1cede1e392b4eb1d98ff7bfb7a5b31aa69c6`与旧正式a92d包31条目同名同序，仅modules.abc变化，其他30项解压字节相同，包括17263992-byte FFI `221cc401…`。未strip FFI源文件6fc→752改变，原因未证明；不能把可能的非运行元数据因素写成已证根因，也没有用旧库替换包。

首次冻结后Root核对20源、149项证据、完整报告、三份生产源diff和实际HAP，并提交独立Spec/Quality审查；初审结果见下文。该候选尚未签装，手机保留旧正式包断开的普通表单，PC按用户要求未观察或操作。即使此几何缺陷修正通过审查，也不能据此认定已解释上一轮真实generic失败或已完成系统IME/Windows文字验收。证据分别在`target/harmony-ime-lifecycle-20261015/`和root独立核对目录`target/harmony-ime-lifecycle-root-20261015/`。


## 已提交源码的桌面CI回归（独立于当前未提交候选）

`18cc0be`最初实际触发的四个workflow现均成功：通用CI `38736701759`（12:51:13UTC）、macOS ARM64 `38736701691`（13:00:37UTC）、Linux三架构客户端/FFmpeg包 `38736701629`（12:59:40UTC）、Windows包 `38736701536`（13:21:56UTC）。没有重启失败run，不把该head未触发的GTK/GL记为通过。原始最终metadata分别在`target/harmony-system-ime-20261015/ci-followup-{2,4,6}.json`。

后续tests-only提交`2f0c85b`的通用CI `38739750012`、GTK `38739750047`、macOS ARM64 `38739750096`、Linux客户端/FFmpeg `38739749975`已成功，Windows `38739750036`仍运行（最后观察13:25UTC）。该状态不用于替代当前Task5C2P1候选的独立审查或目标验收，也未将任何构建当作真实输入法互操作证明。


### 首次独立审查：SDK操作诊断归属缺口

初审为Spec FAIL / Quality NEEDS_FIX，一项Important I1：A的attach尚未settle时，同adapter显式请求B，A原watchdog会读取当前B的attachment/attempt，把未知的A attach误记为B的stage11 cleanup timeout。reviewer对冻结源码作纯内存确定调度反例，实际attachCalls=1、detachCalls=0即得到上述错误终态；旧Promise后续resolve仍保留SDK串行、来源隔离与清理，没有因此放宽输入。相关observe分支也会只凭全局sdkBusy把旧cleanup误当当前attach超时，同属本缺口。

该诊断直接服务下一轮手机根因定位，必须修正，不能因为输入安全仍有效而忽略。root已完整阅读审查并接受finding，原六源/报告/审查/清单/差异/HAP精确归档review-fix1-base。原作者进入Fix1，只修改adapter及其回归，固定未知SDK操作种类和归属、区分当前timeout与受先前singleton poison，保持firstterminal及全局fail-closed。当前未签装候选，原手机generic根因仍未定位，修正后需同一reviewer复审和正式构建，再目标实测。


### Fix1：固定SDK操作归属，复审通过

Fix1仅改变adapter及其测试，另外18条冻结路径和原149项证据保持。未知SDK操作保存原owner、attempt和attach/cleanup种类，cleanup归属在入队前捕获；旧操作封闭等待中的新请求记为process poison（stage1/outcome8），本请求自己的attach超时仍为stage5/outcome6。原始Promise仍串行await，全局fail-closed和旧firstterminal保持。

五条确定RED覆盖旧attach、运行中的旧detach、B建立后才开始的A cleanup，以及同/跨adapter的旧busy deadline；六组最终交错转GREEN，adapter32、surface13、lifecycle7再通过。初次descriptor的主机AST语法错误保留，修正后官方Release BOTH及精确checker均exit0，无新增ArkTS警告。Root核20源、旧149/新59项证据、完整追加报告和原12907-byte前缀保持；仅两源变化，完整delta已交原reviewer scoped复审。

Fix1清单SHA256 `d8bee56388f1ba23af1b98f349e2e9b2835cb4f6620fc55d9a1eb69a923f7e77`；新unsigned HAP `d11e3ec6835ee2e1d77f7dd806d4ffca2c4cceba20e67f69ac1b88b6fffbaef6`，相对35bb仍仅modules.abc变化，其余30payload字节相同，FFI运行与未strip产物各自保持。当前未签装，不能把作者回归GREEN视为独立review关闭I1，更不等于手机系统IME通过。Fix1证据位于`target/harmony-ime-lifecycle-20261015/fix1/`。


同一独立reviewer已完成scoped复审：I1 ADDRESSED / Spec PASS / Quality APPROVE，无新增Critical/Important；原审查11663-byte前缀保持，完整报告SHA256 `cc229f58a527aa1c20666c30d99b02a8ad81baa2ac01f1c917ca520ebef048cf`。root完整读取复审，确认源码/宿主consumer/HAP子步骤可进入签验与手机实测。实际手机generic根因和系统IME仍待新的目标阶段日志，不由复审结论推定成功。


## Fix1签装与手机实际失败阶段

复审后root以既有SDK调试身份签名、官方验签及双normal精确checker均exit0。signed HAP `2e3387388fac9d22c967b53cf6ee4efd7f35d0e1bb9b1d43fb633a3c64ebd8d0`保留unsigned d11e全部31payload解压字节，仅签名器新增.pages.info。手机PID从5758变为20556，安装前及新进程尚未请求IME时FrdIme日志均为空；PC没有观察或操作。

普通最近连接实际显示HUKS密码载入，21:43:05正在连接、21:43:08进入2760×1256全屏桌面，21:43:36截图确认真实Windows桌面。随后同一脚本读取当前浮动岛入口/展开/键盘ID并即时点击，没有用过期ID或手工发送页面。21:43:43仍提示输入法暂不可用，本进程本attempt的数值日志明确为：

```text
1792071821.288 PID20556 attempt=1 stage=1 outcome=0 gates=0 progress=0
1792071821.288 PID20556 attempt=1 stage=2 outcome=1 gates=639 progress=0
1792071823.364 PID20556 attempt=1 stage=2 outcome=6 gates=895 progress=0
```

原完整固定字段日志与strict解析结果保存在`target/harmony-ime-lifecycle-root-20261015/device/ime-first-numeric-log.json`及`ime-first-verdict.json`。gates895包含前台、page3、installed、协议text、合法window/stage、session/generation、context和Owner sourceAvailable，唯一缺少128的前端focused聚合位；progress0、没有stage3/4/5，说明尚未调用controller/订阅/attach。该次失败停在前端焦点门，不能再归因于ICU或SDK attach拒绝。

当前Index的surfaceHasFocus默认false，唯一路径由XComponent ArkTS onFocus置true；但该XComponent配置了libraryname=entry。[官方XComponent事件说明](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/reference/apis-arkui/arkui-ts/ts-basic-components-xcomponent.md#事件)明确该配置的焦点、按键、触摸等事件只响应C API。现有原生Focus/Blur已注册，却没有把真实焦点证据交回Index；这是与上述日志一致的接线缺陷。后续需用绑定实际surface/代际的原生焦点证据修正，不能设定焦点恒true或跳过门。按键等其它被抑制的ArkTS事件也需独立跟踪，不能由本次诊断授予物理键/多点资格。

21:49:26使用当前浮动岛正常断开，21:49:28回普通表单且旧IME提示已消失，符合本轮notice清理行为。手机保留该正式包和原应用数据；真实系统键盘、候选与远端文字继续未通过。此处定位的是本次SDK调用前的阻塞点，后续SDK阶段尚未执行，不推定其一定成功。


## 原生焦点桥候选与审查边界

Task5C2P2限定鸿蒙窗口层7路径，新增只读remoteSurfaceFocusToken(surfaceId)，身份无损绑定实际window的surfaceId、engine、native generation和真实focus epoch。仅NativeFocus/Blur改变节点证据；同surfaceId重建和两次poll之间Blur→Focus均关闭旧ticket，普通poll/geometry保持同身份。原生失焦即时走原Owner撤销，旧component的迟到事件不撤销新来源；浮动岛/详情本地所有权与窗口前台保持独立。未改Rust/CABI/协议/decoder/adapter/EntryAbility。

原native-only行为反例从真实Index attach0转GREEN；最终20项surface（含实际NAPI callback/getter生成的身份trace进入实际Index）、7生命周期、32adapter、8editor共67项通过，现有生产ComponentState及直接include生产napi_init.cpp的host callback/NAPI消费者通过。host SDK/worker/Owner为stub，不能视为设备SDK或Rust Owner执行。官方API26 ARM64 Release BOTH及独立精确checker均exit0；原有Rust/material/sign配置警告保持，无新增ArkTS警告。冻结脚本曾因两个payload预期顺序写反而exit1，修正后通过，原记录保留。

Unsigned HAP为29cfd7a88d478dc7ae5910f86d87596abacfdde9f122badacb16e507d6cc3adf，相对P1 d11e31条目同名同序，仅libentry/modules.abc变化，其余29项逐字相同；FFI包内221cc401及未strip752100b身份保持。27源清单079c1598、122证据清单cb7f86f8、完整报告cb65131b经root独立逐项核对。root不可变7源完整审查包127827字节、SHA256 88570178e3b098b38838ded39eadc05680f0b3156a096bfef1c86d1a2e378bdc已交独立Spec/Quality审查；当前尚未签装，真实SDK/系统键盘/候选/远端文字仍OPEN，PC未观察或操作。证据在target/harmony-native-focus-20261015/和target/harmony-native-focus-root-20261015/。

已提交P1的4c4bdc81ee23a03be98ef323bee9092e0dea393a正常推送到原feature分支并核对远端exact HEAD；该head实际触发的通用CI 38746662876于2026-10-15 14:03:07UTC成功。没有把未触发的其它平台workflow计作通过，也不把旧head CI授予本未提交候选资格。

后续回读同一原始Windows run 38739750036，已于14:07:08UTC成功；因此2f0c85b实际触发的5个workflow现全部成功，未重启或替换run。终态metadata在target/harmony-rdp-text-wire-20261015/root-delivery/ci-followup-6.json。此结论限该tests-only提交及实际运行矩阵。


### 原生焦点桥初审：两项Important，候选不签装

独立报告7ec5ca0e5e8e769e7a57191efe39a05550173fe2d5aa6b6662ea931fb0bbafc0完整审查为Spec FAIL / Quality NEEDS_FIX，两项Important均由root接受。I1：UI getter调用非线程安全GetSurfaceId，现有worker仍持续呈现；poc_status仅复制状态，没有窗口串行屏障，违反NativeWindowLease既有所有权论证。root最初“UI线程+有效borrowedWindow”的设计没有提供并发安全前提，不能以指针有效或API只读豁免。没有声称已在设备观察到崩溃或数据竞争。

I2：独立最小内存消费者纳入此前stub掉的真实returnToSurface，复现ready IME关闭→clearFocus/requestFocus真实焦点模拟→下一poll收到新epoch token后再次appFocus(false)。来源和remoteFocused由已恢复真变假，后续stablepoll不恢复。该问题是新桥主动撤销显式返回，不是已知的libraryname实体键缺口；反例完整命令和首个require.resolve夹具失败均保留在审查报告。

Root已归档原27源/两报告/清单/HAP共33文件到review-fix1-base，原122证据及不可变完整审查包保持；候选29cfd7a8未签装，准备的sign.py未执行。原作者Fix1采用真正worker串行的只读generation-bound surfaceId查询，必要扩展仅OHOS renderer-shell C ABI/Rust/header/tests，不改协议/decoder/Owner/desktop或原64-byte状态布局。成功/失败查询均不得污染普通renderer/Link状态。显式返回意图须绑定当前shell生命周期，取得真实焦点后恢复shell授权，IME保持关闭，无任意token变化自动授权。新增真实returnToSurface和worker/API消费者、完整FFI宿主包测试及重新正式BOTH/checker后，必须交原reviewer限定复审。

此时手机仍保留P1 signed2e338738、已断开普通表单；PC仍由用户填写，未读取或操作。真实SDK/系统键盘/候选/远端文字均未通过。Fix1的Rust FFI会产生真实新身份，后续不得沿用旧候选的FFI不变结论。


### 原生焦点桥Fix1结果与复审新增I3

Fix1增加版本1的generation-bound只读窗口身份C ABI，真正进入唯一renderer worker，在own lease上串行调用公开NativeWindow API。UI只读取数值，查询成功/普通错误不更改PocStatus或Link，实际Rust unwind仍保留原致命清理/停止。新完整FFI94通过/1既有ignored、surface23、lifecycle7、实际C++TU与helper、格式及官方BOTH/checker均通过；原adapter32/editor8源码及同轮证据未变，不重复无关大套件。

Unsigned437fe1b70d96b2bb21f86ddb29aa4487e29970ea2ff30893ee42dd8e46f5b6b0相对旧29c包仅FFI/libentry/modules.abc变化，其余28payload相同。FFI packaged22afc297697025bcac89ed1e5fd9597aca548370063e1f169f6762d7c0a283a6，未strip2069fa1f24cda323be45fc89c9c091f80f1dd5fc935650008d3e680fe470e842；libentry不再导入GetSurfaceId，只导入新renderer query，FFI负责公开原生API。原31源/旧122与新128证据、报告原14102-byte前缀及两包差异经root逐项核对；作者报告e62f2596，清单3189b9de，root限定9路径delta135033字节/253c389b交原reviewer复审。

完整复审afae3bd17a19a92a1fec6ba7b75ffd1aa7425895bca9e4b67424758dcd620bdb明确I1/I2 ADDRESSED，但新增Important I3 OPEN、Spec FAIL / Quality NEEDS_FIX。最小冻结源码内存消费者复现：pending return保存旧7:11:1，model.generation仍远程1，同surfaceId重建后取得当前7:12:1，却消费旧意图恢复shell。既有11种失效用例只改远程generation，没有覆盖native generation。root完整读取并接受，未将其解释为新节点的授权。

Fix1的31源、两报告与HAP等38文件已归档fix1/review-fix2-base；437包未签装，准备的签名helper未执行。原作者Fix2仅5条C++/DTS/Index/测试路径：在focused token之外提供正交只读native identity（engine:generation，不要求节点聚焦），发起显式return前新鲜取得该身份，后续只允许同native身份的真实新Focus证明消费；metadata本身绝不授权，未知/错误关闭。同surfaceId跨native engine/generation必须撤销旧意图，新的显式请求才能恢复；previousToken为空的首次返回也须正确等待真实焦点。Rust/ABI/worker/Owner/协议/decoder及其它冻结依赖保持，官方包与受影响消费者需重新完成后限定复审。

截至此阶段手机仍为P1 signed2e/PID20556断开表单，没有安装29或437候选；PC完全未观察或操作。真实SDK/系统键盘/候选/远端文字继续OPEN，不能由作者GREEN或关闭前两finding推定已可用。


### 原生焦点桥Fix2冻结与限定复审

Fix2仅修改允许的5路径，其余26条冻结源保持。新增正交只读remoteSurfaceIdentity与原focus token共用实际surface/worker安全门；身份在节点未聚焦时可用，但不能证明焦点或授权输入。实际returnToSurface在clearFocus之前取得新鲜native engine:generation，待返回意图只接受同一身份的真实新focus epoch；跨engine/generation永久撤销旧意图，新的显式请求才能恢复。额外RED显示generic false→true恢复可能绕过等待中的意图，现已关闭该旁路，旧IME不自动重附着。

原I3 RED、30项surface、7项生命周期及直接include生产NAPI TU的消费者均通过，包括真实getter在未聚焦时产生metadata→实际Index等待Focus→拒绝engine变化→新显式请求成功。host SDK/worker/Owner仍为明确stub，不授予设备资格。官方API26 ARM64 Release BOTH与独立精确checker均exit0，无新增ArkTS警告；未改FFI94/worker/helper/adapter/editor不重复测试，沿用精确冻结证据。

Root核31源与拷贝、原122/Fix1 128/Fix2 94项artifact及作者原26147-byte报告前缀，清单585765bf、报告02e5ccb2、限定5路径79805-byte审查包2935d04a已交原reviewer，只复审I3及新增getter直接风险。unsigned HAP642f101a相对437包仅libentry/modules.abc变化，其余29payload相同；FFI packaged22afc297保持，未strip变为5a4576d6，原因未证明，真实新文件及SDK strip匹配证据保留，无旧库替换。

截至此阶段复审未出结论，未签装Fix2或前两候选。手机仍为P1 signed2e338738，root只确认现存PID20556及时间，未观察UI或开始新会话。PC仍按用户要求完全不观察或操作。真实系统IME/候选/远端文字仍OPEN。


同一reviewer完成Fix2限定复审：I3 ADDRESSED / Spec PASS / Quality APPROVE，无新增阻断项，I1/I2保持关闭。完整报告40840-byte/SHA256 f688eead5a3332e8bceff748df186e0add51be506b860eb868b2bbbbc96d8c76，root完整读本轮追加并核旧29822-byte前缀afae3保持。该结论允许进入签验与手机实际阶段验证，不证明真实SDK或文字结果；未strip产物差异原因仍未证实。


### Fix2签装与手机新失败：订阅早于SDK绑定

root使用既有SDK开发身份签名，官方验签及双normal精确checker全部exit0；signed HAP6a7bce0a43f65623b39693a9524b9f386c21a4eed042f3f63e0b80b8e08f1d8d保留unsigned642f的全部31项payload，仅新增.pages.info。仅手机覆盖安装，未清数据。首次启动后立即pidof尚为空，稍后确认新PID1801，初始专用日志为空；不是复用旧PID20556结果。

23:06:44安全存储原密码实际载入，23:06:52正在连接，23:07:01全屏2760×1256 Windows桌面已截图确认。首次自动脚本误查不存在的frdIslandKeyboard节点，断言停止且未点击键盘，不计作IME失败。随后核对实际action4为键盘，以新鲜layout→展开→新鲜action4 ID即时点击，23:07:55出现当前请求的暂不可用提示。本PID/本attempt的新数值日志为：

```text
1792076875.073 PID1801 attempt1 stage1 outcome0
1792076875.074 PID1801 attempt1 stage2 outcome1 gates767
1792076875.120 PID1801 attempt1 stage3 outcome0 gates1023
1792076875.121 PID1801 attempt1 stage4 outcome0 gates1023
1792076875.122 PID1801 attempt1 stage4 outcome3 code12800009 subscription1 gates1023
```

这次真实进入controller和订阅，前端focus门已经越过。失败在第一项insertText事件订阅，SDK attach尚未调用；不能称为键盘附着失败。安装、全屏画面与数值日志证据在target/harmony-native-focus-root-20261015/，strict过滤/判断见device/fix2-ime-first-verdict.json。

本地官方API26 @ohos.inputMethod.d.ts:997及[官方insertText API](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/reference/apis-ime-kit/js-apis-inputmethod.md#oninserttext10)均列12800009为client detached；[官方错误码](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/reference/apis-ime-kit/errorcode-inputmethod-framework.md#12800009-输入法客户端未绑定)要求先attach。[华为自绘编辑框指南](https://developer.huawei.com/consumer/cn/doc/harmonyos-guides-V13/use-inputmethod-in-custom-edit-box-V13)也展示await attach后注册监听。当前adapter在attachWithUIContext之前调用14项listeners.on，第一项即失败，符合现场错误；下一子任务修正公开SDK顺序，保持来源/异步/清理门，不能用忽略错误继续或补权限替代。

23:08:33首次标作disconnect的操作实际为action2收起，未断开；核对action映射后23:09:05用action1真正断开并回竖屏普通表单，IME旧提示消失。PC没有观察或操作，原表单草稿保持。真实系统键盘/候选/Windows文字尚未通过，继续OPEN。


### SDK顺序候选冻结（待独立审查）

Task5C2P3仅改adapter生产源与三个受影响测试。先隐藏attach并等待真正settle，再注册全部14项监听；source发布、初始selection成功与精确Owner确认后才串行showTextInput，显示成功才ready。show=false不调用显示API。stage12单独区分显示操作及SDK错误；原1..11、固定数字字段、firstterminal和模块级未知SDK操作poison保持，late show/attach不能复活旧来源。

严格SDK模型在未attach时实际on抛12800009，旧生产/最终模型冻结重放均RED；最终70 adapter+30 surface+7 lifecycle共107项通过，包含显示/订阅/selection失败与关闭、旧Promise未settle、新请求及跨实例串行。初始夹具TypeError、一次旧终态诊断夹具、surface缺显示endpoint和lifecycle过早判断loading的失败均保留并明确与产品RED区分；初次同名RED日志被后次覆盖的事实披露，preliminary工具输出副本单列。

官方API26 ARM64 Release BOTH和独立精确checker、SDK strip比较均exit0；四项既有Rust dead-code与两材料header/未签名提示保持。新unsigned9c8d1acc38fe45e09b3fb8c33da519af4356d373c69bf97eec99cf829b59bf2d相对642f的31payload只有modules.abc变化，其余30相同；所有native及未strip FFI5a4576保持，无旧库替换。Root核31源及Gitbase9dffe17、额外5消费者、165产物、源码拷贝与报告f5cfda67，root首个核验脚本将actualbase对象误当hash的夹具错误保留，修正schema后全部通过。完整4路径132911-byte/U500审查包f7bed723已交fresh独立reviewer；当前未签装，不授予真实IME/候选/文字资格。证据分别在target/harmony-ime-sdk-order-20261015/与target/harmony-ime-sdk-order-root-20261015/。

### 实体键盘路由的独立只读边界澄清

限定审计发现当前未注册Native key callback，但Index除XComponent自身dispatch外，Stack还有普通onKeyEvent。固定官方上游源码的Native handler在未消费时返回false、焦点树允许向祖先处理，因此不能从libraryname对自身组件事件的限制推断祖先必然收不到键；上游master控制流也不能证明当前API26运行时已通。当前先保留真实物理键资格OPEN，没有新增原生键桥。Root完整读审计并核16源/Git对象、8项本地API26 SDK及20项证据；报告6d78a06b，target/harmony-native-key-routing-audit{-root}-20261015/保留对应输入/官方pinned来源/核验。

Root只执行官方Emulator -help，无instance或设备操作；当前公开scenario无通用key/down/up命令，fill不能证明真实物理键或IME。CUA先前无法选中独立Emulator且当前只获准官方CLI回退，不换HDC或AppleScript键注入。后续在正确聚焦surface实际敲键后，才区分平台事件、shell拦截、Owner拒绝与consumer；PC仍在填写，不观察或请求其现在敲键。这只是下一资格边界，不新增设备或目标输入通过结论。


### SDK顺序独立审查与真实手机通过的有限范围

Fresh reviewer Spec PASS / Quality Approved，无Critical/Important，既有构建warnings为披露的非阻断Minor。完整报告95d5854c08cc90a237d93f5d6d30a6539c10d73036223db82ffa12430bbc09b0经root完整读并核hash；跨文件31+5/165/Gitbase/包身份的cannot-verify由root先前独立receipt解决，设备项由下述新实测解决到其有限范围。没有重跑相同套件。

Root以既有开发身份签名新unsigned9c8d，signed HAP b742c9287ed1bec43e8601cbdf4a294d8a8274dbf3ade8fb4938d346dda56985；官方验签、双normal精确checker及31项payload逐字比较通过，仅.pages.info新增。仅手机覆盖安装，新PID24390（首次立即pidof为空，稍后确认）、开始前FrdIme为空；PC未安装/读取/操作。23:30:53原HUKS密码实际载入，23:31:01再次进入2760×1256真实Windows桌面。

23:31:37当前浮岛action4发起attempt1，新的严格数字记录明确：attach5成功→14订阅4无错误→source6成功→selection8成功→Owner7 exact=1→show12成功→ready9成功，原12800009未重现。request到ready本次601ms仅为有界时间观察，不是性能基准。23:31:54截图确认真实系统键盘，窗口避让至2760×479，原桌面按相同比例contain；不是由host或SDK返回值推定“显示”。

首次“收键盘”脚本在reveal时已触发本地所有权并关闭IME，随后action4实际重新开启为attempt2；不能把这个脚本称为关闭失败。后来只用reveal关闭。CLI拒绝横屏x1700超出范围，两个按旋转猜测的坐标也未打开Start；使用当前横屏字面坐标1139,1233后23:35:23实际打开Windows开始菜单，故未将上述调用当作产品指针缺陷。为使全宽键盘按键都在CLI有效范围，使用官方rotation切为竖屏；此后点击与截图同坐标。

实际打开Windows记事本，恢复旧标签时的“系统找不到指定的路径”提示被关闭；通过文件→新建窗口建立单独空白测试窗口，不在用户旧文档输入。23:40:29竖屏attempt3同样attach/Owner/show/ready全部通过，UI实高1661且系统键盘可见。仅使用官方Emulator CLI点击系统键盘真实键位，未用fill、HDC注入或直接调用adapter：

- 23:41:04，输入ni后显示本地拼音和“你”等系统候选，远端此时尚未出现未提交拼音。
- 分别提交两次“你”，23:42:35图像可见“你你”；点击系统退格，23:42:36回到单个“你”。
- 点击换行、切换英文并按字母键，23:44:29关闭键盘后横屏原尺寸截图明确为第一行“你”、第二行“ABc”，记事本状态5个字符、行2列4。大小写按实际系统键盘状态记录，不把它改写为预设abc。
- 当前attempt3最终日志为stage10/outcome9/code0正常关闭。点击远端菜单可继续操作，23:46:17关闭本次测试标签，在自己的保存提示选择“不保存”，23:47:16截图确认测试窗口/文字已移除，未创建测试文件。23:47:19正常断开回竖屏表单。

完整签验、fresh-PID数值日志、各次命令/失败、原始截图与有界判断保存在target/harmony-ime-sdk-order-root-20261015/，主receipt为phone-validation.json，最终结果图为ui/phone/sdk-order-final-remote-text/Screenshot_2026-10-15T234429.png。这新增的是API26手机模拟器→Windows RDP系统IME的有限实际互操作证据。PC仍由用户填写且完全未观察/操作；实体键盘、硬件真机、非BMP真实输入、完整编辑/选择、前后台/重连长时及Mac目标仍不由此授予资格。

桌面回归身份独立：原Apple tests-only71e实际5个workflow均成功；当前native焦点9df原始通用CI38758133053、macOS ARM6438758132773、Linux FFmpeg38758132894已成功，Windows38758133250仍运行（15:29UTC观察）。这些不证明本SDK顺序候选的设备行为，也不替换上面的真实输入证据。


2026-10-16后续CI观察：native焦点提交9dffe17的原Windows38758133250全部成功，含x86/x64/ARM64三个包及独立Windows ARM64 decoder runtime（16:07:39UTC完成）；此前其余三个原workflow已成功，因此该头实际触发的四个workflow均成功。SDK顺序提交c2e6f90仅触发原通用CI38763798663，现已成功（15:58:52UTC），含格式及Linux/macOS安全测试；没有给未触发的平台workflow补记成功，也不把host CI当作ArkTS/HAP/设备输入验收。原始完整metadata：target/task5a6-root-20261015/ci-final-20261016.json。
