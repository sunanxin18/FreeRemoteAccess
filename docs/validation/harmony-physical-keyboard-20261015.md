# 鸿蒙有限实体键适配（2026-10-15）

状态：开发中。本片完成OHOS应用侧有限实体键源码、宿主consumer回归及真实Release HAP构建；不代表手机或PC实体键已实测，不包含系统软键盘/中文IME。PC仍由用户填写登录表单，本轮没有读取、点击、重装或连接该实例。

## 实现范围

ArkUI KeyCode通过本机正式SDK定义归一为USB HID，补齐小键盘0–9、四则、decimal/Enter、ScrollLock和Menu。有限profile包含101个HID：`04–31, 33–47, 49–63, 65, E0–E2, E4–E6`。小键盘保持物理位置，NumLock不在本地改成主键区或猜Unicode；Meta及既有系统快捷键继续归本地。

当前RDP对Pause `0x48`、国际键 `0x32` 无扫描码转换，未知Down/Up可使active session失败。ISO102ND `0x64`虽在RDP有物理转换，Apple却将其并入普通反斜杠符号，尚无共同ISO布局语义。本片拒绝这三种usage及其他未资格键；102ND规范映射知识保留，不能据此宣称可发送。没有改两个协议、桌面默认路由或共享能力类型。

普通旧`AppEngine::spawn`保持keyboard backend不可用；真实OHOS ARM64组合根显式安装`ArkUiLimited`。它只提升OHOS的platform/product physical轴，text保持false，协议能力仍需交集。既有完整首帧、前台/实际焦点、session/generation/revision及队列屏障继续约束发送。snapshot只增量提供固定backend原因和effective physical/text布尔，不记录输入正文、长度或按键身份。

新Down在入队前被明确拒绝：未安装backend返回现有`-4`，未资格返回`-2`。真实旧来源Up先走账本清理；收窄后不能再发送的held键通过既有ReleaseAll清理，不把unsupported Up交协议。孤立未知Up不释放旁侧合法held。ABI返回0只说明命令被队列接受，不是远端执行确认。

## 验证与产物

使用绝对Rust1.96 cargo/RUSTC/RUSTDOC、独立target目录；ETS使用本机正式SDK loader。实际SDK enum作为输入、独立USB HID期望作为oracle，执行真实ETS映射及从ArkTS AST抽取的Index派发方法。保留真实行为RED：ScrollLock缺失、小键盘无事件、Pause下发、旧来源尾Up丢清理，以及Rust无backend仍入队/未资格事件到下游。路径/编译/测试夹具等待字段错误及一次SIGABRT另行记录，未算作产品行为RED或删除。

- 完整FFI宿主：69 passed、0 failed、1项原有ignored；doc0，exit0。
- 全部六个ETS测试文件：67个内部用例通过，Node顶层12通过，exit0；登录数字和密码Enter回归保留。
- ABI→Engine FIFO→Owner→测试ProtocolSession实际捕获101个HID的平衡Down/Up；含无backend、非法键、首帧前、失焦、旧generation/revision、队列满及ReleaseAll反例。presentation feedback来自已有测试机制，非GPU/网络证明。
- 双协议feature宿主check通过；四条原有RDP dead-code警告保留。宿主不编译OHOS-only Apple分支。
- 真实官方SDK ARM64 Release双协议HAP构建exit0，签前checker通过。unsigned SHA-256为`1304d2b623500eb37a8e0f807f8a255fe4e08097fff8ce3b6095edfc32de1e1c`；包内FFI为`3c8f8e2c73b7ab18e488adb439d9fb2a6e8ba6c069a7f803a5dee369841468b9`。构建前后11个冻结源码hash不变；本片未签名、安装或启动该包。

完整diff SHA-256为`016f833e58def3bd3f21a616ebbfa89891fbdbc90eba5064876c0eb3335a8fa2`；源码、SDK、24份命令日志、真实构建及包内身份记录在`target/harmony-physical-keyboard-20261015/`。独立Spec/Quality审查通过，未发现需先修复的问题，结论限冻结源码及宿主证据；root另行完成上述真实OHOS/HAP构建，不由独立审查推导设备运行已通过。

## 后续门

OHOS片consumer到测试ProtocolSession队列边界。后续独立RDP tests-only子步骤追加7项固定oracle回归，实际通过`translate_input`/`RdpInputState`进入IronRDP Database，精确比较101个HID的最终FastPath code/EXTENDED/RELEASE、孤立Up、独立held/ReleaseAll、重复Down、stop/resume、modifier不自动合成及锁键无Synchronize。0x32/0x48仍InvalidScancode，测试证明本地Database不污染，不证明active session遇错仍存活；0x64只保留RDP0056合同。

该子步骤绝对Rust1.96 focused7通过、完整RDP359通过/11项既有ignored、doc0；独立Spec/Quality复审通过。生产前缀及原测试逐字节不变，新增仅`crates/frd-protocol-rdp/src/input.rs`测试区338行；最终源码SHA-256为`08939d5760851c2ebe65432d80f05e0531141f55903110a70c48e583fe0dd6aa`，完整日志在上述证据目录的`rdp-consumer/`。固定expected不从OHOS或生产映射生成。Apple keysym完整固定oracle、其他架构执行及真实远端编辑仍未验证；RDP事件consumer通过也不等于网络送达或设备输入验收。

ReleaseAll只解除已记录按住状态，不同步或重置Caps/Num/Scroll锁态。现有RDP根据远端布局/锁态解释物理位置，Apple小键盘固定KP数字keysym；不能宣称本地LED同步、NumLock两态远端结果、国际布局或锁键长按正确。

仍需分别验证PC实体键、手机外接键盘（若宣称支持）、真实编辑、进岛/回surface、旋转/后台/断连重连无卡键。正常系统IME的attach、自绘预编辑事务、唯一实时提交和正式键盘入口属后续串行工作；text仍不可用。本片不证明Mac真实登录、完整Harmony客户端或所有架构完成。


## Apple 101 HID 真实 writer 消费回归（2026-10-15）

Task5C1A补齐本记录此前明确OPEN的Apple消费边界。仅在原runtime.rs测试模块末尾增加include子模块及446行独立测试源，原112286字节的生产和全部旧测试可逐字重建相等；Cargo.lock及76项Apple/共享依赖清单保持，唯一已有文件变化为测试模块追加。没有修改产品协议、decoder、OHOS、平台能力或实际键盘广告。

固定101 HID的plain/shift keysym逐项列出，来源为USB HID Usage Tables 1.5键盘页、libxkbcommon 1.7.0正式X11兼容头和RFC6143 KeyEvent/PointerEvent布局；期望不从生产映射或encoder生成。真实InputEvent经PointerWireState、AppleWriterHandle与实际writer线程发送到127.0.0.1:0 TCP，Standard直接比较字节，HP读出真实密文帧后沿用现有SessionCrypto打开plaintext并比较同一手写字节oracle。该inverse不构成新的独立密码学证明。

两模式各5项，共10项：101键plain/shift Down/Up共808个KeyEvent；held/repeat使用首次keysym、孤立Up；ReleaseAll的按usage顺序KeyUp及完整pointer尾包；modifier元数据不合成事件、六实体modifier及主键盘/小键盘和锁键边界；unknown/Text不污染旁侧held。真实writer等待write_all结果，负断言只接受实际TCP限时无字节，额外包/EOF/其它错误失败。没有给Pause、国际/ISO、Meta或Text增加资格。

绝对Rust1.96、offline/locked、独立target实际运行：聚焦10通过；完整默认Apple包550 unit通过、11项既有ignored，auth integration4和session integration1通过，doc0。包格式与include源单独rustfmt检查通过。无编译/测试失败；原audio_video_port_announcement_message dead-code warning保持，资料获取失败与后续成功hash均保存。

独立审查为Spec PASS / Quality APPROVE，无Critical/Important；审查报告SHA256为2b6b8f7e687636e9658ad88523a7b2d8a83553927972c618cf07fd4f0a578806。root完整读取两报告/测试diff并核对76源、原Git对象及旧代码完整保持、两冻结源和36证据索引。新测试源SHA256为a48d8b0ecf214e694b823fef3caf28913ef4a40d781d9db95d1eabc3977a131a，runtime为18f8672c3b055485b991dd6d5a6348a7d8b5e0b78e64e2e330a5d9e3bdba1ba9；完整作者与root证据位于target/harmony-apple-key-consumer-20261015/。

本项只关闭Apple有限US样式keysym到本地writer/TCP的宿主回归缺口。SessionCommand/Owner/generation准入、真实Mac登录/执行、非loopback送达、设备键盘、国际布局、系统锁态/LED和IME仍需独立验收；没有观察或操作正在填写的PC。鸿蒙系统IME焦点桥的未提交修正独立跟踪，不由本测试或本提交授予资格。
