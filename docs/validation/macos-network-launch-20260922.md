# macOS 标准启动 TCP 失败调查（2026-09-22）

## 结论与边界

固定本地证书签名的 ARM64 客户端通过 LaunchServices 启动时，曾在证书预检
TCP connect 阶段失败。新增诊断得到 `HostUnreachable` / OS errno 65。
同次系统内核日志记录该进程连接被 NECP 丢弃，SYN in/out 为 0/0，
不是远端服务器拒绝、密码错误或图形解码失败。

随后 nehelper 明确记录本地网络偏好已允许，但仍收到权限请求，因此清理
并重建应用 UUID 缓存。未修改系统权限，退出后用 LaunchServices 重新启动
同一签名构建，保存的密码自动恢复，实际登录并显示完整 Windows 桌面。
本次恢复与系统自动刷新策略缓存相符，不声明所有 errno 65 都是权限问题。

## 本地证据

- 失败 stderr：`/private/tmp/frd-network-launchservices.err`。
- 同次内核日志时间：18:40:33，`reason: NECP`，error 65，零发出包。
- 同秒 nehelper：`Local network allowed by preference for FreeRemoteDesk`，
  并记录 `Clearing cached UUIDs and restarting session`。
- 成功重试 stderr：`/private/tmp/frd-network-launchservices-retry.err`。
- 两次均为 `target/macos/stable-release/FreeRemoteDesk.app`，未在其间重新构建。
- 系统设置四个同名本地网络条目均开启，未切换任何权限。
- 本轮没有向远程桌面发送键鼠测试，当前桌面正常显示。

## 实现与验证

frd-protocol-rdp 新增固定网络失败字段：stage、operation、reason、io_kind、
os_error。保留公开错误码和取消/超时行为，不输出地址、用户名、密码或
底层错误自由文本。覆盖 DNS、预检、认证连接及套接字转换/克隆等阶段。
worker_unavailable仍合并占用/创建/注册失败，不声称完全区分它们。

RDP 库测试321 passed / 0 failed / 3 ignored；macOS ARM64 release编译及
真实签名包验证通过。Windows/Linux未做本次真机验证；此诊断改动不改变
平台协议行为或生产codec gate。

更新构建第一次读取旧密码曾超时，同构建重启后自动恢复。跨进程读取与
实际登录已证实；没有足够证据宣布跨所有构建永久免授权。

Apple文档说明命令行与普通应用的权限归属可能不同，多版本应用可能影响
权限显示，且建议Apple签发证书以可靠追踪本地网络身份。固定本地开发证书
不等于Apple签发发布身份：
[TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)。

## 深入身份与缓存调查

只读解析系统网络权限归档确认4条本产品规则：无路径通用条目及debug、
release、stable-release路径条目；均为DenyMulticast=false、DenyAll=false、
MulticastPreferenceSet=true。没有发现配置层拒绝。

LaunchServices登记7份相同bundle ID的历史产物，7个Mach-O UUID互异；
当前stable-release为identifier+leaf证书DR，旧release/debug等为cdhash DR。
至少旧debug的注册签名哈希与磁盘实际签名不同，证明历史注册可能滞后；
不能据此断言stable-release注册也错配。未注销、删除或修改任何规则。

本次诊断构建于18:39:41原路径替换主可执行文件并重新签名，18:40:33首次
连接被NECP拒绝；所检查18:38–18:42日志中未看到本产品的安装通知提前
刷新缓存，第一次明确缓存刷新发生在拒绝之后，generation升到649及后续值。
同一构建重启成功。直接替换已注册.app内部可执行文件是本次实际使用的
开发更新方式，可能让UUID网络策略更新滞后；它是具体触发假设，不等于
已捕获失败前缓存内容。尚缺失败前UUID映射快照与控制变量复现，不能把
多副本、固定自签证书或原地更新单独宣布为唯一根因。

可确认的根因层级：持久权限允许，但运行时NECP策略未正确覆盖首次连接；
系统识别不一致并刷新UUID缓存后恢复。开发产物多副本/历史签名混用和
原地替换是已确认的环境及流程因素；各因素的因果贡献尚未隔离。
