# 独立原版 legacy oracle

这是独立 Cargo workspace/process；根明确 exclude，自有 lock，四项精确 registry pin：graphics0.9.0/session0.11.0/PDU0.9.0/yuv0.8.17。没有任何 frd/vendor/path 依赖，根 `[patch.crates-io]` 不传播。原始 archive 和所有源文件 hashes 位于 `SOURCE-INVENTORY.json`，可用 `verify_sources.py` 根据实际 metadata 的 registry 路径重新核验。生产副本保留自身原始 hash 清单和 MIT/Apache 许可证。

从仓库根运行，使用 Rust1.96 的 `cargo`/`rustc` 同一 PATH（或给脚本 CARGO 路径）；构建输出均放根 gitignored target：

```sh
export CARGO_TARGET_DIR="$PWD/target/legacy-oracle"
python3 tests/legacy-oracle/check_isolation.py
python3 tests/legacy-oracle/test_isolation.py
python3 tests/legacy-oracle/verify_sources.py
python3 tests/legacy-oracle/verify_catalog.py
cargo run --offline --locked --manifest-path tests/legacy-oracle/Cargo.toml -- verify
cargo test --offline --locked --manifest-path tests/legacy-oracle/Cargo.toml
cargo test --offline --locked --release --manifest-path tests/legacy-oracle/Cargo.toml
cargo test --offline --locked --release --manifest-path tests/legacy-oracle/Cargo.toml --test benchmark -- --ignored --nocapture
```

锁初次建立用 `cargo generate-lockfile --offline --manifest-path tests/legacy-oracle/Cargo.toml`；日常不重建锁。显式 `generate` 模式重新生成叶子 snapshot、wire catalog、composition；已有 planar64 永远重算比较，不覆盖原值。`verify` 同样执行真实原版 API 再逐字节比较，不能仅重新算文件自身 hash 来冒充 oracle 验证。压缩固定 gzip 元数据，原版运行不从生产副本读取 decoder 源码。第三方六组 bin/BMP 从原版 archive 原样保存于 `fixtures/upstream`（本目录附原 MIT/Apache 许可，源hash见固定 graphics 副本），其 expected 是原版 public decoder 输出。

`original-leaves.bin.gz` 解压为 `FRDORC01` 加连续 records：32byte SHA256 key、LEu32 output length、完整 output bytes。key = SHA256(op:u8 + 每段 LEu64 length + bytes)，op1 dequant(input LEi16,quant10bytes)、op2 DWT(input LEi16)、op3 prefix(input LEi16)、op4 color(y,cb,cr分别LEi16)。BTreeMap 按key排序，保留25962个唯一原始输入；重复输入共用记录。对应kernel测试仍逐一执行原循环，包含4096位置脉冲、12随机DWT、5常量极值、170量化、1104 prefix tails/offsets+4极值、100000随机色彩域判定和1096色彩tail/offset cases。非法色彩域的 native 原子错误测试独立保留；上游 debug panic/release wrap 单独运行 `tests/overflow.rs`。

`original-composition.bin` = `FRDCMP01` + 三个固定plane records，每个quant10byte、RLGR系数4096×LEi16、原版LL3→quant→DWT输出4096×LEi16。对应原始2970byte session wire，原始16384byte BGRX图像未变。kernel使用捕获系数做叶子组合，不运行原版或patched parser；原版session成功、截断拒绝、RLGR original vectors/modes/padding的真实计算保存在此workspace。

`fixtures/wire-catalog.json.gz` 每条记录保存 codec、命名case、geometry/bpp、原始input和initial dst及各自SHA256、原版result的完整Debug variant/fields、output或错误后的partial bytes及SHA256。`consumed_bytes=null` 与private_scratch说明是公开API不可观测，不是0或猜测消费。覆盖所有Bitmap20 code families×4depth、直接/扩展/mega、首行和后续行、跨行与连续BG/持久FG、全部mask×tail×first/next、错误与优先序/nonempty dst；planar raw/RLE×CLL0..7×alpha×CS×1/3宽高、每字节truncate、padding/extra、16/32 repeat controls、混合literal/repeat与row reset、原六组fixtures；RLGR1/3、输出0/1/4095/4096/4097、zero runs与逐byte/关键bit（byte中低位补零明确是实际可传public API输入）截断、原始tile/session。bounded planar输入用catch_unwind显式分类潜在上游panic，没有进程外指针或超大尺寸；当前fixture结果另见manifest。没有声称覆盖不可达错误或public API未暴露的私有scratch。

基准的每个case维持128warmup、501×16 batch、thin LTO、同一wire与composition hash，预分配输出。原版完整component/tile仍含RLGR parser/materialization；新增 `*_original_coefficients_*` 与kernel `*_native_coefficients_*` 都以同一系数copy起点计时。只能比较相同边界；IPC/加载/解压/编译不计时，禁止把旧混合路径数字当新全native decoder数据。完整native wire计时要等真正parser接线。

这些是原版生成、数值与依赖隔离证据，不是消费者接线、生产能力、OHOS/Windows/Linux目标执行或远程互操作证明。

`original-rlgr-replay.bin.gz` = `FRDRLG01` + records（原wire catalog索引LEu32、output byte length LEu32、完整LEi16 bytes），包含catalog全部546个成功RLGR输出。kernel保留旧截断/modes/terminal padding输出上的zero区间重放，并扩展到整个catalog；该重放明确不是parser hook。生成与verify由同一独立runner执行。
