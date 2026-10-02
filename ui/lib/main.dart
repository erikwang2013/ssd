import 'package:flutter/material.dart';

import 'core_client/core_client.dart';
import 'core_client/ipc_transport.dart';
import 'core_client/protocol.dart';
import 'home_page.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // M0：真实 daemon 通过 XD_DAEMON_BIN 指定；无 daemon 时 UI 显示错误态。
  CoreClient client;
  try {
    client = await IpcCoreClient.start();
  } on StateError {
    client = _MissingDaemonClient();
  }
  runApp(XiaodunApp(client: client));
}

class XiaodunApp extends StatelessWidget {
  const XiaodunApp({super.key, required this.client});

  final CoreClient client;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: '小盾',
      theme: ThemeData(
        colorSchemeSeed: const Color(0xFF2E6BE6),
        useMaterial3: true,
      ),
      home: HomePage(client: client),
    );
  }
}

class _MissingDaemonClient implements CoreClient {
  @override
  Future<PingResult> ping() async =>
      throw StateError('未找到 daemon：请设置 XD_DAEMON_BIN');

  @override
  Future<List<DeviceInfo>> listDevices() async =>
      throw StateError('未找到 daemon：请设置 XD_DAEMON_BIN');

  @override
  Future<void> close() async {}
}
