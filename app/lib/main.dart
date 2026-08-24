import 'package:flutter/material.dart';

import 'bridge/connection_bridge.dart';
import 'bridge/native_connection_bridge.dart';
import 'connection/connection_form.dart';
import 'connection/connection_model.dart';
import 'session/desktop_session_launcher.dart';

void main() {
  runApp(FreeRemoteAccessApp());
}

class FreeRemoteAccessApp extends StatelessWidget {
  const FreeRemoteAccessApp({super.key, this.bridge, this.sessionLauncher});

  final ConnectionBridge? bridge;
  final DesktopSessionLauncher? sessionLauncher;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'FreeRemoteAccess',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: const Color(0xff2563eb),
          brightness: Brightness.light,
        ),
        scaffoldBackgroundColor: const Color(0xfff4f6fa),
        inputDecorationTheme: const InputDecorationTheme(
          border: OutlineInputBorder(),
          filled: true,
          fillColor: Colors.white,
        ),
        useMaterial3: true,
      ),
      home: ConnectionHomePage(
        bridge: bridge,
        sessionLauncher: sessionLauncher,
      ),
    );
  }
}

class ConnectionHomePage extends StatefulWidget {
  const ConnectionHomePage({super.key, this.bridge, this.sessionLauncher});

  final ConnectionBridge? bridge;
  final DesktopSessionLauncher? sessionLauncher;

  @override
  State<ConnectionHomePage> createState() => _ConnectionHomePageState();
}

class _ConnectionHomePageState extends State<ConnectionHomePage> {
  bool _connecting = false;
  String? _sessionStatus;

  Future<void> _connect(ConnectionDraft draft) async {
    if (_connecting) return;
    try {
      final validation = (widget.bridge ?? NativeConnectionBridge()).validate(
        draft,
      );
      if (validation.protocol != ConnectionProtocol.appleRfb) {
        throw const DesktopSessionLaunchException('当前构建优先完成 Mac OS 原生屏幕共享');
      }
      setState(() {
        _connecting = true;
        _sessionStatus = '正在建立 Apple 高性能屏幕共享…';
      });
      final result =
          await (widget.sessionLauncher ??
                  const ProcessDesktopSessionLauncher())
              .launch(draft);
      if (!mounted) return;
      setState(() {
        _connecting = false;
        _sessionStatus = '远程会话已启动（${result.width}×${result.height}）';
      });
    } on ConnectionBridgeException catch (error) {
      if (!mounted) return;
      setState(() {
        _connecting = false;
        _sessionStatus = error.message;
      });
    } on DesktopSessionLaunchException catch (error) {
      if (!mounted) return;
      setState(() {
        _connecting = false;
        _sessionStatus = error.message;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('FreeRemoteAccess'), centerTitle: false),
      body: SafeArea(
        child: LayoutBuilder(
          builder: (context, constraints) {
            final form = ConnectionForm(
              connecting: _connecting,
              onConnect: _connect,
            );
            if (constraints.maxWidth >= 720) {
              return Row(
                key: const Key('desktop-connections-layout'),
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  const SizedBox(width: 280, child: RecentConnectionsRail()),
                  Expanded(
                    child: Center(
                      child: SingleChildScrollView(
                        padding: const EdgeInsets.all(32),
                        child: form,
                      ),
                    ),
                  ),
                ],
              );
            }
            return ListView(
              key: const Key('mobile-connections-layout'),
              padding: const EdgeInsets.all(16),
              children: [
                form,
                const SizedBox(height: 20),
                const RecentConnectionsRail(compact: true),
              ],
            );
          },
        ),
      ),
      bottomNavigationBar: _sessionStatus == null
          ? null
          : BottomAppBar(
              child: Text(_sessionStatus!, key: const Key('session-status')),
            ),
    );
  }
}

class RecentConnectionsRail extends StatelessWidget {
  const RecentConnectionsRail({super.key, this.compact = false});

  final bool compact;

  @override
  Widget build(BuildContext context) {
    return Material(
      key: const Key('recent-connections-rail'),
      color: compact ? Colors.transparent : const Color(0xffe9eef8),
      child: Padding(
        padding: EdgeInsets.all(compact ? 8 : 24),
        child: Column(
          mainAxisSize: compact ? MainAxisSize.min : MainAxisSize.max,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('最近连接', style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 12),
            const Text('还没有最近连接'),
          ],
        ),
      ),
    );
  }
}
