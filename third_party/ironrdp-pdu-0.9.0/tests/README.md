# A2.7P Surface optional-header 长度前门回归

2026-10-14。在 macOS ARM64 使用本机精确 Rust 1.96.0；所有常规 Cargo
命令均 offline/locked。本目录不是完整上游 PDU suite（上游 dev dependency
`expect-test` 不在当前 cache 中），也不添加或下载依赖。runner 从 Cargo JSON
artifact 获取真实 shipping rlib，再用同一工具链编译窄 public-library tests。

```sh
python3 third_party/ironrdp-pdu-0.9.0/tests/freeze_surface.py
python3 third_party/ironrdp-pdu-0.9.0/tests/freeze_surface.py --release
python3 third_party/ironrdp-pdu-0.9.0/tests/run_surface_tests.py
python3 third_party/ironrdp-pdu-0.9.0/tests/run_surface_tests.py --release
python3 third_party/ironrdp-pdu-0.9.0/tests/test_surface_mutations.py
```

freeze 仅使用独立 oracle workspace 的 registry-only 原图；校验完整 archive
SHA和126个原文件，然后在独立进程运行真实 public EXT/Set/Stream/FastPath API。
860条记录包括 optional/no header、retained unknown flags、长度0/4/MAX-24/
MAX-23/MAX、有限0/3/4/23/24/27/28/29byte buffer，以及fixed part短buffer。
expected固定为 `fixtures/surface-original-64.txt`，SHA
`32aa7d326240ed7bf8ba9b897b8ee5bee5163a4bb180f132ca30b9948ecd84e6`。
debug/release实际原版输出相同。`SURFACE-PROVENANCE.json`记录原来源、observer
hash和实际pointer width；不得从生产构建动态生成expected。

public PDU回归直接消费补丁后shipping library，比较冻结原版记录，并单独核对
四header字段端序、borrowed slice内容和地址、剩余cursor位置。public session
回归用144个完整malformed Surface FastPath帧，通过两种现有default/native
policy核对精确DecodeError source、不产出Region/None、无output，以及非零
sentinel图像和cursor state保持一致。新建processor无pending mouse；证明只限
这一PDU前门，不能推导所有session输入或多命令图像事务回滚。

临时actual-session-source wrapper只加入 `process_surface_commands` 和
`pointer_rendering_begin` 入口计数；真实bad-frame回归计数都为0。另一个单独
命名测试把实际PDU源码抽出的长度算术leaf以u32执行后，将其typed error注入
真实session的现有decode-result dispatch，验证 `Other` 传播和 `InvalidField`
mutation被吞后失败。此合成错误注入不等于shipping PDU在64位上溢出，不能
称为i686运行。长度算术leaf抽取不包含替代wire parser。

原版源码修改前的 `--original-red` 以完全冻结原source hash校验后执行：debug
u32 arithmetic发生overflow panic，release回绕后得到Ok(0)并使typed-error
assertion失败。后续源码已修补，该选项故意拒绝把patched源码冒称原版RED。
mutation runner另行在临时副本重建unchecked leaf，并拒绝两个profile的unchecked
addition及错误的InvalidField variant；实际session synthetic dispatch也能捕获
InvalidField返回Ok([None])。

i686 Windows/Linux `cargo check --lib`是编译层证据；本机没有可运行这些目标的
runtime/emulator，本次没有32位完整PDU或session实跑。64位public parity、u32
arithmetic和synthetic dispatch必须分别报告。没有decoder性能、完整上游suite、
目标服务互通或设备验收声明。上游normalized manifest和原source单测保持不变。
