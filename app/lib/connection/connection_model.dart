enum ServiceKind { automatic, windows, macOs, linuxVnc }

extension ServiceKindPresentation on ServiceKind {
  String get label => switch (this) {
    ServiceKind.automatic => '自动识别',
    ServiceKind.windows => 'Windows',
    ServiceKind.macOs => 'Mac OS',
    ServiceKind.linuxVnc => 'Linux / VNC',
  };

  int get defaultPort => switch (this) {
    ServiceKind.windows => 3389,
    ServiceKind.automatic || ServiceKind.macOs || ServiceKind.linuxVnc => 5900,
  };
}

class ConnectionDraft {
  const ConnectionDraft({
    required this.service,
    required this.host,
    required this.port,
    required this.username,
    required this.password,
    this.domain,
  });

  final ServiceKind service;
  final String host;
  final int port;
  final String username;
  final String password;
  final String? domain;
}
