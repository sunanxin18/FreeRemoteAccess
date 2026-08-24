import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:freeremote_access/bridge/connection_bridge.dart';
import 'package:freeremote_access/connection/connection_model.dart';
import 'package:freeremote_access/main.dart';
import 'package:freeremote_access/session/desktop_session_launcher.dart';

class _AppleValidationBridge implements ConnectionBridge {
  @override
  ConnectionValidation validate(ConnectionDraft request) {
    return const ConnectionValidation(
      protocol: ConnectionProtocol.appleRfb,
      port: 5900,
    );
  }
}

class _PendingLauncher implements DesktopSessionLauncher {
  final Completer<DesktopSessionLaunch> completion =
      Completer<DesktopSessionLaunch>();

  @override
  Future<DesktopSessionLaunch> launch(ConnectionDraft request) {
    return completion.future;
  }
}

void main() {
  testWidgets('Mac OS connect shows progress then a started desktop session', (
    tester,
  ) async {
    final launcher = _PendingLauncher();
    await tester.pumpWidget(
      FreeRemoteAccessApp(
        bridge: _AppleValidationBridge(),
        sessionLauncher: launcher,
      ),
    );

    await tester.tap(find.byKey(const Key('service-selector')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Mac OS').last);
    await tester.enterText(find.byKey(const Key('host-field')), 'mac.example');
    await tester.enterText(
      find.byKey(const Key('username-field')),
      'local-user',
    );
    await tester.enterText(find.byKey(const Key('password-field')), 'secret');
    await tester.tap(find.byKey(const Key('connect-button')));
    await tester.pump();

    expect(find.text('正在建立 Apple 高性能屏幕共享…'), findsOneWidget);

    launcher.completion.complete(
      const DesktopSessionLaunch(processId: 42, width: 1920, height: 1080),
    );
    await tester.pumpAndSettle();

    expect(find.text('远程会话已启动（1920×1080）'), findsOneWidget);
  });
}
