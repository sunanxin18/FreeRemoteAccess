# macOS ARM64 固定 H264 配方与实际包验证

2026-10-14。`57d3b4a`接通显式`frd-h264dc-aarch64-v1`的macOS构建、bundle、应用和对应源码；`f6bdda0`修正独立审查发现的MH_DYLIB段映射与仓库外相对目录问题。复审Approved，无遗留finding。本记录只关闭这条macOS ARM64配方/包接线与限定数值、夹具验证门，不代表完整codec或远程客户端完成。

## 实现与审查

新桌面来源接口复用原固定archive/patch/recipe信任根，旧helper、OHOS policy/schema、Windows/Linux builder和Rust decoder未改。默认upstream保持原五文件；修改配方为明确ARM64-only的九成员bundle，三dylib位于原MacOS codec目录，六个来源材料位于Resources。macOS Intel仍拒绝。修改来源使用独立macOS contract，不套用OHOS的ELF、API26或线程设置。

Mach-O门验证薄ARM64、真实符号、可执行范围、段地址到文件偏移的真实映射，以及固定276字节数学代码；重签后再次核对。初审发现section与symbol同时移位会绕过旧映射门，以及相对build-root在预检与复制之间变基准；两项都有实际小fixture RED→GREEN，修复后的18项来源/材料/符号/重签/目录回归、原签名配置及静态检查通过。这些小fixture不当作完整FFmpeg运行。

## 实际构建、数值与应用

Root从`f6bdda0`生成gitarchive，隔离并行image/session工作树。Apple M4、macOS ARM64、Apple clang21.0.0、Rust1.96.0、Python3.13.3，最低部署macOS12.0；使用缓存固定FFmpeg8.1.2归档，Cargo offline/locked。新三库bundle实际构建成功，独立原归档、两个修改源码pre/postimage、未改C oracle和同源数学核身份均校验通过。

构建器的原生插件夹具六项通过：AVC420、AVC444、HEVC444、裁剪HEVC、声明格式及错误拒绝；检查owned plane字节、出帧和对应ABI行为。这是同一个实际bundle的离线解码，不是系统后端、GUI或网络证明。

Root复用本次shipping的DSP/idct/init/NEON/startcode对象和同源avutil，链接已审私有数学harness，原C initializer来自独立固定源码。**5,974,518**组数值、舍入/回绕、内存/别名/只读保护页及ABI对照通过；CPU开关×位深8/9/10/12/14×chroma1/2/3共**30**种真实分派通过。被测内核对象SHA256`43fb729c25074f1fc2cd9ee3e5bd8c5adf8be7569bf21f57a522ee757f2113da`与bundle provenance一致。没有重新写数学公式或把另一套未对应对象当shipping结果；本轮未重复此前已绿的上游checkasm整套。

随后release `.app`实际构建、暂存并使用既有固定本地开发证书重签；三个库的签名、ARM64、install_name、依赖、来源材料、签名外payload及固定内核再次通过。实际应用CLI/codec加载校验通过；以最终重签应用内的三库为输入，六项插件解码夹具再次全部通过（0.05秒），命令及日志分别保存在`final-app-fixture-command.json`、`final-app-native-fixtures.log`。未创建证书、未启动GUI或操作远程会话。内核位于`__TEXT/__text`，地址与文件offset均14192，size276、SHA`1e70d378b34fb196afd0942b9bd0c31c1715b4042c6c3e8f032dad5e76b42476`。

完整可重放来源ZIP为`FreeRemoteDesk-ffmpeg-8.1.2-macos-aarch64-frd-h264dc-aarch64-v1-corresponding-source.zip`，SHA256`b1af17f45923450977c3483344a6dfc3054016b8da136b75d8653e0f0734de45`；含原归档、真实patch/pin、shared/platform helpers、builder、license和重建说明，逐成员校验。它与binary配套，不声明本地修改受上游签名认证。

第一次验证PATH选到系统Python3.9，因不支持tarfile data filter在源码提取处失败；改用已安装Python3.13后fresh attempt成功，没有取消安全filter。私有harness第一次链接缺少shipping startcode对象；添加实际原对象后通过，没有修改产品源码。两次失败日志保留，不能计为成功。

## 有界性能

应用构建完成后暂停本线程编译，确认没有clang/rustc/cargo/make活动，关闭profiler。实际同一harness/对象交替原C与NEON，256输入池、65536 warmup、31 batches×262144 calls，每侧8,126,464调用；校验在计时区外，三个window checksum均`6170d61369b74c40`。

| window | C median / p95 ns | NEON median / p95 ns |
|---|---|---|
| 1 | 6.039 / 6.306 | 4.761 / 5.165 |
| 2 | 6.096 / 10.136 | 4.780 / 7.763 |
| 3 | 6.107 / 6.359 | 4.711 / 4.837 |

此工作负载的局部median降低约21–23%，第二window的尾延迟保留，不筛掉较差结果。这不是完整codec吞吐、网络或呈现延迟，也不外推Windows/Linux/OHOS或其他CPU。

所有快照、实际配置、bundle provenance、输入/输出SHA、fixture日志、同对象数值命令与结果、三个基准window和实际`.app`位于`target/task4c5c1-runtime/`。入口及三库身份另见其中`app-identity.json`。Windows/Linux ARM64配方/目标执行、HEVC已识别预测分支、完整codec资格和真实GUI互操作仍待完成；NativeFirst和codec广告未因本记录改变。
