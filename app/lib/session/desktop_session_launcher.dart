import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import '../connection/connection_model.dart';

class DesktopSessionLaunch {
  const DesktopSessionLaunch({
    required this.processId,
    required this.width,
    required this.height,
  });

  final int processId;
  final int width;
  final int height;
}

class DesktopSessionLaunchException implements Exception {
  const DesktopSessionLaunchException(this.message);

  final String message;

  @override
  String toString() => message;
}

abstract interface class DesktopSessionLauncher {
  Future<DesktopSessionLaunch> launch(ConnectionDraft request);
}

const desktopSessionArguments = <String>[
  'hpssview',
  '--credentials-stdin-v1',
  '--parent-status-stdout-v1',
  '--scale',
  '0.25',
];

bool isAppleDesktopRequest(ConnectionDraft request) {
  return request.service == ServiceKind.macOs ||
      (request.service == ServiceKind.automatic && request.port == 5900);
}

class ProcessDesktopSessionLauncher implements DesktopSessionLauncher {
  const ProcessDesktopSessionLauncher({
    this.connectionTimeout = const Duration(seconds: 20),
  });

  final Duration connectionTimeout;

  @override
  Future<DesktopSessionLaunch> launch(ConnectionDraft request) async {
    if (!isAppleDesktopRequest(request)) {
      throw const DesktopSessionLaunchException('当前构建尚未接入所选服务的会话后端');
    }
    if (!Platform.isWindows) {
      throw const DesktopSessionLaunchException(
        '当前 Apple 高性能桌面会话仅先开放 Windows 客户端验证',
      );
    }

    final executable = File(
      '${File(Platform.resolvedExecutable).parent.path}'
      '${Platform.pathSeparator}freeremotedesk.exe',
    );
    if (!executable.existsSync()) {
      throw const DesktopSessionLaunchException('Windows 发行包缺少 Rust 远程会话后端');
    }

    final process = await Process.start(
      executable.path,
      desktopSessionArguments,
      workingDirectory: executable.parent.path,
    );

    final frame = encodeCredentialFrame(request);
    try {
      process.stdin.add(frame);
      await process.stdin.flush();
      await process.stdin.close();
    } finally {
      frame.fillRange(0, frame.length, 0);
    }

    final result = Completer<DesktopSessionLaunch>();
    var errorSummary = '';
    late final StreamSubscription<String> stdoutSubscription;
    late final StreamSubscription<String> stderrSubscription;
    final timer = Timer(connectionTimeout, () {
      if (result.isCompleted) return;
      process.kill();
      result.completeError(
        const DesktopSessionLaunchException('连接 Mac 超时，远程会话未建立'),
      );
    });

    stdoutSubscription = process.stdout
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen((line) {
          final fields = line.split(' ');
          if (fields.length != 4 ||
              fields[0] != 'FRDSESSION1' ||
              fields[1] != 'CONNECTED') {
            return;
          }
          final width = int.tryParse(fields[2]);
          final height = int.tryParse(fields[3]);
          if (width == null || height == null || width <= 0 || height <= 0) {
            return;
          }
          if (!result.isCompleted) {
            timer.cancel();
            result.complete(
              DesktopSessionLaunch(
                processId: process.pid,
                width: width,
                height: height,
              ),
            );
          }
        });
    stderrSubscription = process.stderr
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen((line) {
          if (line.trim().isNotEmpty) errorSummary = line.trim();
        });
    unawaited(
      process.exitCode.then((exitCode) async {
        timer.cancel();
        await stdoutSubscription.cancel();
        await stderrSubscription.cancel();
        if (!result.isCompleted) {
          result.completeError(
            DesktopSessionLaunchException(
              errorSummary.isEmpty ? '远程会话启动失败（退出码 $exitCode）' : errorSummary,
            ),
          );
        }
      }),
    );

    return result.future;
  }
}

Uint8List encodeCredentialFrame(ConnectionDraft request) {
  final host = utf8.encode(request.host);
  final username = utf8.encode(request.username);
  final password = utf8.encode(request.password);
  for (final field in <(String, List<int>, int)>[
    ('服务器地址', host, 255),
    ('用户名', username, 255),
    ('密码', password, 1024),
  ]) {
    if (field.$2.isEmpty || field.$2.length > field.$3) {
      throw DesktopSessionLaunchException('${field.$1}长度超出安全凭据帧限制');
    }
  }

  final payloadLength = 8 + host.length + username.length + password.length;
  final frame = Uint8List(12 + payloadLength);
  frame.setRange(0, 8, ascii.encode('FRDSTD01'));
  final data = ByteData.sublistView(frame);
  data.setUint32(8, payloadLength, Endian.big);
  data.setUint16(12, host.length, Endian.big);
  data.setUint16(14, username.length, Endian.big);
  data.setUint16(16, password.length, Endian.big);
  data.setUint16(18, request.port, Endian.big);
  var offset = 20;
  for (final field in <List<int>>[host, username, password]) {
    frame.setRange(offset, offset + field.length, field);
    offset += field.length;
  }
  return frame;
}
