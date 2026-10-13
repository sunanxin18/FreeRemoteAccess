# Linux ARM64 H264 固定配方接线

2026-10-14，`af28442`。十一文件实现经独立审查Approved，无确认的P0–P3问题。初始审查范围为来源、ELF及包材料接线。随后实际CI结果见末节；完整codec、真实远程客户端与同对象数值/ABI/基准仍未完成。

## 平台与来源边界

默认配方仍为upstream，Linux x86/i686、AMD64维持原源码、NASM/x86asm、交叉工具链及真实目标测试选择。显式`frd-h264dc-aarch64-v1`只允许Linux ARM64；未知配方或错误架构在创建目录、下载、源码重放之前拒绝。CI的`arm64_source_recipe`只影响ARM64 job，另外两个job保持upstream。Mac继续仅支持ARM64，Windows/OHOS实现未改。

共用固定归档、patch/pin、fresh pre/postimage及未改C oracle，原`ffmpeg-source-recipe.py`未修改。Linux来源与构建配置使用独立contract，不套用OHOS线程策略或schema。修改bundle为固定九成员（三库、许可、NOTICE、patch、recipe、provenance和对应源码ZIP）；ZIP为固定十一成员，包含原归档、实际patch/pin、必要helper/builder/verifier、许可与重建说明。仓库受信bytes决定允许集合，不能用ZIP自己的manifest授权变化。

共享desktop helper新增Linux profile后，未来Mac源码ZIP的helper摘要也随之变化。已验证的`f6bdda0`Mac应用及源码ZIP仍对应其冻结checkout；新Mac构建需要重新生成匹配材料。本次没有修改历史产物或放宽素材检查。

## 内核与包门

Linux adapter检查LE ELF64/AArch64/ET_DYN、真实定义的FUNC符号、唯一size276、可执行PROGBITS与file-backed LOAD的线性地址/偏移映射。数学代码SHA256固定为`1e70d378b34fb196afd0942b9bd0c31c1715b4042c6c3e8f032dad5e76b42476`。builder在make后、install剥离前冻结真实内核记录；complete再次比较同源未strip库，并核验最终shipping副本。最终全库SHA用于复制身份，不能代替核授权或运行分派证明。

普通文件、无symlink祖先、owner/mode、ELF/SONAME/依赖/RPATH、插件ABI及完整package文件/目录树门保留；相对buildroot在首次使用前按仓库根统一，暂存产物通过验证后才替换。显式`FRD_FFMPEG_KEEP_BUILD=1`仅在成功后保留实际配置树、对象及prefix供目标后置验证，默认与失败仍清理。

## 当前验证层级

- 作者来源/配方测试12/12（13.508秒）：cached fresh重放、原C oracle、源码ZIP及材料篡改拒绝、实际小ELF符号/LOAD映射/容器/代码负例、剥离后record、复制和相对目录门。
- 包合同27/27（5.470秒）：原合同与三个upstream profile、错误架构、modified copied package资源/完整树/材料负例。
- Mac共享接口的非签名回归3/3；受影响Shell/Python/YAML及diff静态检查通过。
- 独立审查完整冻结91,329-byte diff并核对十一文件hash及保存的Linux日志；没有重复运行绿色套件。

小ELF以此前实际批准的276字节核经.incbin、clang/lld交叉链接生成，没有执行核。stager的GNU工具/Cargo/最终产品verifier及package readelf输出部分模拟；这些结果证明合同和文件操作，不能当作Linux宿主、完整FFmpeg、动态链接或正常客户端运行。作者scope与日志位于`target/task4c5c2-author/`。

下一门使用native Linux ARM64同一次builder的真实DSP/idct/init/NEON/startcode对象和同prefix avutil，执行原C对照、30种分派、ABI/保护页与三轮有界基准；正常bundle/native fixtures及最终完整包加载另作实测。x86/i686、AMD64本轮目标回归独立记录。默认生产选择及codec广告不因这份静态记录改变，完整Task4C5保持未完成。

## 实际 Linux ARM64 CI：构建通过，后置资格失败

冻结 `37ca646e015834ee98e14c4587c3f48d01283180` 的显式修改配方 [run 38452572725](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38452572725) 在原生 `ubuntu-24.04-arm` 上执行。实际 builder 返回0：三库、修改来源ZIP及bundle材料检查成功，插件12项与真实解码fixture6项均通过。配置为GCC13.3/AArch64，原生Linux6.17/glibc2.39；来源ZIP SHA256为 `d5f4872753bd16d4d5170b26f7e6271f936cb99080dcb3d2216c6145a9d2582f`。

随后qualification runner在读取保留prefix的 `libavutil.so.60` 时拒绝符号链接，整个run失败。FFmpeg安装规则把SONAME别名链接到同目录版本文件（avutil `.60.26.102`、avcodec `.62.28.102`）；当前runner对所有输入使用禁止链接的通用门，错误地拒绝了这个正常安装布局。数值/30分派/ABI/保护页与三轮基准尚未开始，完整客户端包步骤也未执行。需单独修复这两个精确安装别名的解析、实际加载路径与检查前后身份；不放宽source/object/package路径门，不把构建成功当资格成功。

root下载该run实际诊断artifact并用当前受信bundle verifier重验：九成员、来源ZIP受信十一成员、配置/源码/材料、三个最终库hash与固定276字节核全部匹配。未strip库实际核地址/文件偏移147728、`.text`、固定hash仍为上述pin；installed prefix副本与bundle一致。下载过程可能解引用链接，这次本地复验只证明下载bytes和来源身份，实际别名布局依据target失败记录与固定FFmpeg安装规则，未在Mac执行Linux代码。完整builder日志、失败qualification JSON、artifact与root重验JSON位于 `target/ci-h264-linux-20261014/`。

同冻结commit的默认upstream三架构 [run 38452184404](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38452184404) 均完成成功，Mac ARM64、GTK及GL三架构回归也成功。这些各自有界CI结果不替代修改配方的失败门，更不证明真实远程登录。
