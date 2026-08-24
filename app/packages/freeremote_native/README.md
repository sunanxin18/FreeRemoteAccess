# freeremote_native

FreeRemoteAccess 的内部 Rust FFI 桥接插件。应用层只依赖版本化 C ABI；平台
目录负责把同一个 Rust 核心构建为 Windows、macOS、Linux、Android 和 iOS
可加载的本机库。后续 OpenHarmony 适配复用相同 ABI。

当前 ABI 版本为 1，公开头文件位于
`native/freeremote_ffi/include/freeremote_native.h`。连接密码仅在调用期间
传入 Rust；返回结构不包含凭证或由本机侧分配的指针。

验证命令：

```text
cargo test -p freeremote_ffi
cd app/packages/freeremote_native
FRD_NATIVE_LIBRARY=<native-library-path> flutter test test/native_abi_test.dart
```
