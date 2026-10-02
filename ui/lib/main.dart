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

  Never _missing() => throw StateError(reason);

  @override
  Future<PingResult> ping() async => _missing();

  @override
  Future<List<DeviceInfo>> listDevices() async => _missing();

  @override
  Future<ScanStartResult> scanStart(
    String device, {
    String mode = 'quick',
  }) async => _missing();

  @override
  Future<ScanStatusResult> scanStatus(int taskId) async => _missing();

  @override
  Future<ScanResultsPage> scanResults(
    int taskId, {
    int offset = 0,
    int limit = 200,
    bool deletedOnly = false,
  }) async => _missing();

  @override
  Future<void> scanPause(int taskId) async => _missing();

  @override
  Future<void> scanResume(int taskId) async => _missing();

  @override
  Future<void> scanCancel(int taskId) async => _missing();

  @override
  Future<FsReadResult> fsRead(
    int taskId,
    int idx, {
    int offset = 0,
    int length = 1048576,
  }) async => _missing();

  @override
  Future<ExportStartResult> exportStart(
    int taskId,
    List<int> idxs,
    String targetDir,
  ) async => _missing();

  @override
  Future<void> exportCancel(int exportId) async => _missing();

  @override
  Stream<Map<String, dynamic>> get notifications => const Stream.empty();

  @override
  Future<void> close() async {}
}
