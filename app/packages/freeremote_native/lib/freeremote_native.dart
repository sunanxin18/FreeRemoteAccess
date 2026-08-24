import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

const _abiVersion = 1;
const _libraryName = 'freeremote_native';

final class _FrdValidationOutput extends Struct {
  @Uint32()
  external int abiVersion;

  @Uint32()
  external int status;

  @Uint8()
  external int protocol;

  @Uint8()
  external int reserved;

  @Uint16()
  external int port;
}

typedef _ValidateNative =
    Uint32 Function(
      Uint8 service,
      Pointer<Utf8> host,
      Uint16 port,
      Pointer<Utf8> username,
      Pointer<Utf8> password,
      Pointer<Utf8> domain,
      Pointer<_FrdValidationOutput> output,
    );
typedef _ValidateDart =
    int Function(
      int service,
      Pointer<Utf8> host,
      int port,
      Pointer<Utf8> username,
      Pointer<Utf8> password,
      Pointer<Utf8> domain,
      Pointer<_FrdValidationOutput> output,
    );

class NativeValidationResult {
  const NativeValidationResult({required this.protocol, required this.port});

  final int protocol;
  final int port;
}

class FreeremoteNativeException implements Exception {
  const FreeremoteNativeException(this.status);

  final int status;

  @override
  String toString() => 'FreeRemoteAccess native status $status';
}

class FreeremoteNative {
  FreeremoteNative({DynamicLibrary? library})
    : _validate = (library ?? _openLibrary())
          .lookupFunction<_ValidateNative, _ValidateDart>(
            'frd_validate_connection',
          );

  final _ValidateDart _validate;

  NativeValidationResult validateConnection({
    required int service,
    required String host,
    required int port,
    required String username,
    required String password,
    String? domain,
  }) {
    if (service < 0 || service > 255) {
      throw ArgumentError.value(service, 'service');
    }
    if (port < 0 || port > 65535) {
      throw ArgumentError.value(port, 'port');
    }

    final hostPointer = host.toNativeUtf8();
    final usernamePointer = username.toNativeUtf8();
    final passwordPointer = password.toNativeUtf8();
    final domainPointer = domain?.toNativeUtf8();
    final output = calloc<_FrdValidationOutput>();
    try {
      final status = _validate(
        service,
        hostPointer,
        port,
        usernamePointer,
        passwordPointer,
        domainPointer ?? nullptr,
        output,
      );
      if (status != 0) {
        throw FreeremoteNativeException(status);
      }
      if (output.ref.abiVersion != _abiVersion) {
        throw const FreeremoteNativeException(255);
      }
      return NativeValidationResult(
        protocol: output.ref.protocol,
        port: output.ref.port,
      );
    } finally {
      calloc.free(hostPointer);
      calloc.free(usernamePointer);
      calloc.free(passwordPointer);
      if (domainPointer != null) calloc.free(domainPointer);
      calloc.free(output);
    }
  }

  static DynamicLibrary _openLibrary() {
    if (Platform.isIOS) return DynamicLibrary.process();
    if (Platform.isMacOS) {
      return DynamicLibrary.open('$_libraryName.framework/$_libraryName');
    }
    if (Platform.isAndroid || Platform.isLinux) {
      return DynamicLibrary.open('lib$_libraryName.so');
    }
    if (Platform.isWindows) {
      return DynamicLibrary.open('$_libraryName.dll');
    }
    throw UnsupportedError('不支持的平台：${Platform.operatingSystem}');
  }
}
