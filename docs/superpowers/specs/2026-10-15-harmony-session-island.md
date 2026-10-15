# 鸿蒙手机与PC右下角会话浮动岛

2026-10-15，用户明确要求将详情/断开恢复为 Windows/macOS 同一产品的浮动岛，放到鸿蒙手机和PC屏幕右下角，尽量释放远程桌面面积。登录前竖屏、远程登录成功后横屏并继续自动旋转的已验证策略保持。

## 布局与边界

会话阶段移除额外56vp产品工具条，XComponent占满系统提供的可用内容矩形；岛为同一安全区内的普通Stack兄弟，只在右下角自身范围命中，不使用阻断下层交互的全尺寸overlay。展开、收起、详情开关不重建表面或更改画面/输入矩形；不拉伸或裁切远程画面来伪造满屏。手机在成功登录前保留系统导航/挖孔安全区；成功登录后进入沉浸全屏，隐藏状态栏、导航栏和手势指示条，远程画面延伸至屏幕边缘，浮动岛单独避让挖孔与临时系统手势区域。断开过程保持布局，返回登录页恢复系统栏与安全区。PC保留原生最小化、最大化、关闭、拖动、双击与系统菜单，不在岛复制窗口按钮。

这是用户明确指定的Harmony局部覆盖方案，优先于旧desktop-only-titlebar要求。产品理由是释放额外工具条和保持移动端可达性，例外不扩散到Windows/macOS/Linux，也不让协议、renderer或decoder依赖窗口接口。

## 交互

复用仓库固定Material Symbols Rounded字体、Apache许可与来源，24vp图标、48vp最小目标、圆角胶囊、亮暗配色及非颜色状态区别。状态/详情、断开（连接中为取消、失败为返回）均有中文可访问名称和悬停/键盘焦点提示；按下、禁用和焦点状态明确。断开中不重复提交。

成功会话空闲自动收起，右下保留48vp可发现入口，触摸/鼠标点击或现有Ctrl+Alt+Home本地快捷键展开。鼠标离开700ms收起，触摸操作结束3秒后收起；悬停、按下、键盘焦点、提示与详情保持显示。为避免没有remote-held快照时悬停扩展吞掉远端拖动的mouse-up，第一片仅显式点击/键盘展开，不新增跨远程面的悬停sensor；这是与桌面150ms自动揭示的有意差异，服务于输入可靠性和触摸可达性。

本地岛/详情取得输入前先释放远程输入；同一点击不送入远端。计时器隐藏不抢焦点，不自动声称远端聚焦；真实表面交互恢复远程所有权。Escape/Back仅在本地瞬态有效时关闭它，正常远程Escape保持原行为；本地快捷键尾部keyup不发送远端。后台、离页、新会话清理计时器与旧详情。错误与证书身份仍可持续查看，不用toast替代。

官方基线复核：Apple HIG [Layout](https://developer.apple.com/design/human-interface-guidelines/layout)、[Buttons](https://developer.apple.com/design/human-interface-guidelines/buttons)官方markdown；Material 3 [Toolbars](https://m3.material.io/components/toolbars/overview)、[Tooltips](https://m3.material.io/components/tooltips/guidelines)页面当前需JavaScript，辅以Google官方[Material Web icon-button](https://github.com/material-components/material-web/blob/main/docs/components/icon-button.md)的紧凑动作与可访问标签指导。遵循既有平台字体、安全区与原生Button状态，不增加第二图标家族。

## 验证

纯策略测试覆盖deadline、交互pin、退出/后台/新session和旧timer。独立源码审查后构建双协议普通Release HAP，检查真实字体/许可/FFI字节，手机和PC分别安装与观察位置、自动隐藏、焦点/提示、全尺寸表面。手机用已授权Windows/HUKS真实登录验证方向、隐藏后触控与唤出/详情/断开；PC本地GUI与真实认证分开记录，不将新包构建或手机结果算作PC远程成功。深色、大字、完整键盘/IME、拖动与真机证据不足时保留准确限制。

## 手机全屏补充（同日用户纠正）

横屏底部白块来自系统导航保留区，左侧浅色条来自全局cutout padding。全屏与方向策略分离；只有已呈现完整首帧的成功会话启用全屏，轮询与旋转不重复强制。窗口异步操作按阶段和请求版本串行，旧窗口结果不得修改新窗口；实际布局属性驱动页面，渲染与指针继续共用XComponent矩形。原生系统边缘手势仍可临时唤出系统栏，不能禁用系统导航。远程画面维持原比例，分辨率协商为独立功能。
