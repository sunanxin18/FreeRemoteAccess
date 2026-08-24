import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:freeremote_access/connection/connection_model.dart';
import 'package:freeremote_access/bridge/connection_bridge.dart';
import 'package:freeremote_access/main.dart';

class _RecordingBridge implements ConnectionBridge {
  ConnectionDraft? lastRequest;

  @override
  ConnectionValidation validate(ConnectionDraft request) {
    lastRequest = request;
    return const ConnectionValidation(
      protocol: ConnectionProtocol.appleRfb,
      port: 5900,
    );
  }
}

void main() {
  test('service labels use the approved product names', () {
    expect(ServiceKind.values.map((service) => service.label), <String>[
      '自动识别',
      'Windows',
      'Mac OS',
      'Linux / VNC',
    ]);
  });

  testWidgets('selector exposes the approved Mac OS option', (tester) async {
    await tester.pumpWidget(FreeRemoteAccessApp());

    await tester.tap(find.byKey(const Key('service-selector')));
    await tester.pumpAndSettle();

    expect(find.text('Mac OS'), findsOneWidget);
  });

  testWidgets('domain field is visible only for Windows', (tester) async {
    await tester.pumpWidget(FreeRemoteAccessApp());

    expect(find.byKey(const Key('domain-field')), findsNothing);

    await tester.tap(find.byKey(const Key('service-selector')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Windows').last);
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('domain-field')), findsOneWidget);

    await tester.tap(find.byKey(const Key('service-selector')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Mac OS').last);
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('domain-field')), findsNothing);
  });

  testWidgets('service selection updates the default port', (tester) async {
    await tester.pumpWidget(FreeRemoteAccessApp());

    TextFormField portField() =>
        tester.widget(find.byKey(const Key('port-field')));

    expect(portField().controller!.text, '5900');

    await tester.tap(find.byKey(const Key('service-selector')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Windows').last);
    await tester.pumpAndSettle();

    expect(portField().controller!.text, '3389');
  });

  testWidgets('password field is obscured', (tester) async {
    await tester.pumpWidget(FreeRemoteAccessApp());

    final password = tester.widget<EditableText>(
      find.descendant(
        of: find.byKey(const Key('password-field')),
        matching: find.byType(EditableText),
      ),
    );

    expect(password.obscureText, isTrue);
  });

  testWidgets('desktop width shows the recent connections rail', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1200, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    await tester.pumpWidget(FreeRemoteAccessApp());

    expect(find.byKey(const Key('desktop-connections-layout')), findsOneWidget);
    expect(find.byKey(const Key('recent-connections-rail')), findsOneWidget);
  });

  testWidgets('Mac OS submission crosses the bridge as Apple RFB', (
    tester,
  ) async {
    final bridge = _RecordingBridge();
    await tester.pumpWidget(FreeRemoteAccessApp(bridge: bridge));

    await tester.tap(find.byKey(const Key('service-selector')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Mac OS').last);
    await tester.enterText(find.byKey(const Key('host-field')), 'mac.example');
    await tester.enterText(
      find.byKey(const Key('username-field')),
      'local-user',
    );
    await tester.enterText(
      find.byKey(const Key('password-field')),
      'one-session-secret',
    );
    await tester.tap(find.byKey(const Key('connect-button')));
    await tester.pump();

    expect(bridge.lastRequest?.service, ServiceKind.macOs);
    expect(bridge.lastRequest?.port, 5900);
    expect(find.text('已选择 Apple RFB，会话端口 5900'), findsOneWidget);
    expect(
      find.descendant(
        of: find.byType(SnackBar),
        matching: find.textContaining('one-session-secret'),
      ),
      findsNothing,
    );
  });
}
