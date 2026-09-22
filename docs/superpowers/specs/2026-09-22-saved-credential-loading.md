# 保存连接的异步凭据加载

## 问题与证据

macOS ARM64 新包登录页可以列出保存连接，但选择后地址和用户名仍空白。
2026-09-22 对当前进程做只含调用栈的 sample，确认
`frd-profile-credential-load` 停在 `SecItemCopyMatching` 的系统解密调用中。
原设计直到密码加载结果返回才选择元数据，并在 worker 在途时忽略所有改选。
迟到结果还会重新填入元数据，覆盖用户的手动编辑。

## 行为与边界

- 选择连接立即填入非秘密元数据，密码读取独立进行。
- 等待期间显示中性状态，允许编辑和手动输入密码。
- 密码读取结果只有在请求身份、当前配置和密码编辑版本均匹配时才能应用。
- 五秒超时后显示可操作的读取失败提示；晚到结果不得覆盖用户输入。
- 一个系统读取尚未退出时不再创建第二个读取线程，避免系统服务卡住时资源无界增长。
- 改选仍立即更新元数据；系统读取繁忙时提示手动输入，不把原密码带到新目标。
- 关闭客户端不等待不可取消的系统读取。平台凭据库不变，密码不进入普通配置或日志。

实现分为协议中立的表单/请求身份和桌面异步调度，macOS Keychain、Windows
Credential Manager 等仍由既有平台后端负责。不能把超时解释为密码错误或账户失效。

## UI 指引

复用登录表单的字段层级与内联状态，不新增模态阻塞。
参考 [Apple Loading](https://developer.apple.com/design/human-interface-guidelines/loading)
对持续加载提供进度反馈的要求，以及
[Material 3 Progress indicators](https://m3.material.io/components/progress-indicators/overview)
的过程状态表达。M3 页面正文依赖 JavaScript，当前抓取仅确认官方入口；不据此
声称完成全部组件视觉验收。等待状态采用文字而不依赖动画，超时提示保留至用户操作。

## 验收

确定性验证立即元数据、成功读取、超时、改选、A→B→A、密码/配置编辑、迟到结果
与单worker上限；真机验证当前Keychain阻塞时表单仍可手动连接。其他平台的
构建与已有保存凭据测试不得由macOS结果推断。

## 实现验证

2026-09-22：app、desktop shell、egui 和 ui-model library 测试合计
350 passed / 0 failed / 0 ignored。覆盖请求期限、worker 槽保留、请求ID、
立即显示元数据、手动编辑后删除密码、改选后返回原配置、表单重建及身份编辑；
分辨率调整不改变凭据身份。格式与diff检查通过。真实GUI验证单独记录。

ARM64 release 包重建及签名/结构/CLI校验通过。真实Keychain仍阻塞时，
新GUI立即显示保存的地址/用户名，五秒后出现超时提示；手动输入后提示清除，
密码字段Return发起连接。第一次得到rdp_tcp_failed，同机TCP探测可达；
重启相同包后手动登录成功，原生分辨率桌面持续刷新。
这证明超时后的手动登录路径，不证明当前Keychain读取已恢复，也不证明
第一次TCP错误的原因。启动方式不同属于未隔离变量。

鼠标曾打开开始菜单；随后的键盘测试焦点回到远端终端，Escape触发其中
任务中断，已停止按键测试。本轮不记作完整键盘焦点验收通过。

## 重复 Keychain 授权与签名身份

用户报告输入密码并选“始终允许”后，后续构建仍重复弹窗。
当前应用签名检查报告 `Signature=adhoc`、`TeamIdentifier=not set`，
designated requirement 仅为 cdhash；此前脚本对 app 与嵌套库均使用
`--sign -`。重建改变身份，对上次构建的授权不能覆盖新构建。
[Apple TN3127](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements)
明确说明 ad-hoc 身份绑定具体代码版本。密码保存使用 SecItemAdd/Update，
没有在保存前删除条目。读取超时修复不替代稳定签名。

打包现支持显式 `FRD_MACOS_SIGNING_IDENTITY`，其次读取当前用户
`Application Support/FreeRemoteDesk/development-signing-identity` 中的40位
证书指纹。没有配置时仍为 ad-hoc；配置错误或稳定签名失败必须停止，
不能降级。隔离mock验证同身份签app/全部库、特殊字符引用、失败无回退、
配置优先级与损坏配置拒绝，真实稳定签名验证另计。

用户已授权创建固定本地开发证书。证书和不可导出的私钥已导入登录钥匙串，
临时私钥文件已清理；仅请求用户域codeSign信任，不申请SSL或系统全局信任。
系统认证窗口由用户本人操作，自动化工具禁止访问SecurityAgent。

2026-09-22 系统信任命令已成功退出，`security find-identity -v -p codesigning`
确认一个有效的 FreeRemoteDesk Local Development 身份；本机40位指纹配置
已保存，权限0600。首次真实codesign调用仍等待私钥访问授权，因此尚未
替换运行中的应用，也未证明跨构建Keychain授权延续。

合并后的工作区测试：1794 passed / 0 failed / 16 ignored（78组）；
日志 `/private/tmp/frd-integrated-20260922.log`。此为本机测试，不替代其他
平台运行、签名包或实际AVC互操作验收。

签名verifier现提取应用及每个嵌套dylib的leaf DER并核对SHA-1；
完整名称模式先精确核对leaf Authority，再锁定应用证书指纹核对嵌套库。
确定性fixture覆盖指定A而实际B拒绝、应用B/库C拒绝，以及全部B通过。

### 固定证书实际签名验证

用户完成私钥访问授权后，两份内容不同的本机测试Mach-O均签名成功，
designated requirement相同，绑定相同identifier和leaf证书，已不依赖cdhash。
最新release应用另行生成于`target/macos/stable-release/FreeRemoteDesk.app`，
应用和三个FFmpeg dylib使用同一证书；真实包verifier通过结构、证书指纹、
ARM64及CLI检查。真实调用暴露`--extract-certificates`可选参数必须使用
等号形式的问题，脚本及严格mock已修正，两套脚本测试通过。

旧客户端确认位于登录页后退出，新签名客户端成功启动；旧密码条目读取
仍超时，等待用户确认是否出现并完成条目访问授权。签名私钥授权与应用
读取已保存登录密码是两项不同权限。当前仅证明稳定身份及实际签名包，
未证明真实密码自动恢复、跨构建授权延续或登录成功。

### 用户授权后的实际密码恢复与登录

用户确认允许条目访问后，重新启动固定签名客户端，密码字段自动恢复为
隐藏字符。CUA启动实例连接报告`rdp_tcp_failed`，独立TCP检查确认RDP端口
可达；退出后直接启动同一路径同一签名二进制，从最近连接选择记录，
密码再次自动恢复，提交后完成认证并显示完整远程Windows桌面。未重新
输入密码，未向远程桌面发送键盘测试。当前证据证明授权后跨进程密码
恢复和实际登录；尚不证明两个不同产品构建的Keychain授权延续，亦未
隔离启动方式与TCP失败的根因。
