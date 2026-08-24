import '../connection/connection_model.dart';

enum ConnectionProtocol { automatic, rdp, appleRfb, standardRfb }

extension ConnectionProtocolPresentation on ConnectionProtocol {
  String get label => switch (this) {
    ConnectionProtocol.automatic => '自动识别',
    ConnectionProtocol.rdp => 'RDP',
    ConnectionProtocol.appleRfb => 'Apple RFB',
    ConnectionProtocol.standardRfb => '标准 RFB',
  };
}

class ConnectionValidation {
  const ConnectionValidation({required this.protocol, required this.port});

  final ConnectionProtocol protocol;
  final int port;
}

abstract interface class ConnectionBridge {
  ConnectionValidation validate(ConnectionDraft request);
}

class ConnectionBridgeException implements Exception {
  const ConnectionBridgeException(this.message);

  final String message;

  @override
  String toString() => message;
}
