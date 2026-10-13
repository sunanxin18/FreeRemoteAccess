//! ABI 证明位于 private unit module `native::pixel::abi`，直接调用真实 selected core/copy。
//! 原 outer callback 检查可被 Rust callback prologue/epilogue 遮蔽，已撤销其证明声明。
//! 执行：cargo test -p frd-rdp-legacy-kernels --lib native::pixel::abi --offline --locked
//! negative controls、全部176原版records和bulk/tail覆盖在同一个unit test中串行执行。
