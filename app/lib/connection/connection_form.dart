import 'package:flutter/material.dart';

import 'connection_model.dart';

class ConnectionForm extends StatefulWidget {
  const ConnectionForm({
    super.key,
    required this.onConnect,
    this.connecting = false,
  });

  final ValueChanged<ConnectionDraft> onConnect;
  final bool connecting;

  @override
  State<ConnectionForm> createState() => _ConnectionFormState();
}

class _ConnectionFormState extends State<ConnectionForm> {
  final _formKey = GlobalKey<FormState>();
  final _hostController = TextEditingController();
  final _portController = TextEditingController(text: '5900');
  final _usernameController = TextEditingController();
  final _passwordController = TextEditingController();
  final _domainController = TextEditingController();
  ServiceKind _service = ServiceKind.automatic;
  bool _showPassword = false;

  @override
  void dispose() {
    _hostController.dispose();
    _portController.dispose();
    _usernameController.dispose();
    _passwordController.clear();
    _passwordController.dispose();
    _domainController.dispose();
    super.dispose();
  }

  void _selectService(ServiceKind? service) {
    if (service == null) return;
    setState(() {
      _service = service;
      _portController.text = service.defaultPort.toString();
      if (service != ServiceKind.windows) {
        _domainController.clear();
      }
    });
  }

  String? _required(String? value, String message) {
    if (value == null || value.trim().isEmpty) return message;
    return null;
  }

  void _connect() {
    if (!_formKey.currentState!.validate()) return;
    widget.onConnect(
      ConnectionDraft(
        service: _service,
        host: _hostController.text.trim(),
        port: int.parse(_portController.text),
        username: _usernameController.text.trim(),
        password: _passwordController.text,
        domain:
            _service == ServiceKind.windows &&
                _domainController.text.trim().isNotEmpty
            ? _domainController.text.trim()
            : null,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 560),
      child: Card(
        elevation: 1,
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Form(
            key: _formKey,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(
                  '连接远程计算机',
                  style: Theme.of(context).textTheme.headlineSmall,
                ),
                const SizedBox(height: 8),
                Text(
                  '直接连接系统原生远程桌面服务',
                  style: Theme.of(context).textTheme.bodyMedium,
                ),
                const SizedBox(height: 24),
                DropdownButtonFormField<ServiceKind>(
                  key: const Key('service-selector'),
                  initialValue: _service,
                  decoration: const InputDecoration(labelText: '服务类型'),
                  items: [
                    for (final service in ServiceKind.values)
                      DropdownMenuItem(
                        value: service,
                        child: Text(service.label),
                      ),
                  ],
                  onChanged: _selectService,
                ),
                const SizedBox(height: 16),
                TextFormField(
                  key: const Key('host-field'),
                  controller: _hostController,
                  decoration: const InputDecoration(
                    labelText: '服务器地址',
                    hintText: '主机名或 IP 地址',
                  ),
                  textInputAction: TextInputAction.next,
                  validator: (value) => _required(value, '请输入服务器地址'),
                ),
                const SizedBox(height: 16),
                TextFormField(
                  key: const Key('port-field'),
                  controller: _portController,
                  decoration: const InputDecoration(labelText: '端口'),
                  keyboardType: TextInputType.number,
                  textInputAction: TextInputAction.next,
                  validator: (value) {
                    final port = int.tryParse(value ?? '');
                    if (port == null || port < 1 || port > 65535) {
                      return '端口必须在 1 到 65535 之间';
                    }
                    return null;
                  },
                ),
                const SizedBox(height: 16),
                TextFormField(
                  key: const Key('username-field'),
                  controller: _usernameController,
                  decoration: const InputDecoration(labelText: '用户名'),
                  textInputAction: TextInputAction.next,
                  autofillHints: const [AutofillHints.username],
                  validator: (value) => _required(value, '请输入用户名'),
                ),
                if (_service == ServiceKind.windows) ...[
                  const SizedBox(height: 16),
                  TextFormField(
                    key: const Key('domain-field'),
                    controller: _domainController,
                    decoration: const InputDecoration(labelText: '域（可选）'),
                    textInputAction: TextInputAction.next,
                  ),
                ],
                const SizedBox(height: 16),
                TextFormField(
                  key: const Key('password-field'),
                  controller: _passwordController,
                  decoration: InputDecoration(
                    labelText: '密码',
                    suffixIcon: IconButton(
                      tooltip: _showPassword ? '隐藏密码' : '显示密码',
                      onPressed: () =>
                          setState(() => _showPassword = !_showPassword),
                      icon: Icon(
                        _showPassword ? Icons.visibility_off : Icons.visibility,
                      ),
                    ),
                  ),
                  obscureText: !_showPassword,
                  enableSuggestions: false,
                  autocorrect: false,
                  autofillHints: const [AutofillHints.password],
                  textInputAction: TextInputAction.done,
                  onFieldSubmitted: (_) => _connect(),
                  validator: (value) => _required(value, '请输入密码'),
                ),
                const SizedBox(height: 24),
                FilledButton.icon(
                  key: const Key('connect-button'),
                  onPressed: widget.connecting ? null : _connect,
                  icon: const Icon(Icons.login),
                  label: Text(widget.connecting ? '连接中…' : '连接'),
                ),
                const SizedBox(height: 12),
                const Text('密码仅用于本次连接，不会保存。', textAlign: TextAlign.center),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
