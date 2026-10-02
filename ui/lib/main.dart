// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:io' show Platform, ProcessException;

import 'package:flutter/material.dart';

import 'core_client/core_client.dart';
import 'core_client/ipc_transport.dart';
import 'core_client/protocol.dart';
import 'home_page.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // M0：真实 daemon 通过 XD_DAEMON_BIN 指定；未配置或启动失败时 UI 显示错误态。
  // XD_IMAGE 可选：把镜像文件注册为设备（演示/测试路径，见 README 快速上手）。
  final extraArgs = <String>[];
  final image = Platform.environment['XD_IMAGE'];
  if (image != null) {
    extraArgs.addAll(['--image', image]);
  }
  CoreClient client;
  try {
    client = await IpcCoreClient.start(extraArgs: extraArgs);
  } on StateError catch (e) {
    client = _MissingDaemonClient('$e');
  } on ProcessException catch (e) {
    client = _MissingDaemonClient('daemon 启动失败：${e.message}');
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
  _MissingDaemonClient(this.reason);

  final String reason;

  @override
  Future<PingResult> ping() async => throw StateError(reason);

  @override
  Future<List<DeviceInfo>> listDevices() async => throw StateError(reason);

  @override
  Future<void> close() async {}
}
