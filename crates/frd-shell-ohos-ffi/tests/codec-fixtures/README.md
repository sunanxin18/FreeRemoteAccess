# 正常应用 FFmpeg 合成解码夹具

仅 `normal-hap-codec-fixture` + debug 构建在正常 AppEngine 创建后启动一次独立线程。与 `native-presentation-fixture` 互斥；不连接网络，不注册 decoder，不改变真实会话就绪状态。默认及 release 构建没有调用。

`provenance.json` 保存输入及独立参考像素 SHA256、FFmpeg 7.1.1 oracle 命令。16×16 编码/2×2 可见 AVC420、16×16 AVC444、16×16 编码/16×8 可见 HEVC444 复用现有合成输入，参考由宿主 FFmpeg 独立解码。256×144 AVC420/444、2560×1440/3840×2160 AVC420 都是明确 zerolatency、threads=1、bframes=0 的两帧流：首帧 IDR 和依赖首帧的最终 P 帧，各自一次 submit 必须返回一帧，不能等待下一输入或 EOS。

小样本逐可见行与独立平面字节严格比较，同时核对 SHA256。高分辨率样本只嵌入压缩输入与独立可见平面 SHA256，避免把数十 MB 原始参考嵌入应用。比较遵循实际 stride/crop，不读取 padding 作有效像素。CPU backend、SIMD、30 FPS、RDP AVC444 合并规则、HEVC RDP wire 和实时互操作都不是这些独立 codec 样本的结论。

每个 H.264 样本分别走 AnnexB 和 AVC 4 字节长度前缀输入；长度前缀转 AnnexB 使用真实 FfmpegBackend 的生产转换。HEVC 仅走 AnnexB。每个成功样本还检查 reset/stale generation、重复创建销毁、backend 释放后的 decoder 租约、decoder/backend 释放后的自有帧、畸形输入拒绝以及错误后重建。某一门失败即结束该样本，之后的生命周期门不计作通过。代码不调用 flush。

固定 tag `FrdCodecProbe` 输出无秘密字段。`completed=1` 仅表示循环结束；只有 `passed=13 failed=0` 才是全部样本与生命周期门通过。`before_eos_frames=1` 和 `pixels=pass` 是逐次提交事实。`decode_us` 仅含同步 submit；`wait_us=0`，没有等待补帧。整体 45 秒截止为调用间的协作检查，不是强制中断原生函数的墙钟保证；退出应用只请求取消，不让 UI 等待尚未返回的原生调用。缺少完成 marker 必须视为未完成，不能据此宣称资源已清理。

2×2 AVC 的旧 metadata 使用裁剪后尺寸；新夹具按实际 SPS 的单个16×16宏块及右/下各7个4:2:0裁剪单位设置独立 coded/visible 合同，旧源 metadata 不改写。HEVC 原截断VCL包仅记录具体结果，因FFmpeg允许跳过无效NAL；畸形拒绝门使用缺少初始startcode的完整8字节输入，不能据此泛称所有畸形包都同步报错。
