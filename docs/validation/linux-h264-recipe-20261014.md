# Linux ARM64 H264 固定配方接线

2026-10-14，`af28442`。十一文件实现经独立审查Approved，无确认的P0–P3问题。本记录范围为来源、ELF及包材料接线；实际Linux FFmpeg构建、目标运行、ABI、数值、基准与CI尚未执行，不代表完整codec或真实远程客户端已完成。

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
