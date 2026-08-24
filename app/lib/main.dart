import 'package:flutter/material.dart';

import 'connection/connection_form.dart';
import 'connection/connection_model.dart';

void main() {
  runApp(const FreeRemoteAccessApp());
}

class FreeRemoteAccessApp extends StatelessWidget {
  const FreeRemoteAccessApp({super.key});

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
      home: const ConnectionHomePage(),
    );
  }
}

class ConnectionHomePage extends StatelessWidget {
  const ConnectionHomePage({super.key});

  void _showPendingBridge(BuildContext context, ConnectionDraft draft) {
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(const SnackBar(content: Text('连接参数已验证，正在初始化协议会话')));
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('FreeRemoteAccess'), centerTitle: false),
      body: SafeArea(
        child: LayoutBuilder(
          builder: (context, constraints) {
            final form = ConnectionForm(
              onConnect: (draft) => _showPendingBridge(context, draft),
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
