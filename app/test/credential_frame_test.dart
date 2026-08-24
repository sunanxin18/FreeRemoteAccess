import 'package:flutter_test/flutter_test.dart';
import 'package:freeremote_access/connection/connection_model.dart';
import 'package:freeremote_access/session/desktop_session_launcher.dart';

void main() {
  test('Windows Apple desktop launch uses a visible initial portrait size', () {
    expect(desktopSessionArguments, <String>[
      'hpssview',
      '--credentials-stdin-v1',
      '--parent-status-stdout-v1',
      '--scale',
      '0.25',
    ]);
    expect(desktopSessionArguments, isNot(contains('--udp-media')));
  });

  test('automatic port 5900 is eligible for the Apple desktop launcher', () {
    expect(
      isAppleDesktopRequest(
        const ConnectionDraft(
          service: ServiceKind.automatic,
          host: 'host',
          port: 5900,
          username: 'u',
          password: 'p',
        ),
      ),
      isTrue,
    );
  });

  test('credential frame matches the FRDSTD01 big-endian wire format', () {
    final frame = encodeCredentialFrame(
      const ConnectionDraft(
        service: ServiceKind.macOs,
        host: 'host',
        port: 5900,
        username: 'u',
        password: 'p',
      ),
    );

    expect(frame, <int>[
      0x46,
      0x52,
      0x44,
      0x53,
      0x54,
      0x44,
      0x30,
      0x31,
      0x00,
      0x00,
      0x00,
      0x0e,
      0x00,
      0x04,
      0x00,
      0x01,
      0x00,
      0x01,
      0x17,
      0x0c,
      0x68,
      0x6f,
      0x73,
      0x74,
      0x75,
      0x70,
    ]);
  });
}
