import 'dart:ffi';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:freeremote_native/freeremote_native.dart';

void main() {
  final libraryPath = Platform.environment['FRD_NATIVE_LIBRARY'];

  test(
    'Rust ABI validates a Mac OS password session as Apple RFB',
    () {
      final bridge = FreeremoteNative(
        library: DynamicLibrary.open(libraryPath!),
      );

      final result = bridge.validateConnection(
        service: 2,
        host: 'mac.example',
        port: 0,
        username: 'local-user',
        password: 'one-session-secret',
      );

      expect(result.protocol, 2);
      expect(result.port, 5900);
    },
    skip: libraryPath == null
        ? '设置 FRD_NATIVE_LIBRARY 后执行真实 Rust 动态库验证'
        : false,
  );
}
