import 'package:freeremote_native/freeremote_native.dart';

import '../connection/connection_model.dart';
import 'connection_bridge.dart';

class NativeConnectionBridge implements ConnectionBridge {
  NativeConnectionBridge({FreeremoteNative? native})
    : _native = native ?? FreeremoteNative();

  final FreeremoteNative _native;

  @override
  ConnectionValidation validate(ConnectionDraft request) {
    try {
      final result = _native.validateConnection(
        service: request.service.index,
        host: request.host,
        port: request.port,
        username: request.username,
        password: request.password,
        domain: request.domain,
      );
      return ConnectionValidation(
        protocol: ConnectionProtocol.values[result.protocol],
        port: result.port,
      );
    } on FreeremoteNativeException catch (error) {
      throw ConnectionBridgeException(_messageForStatus(error.status));
    } on ArgumentError catch (error) {
      throw ConnectionBridgeException(error.message?.toString() ?? '连接参数无效');
    }
  }

  String _messageForStatus(int status) => switch (status) {
    1 => '本机协议模块参数错误',
    2 => '连接参数不是有效文本',
    100 => '不支持的服务类型',
    101 => '请输入服务器地址',
    102 => '端口必须在 1 到 65535 之间',
    103 => '请输入用户名',
    104 => '请输入密码',
    105 => '只有 Windows RDP 支持域',
    _ => '本机协议模块初始化失败',
  };
}
