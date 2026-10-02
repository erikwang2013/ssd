// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'core_client.dart';
import 'protocol.dart';

/// 桌面实现：spawn 特权 daemon，stdio 上每行一条 JSON-RPC 消息。
class IpcCoreClient implements CoreClient {
  IpcCoreClient._(
    this._process, [
    this._daemonPath,
    this._extraArgs = const [],
  ]) {
    _sub = _process.stdout
        .transform(const Utf8Decoder(allowMalformed: true))
        .transform(const LineSplitter())
        .listen(_onLine);
    _process.stderr
        .transform(const Utf8Decoder(allowMalformed: true))
        .transform(const LineSplitter())
        .listen((line) => stderrLines.add(line));
    _process.exitCode.then(_onExit);
  }

  /// 启动 daemon。[daemonPath] 缺省取环境变量 XD_DAEMON_BIN。
  /// [environment] 追加到子进程环境（测试用；root 宿主注入 PKEXEC_UID）。
  static Future<IpcCoreClient> start({
    String? daemonPath,
    List<String> extraArgs = const [],
    Map<String, String>? environment,
  }) async {
    final path = daemonPath ?? Platform.environment['XD_DAEMON_BIN'];
    if (path == null) {
      throw StateError('设置 XD_DAEMON_BIN 或传入 daemonPath 指向 xd-daemon 可执行文件');
    }
    final process = await Process.start(
      path,
      extraArgs,
      environment: environment,
    );
    return IpcCoreClient._(process, path, extraArgs);
  }

  /// pkexec 启动（EACCES 引导路径）：polkit 弹窗认证后以 root 拉起同参数 daemon。
  /// 未验证（需真机 polkit + 安装后的 policy 文件）；开发树直连路径不受影响。
  static Future<IpcCoreClient> startPrivileged({
    required String daemonPath,
    List<String> extraArgs = const [],
  }) async {
    final process = await Process.start('pkexec', [daemonPath, ...extraArgs]);
    return IpcCoreClient._(process, daemonPath, extraArgs);
  }

  final Process _process;

  /// 原始启动参数（restartPrivileged 以同参数重启）。
  final String? _daemonPath;
  final List<String> _extraArgs;
  late final StreamSubscription<String> _sub;
  final List<String> stderrLines = [];
  final Map<int, Completer<Map<String, dynamic>>> _pending = {};
  int _nextId = 0;

  final StreamController<Map<String, dynamic>> _notifications =
      StreamController<Map<String, dynamic>>.broadcast();

  @override
  Stream<Map<String, dynamic>> get notifications => _notifications.stream;

  Future<Map<String, dynamic>> _call(String method, [Object? params]) {
    final id = ++_nextId;
    final completer = Completer<Map<String, dynamic>>();
    _pending[id] = completer;
    _process.stdin.writeln(
      encodeRequest(id: id, method: method, params: params),
    );
    return completer.future.timeout(
      const Duration(seconds: 10),
      onTimeout: () {
        _pending.remove(id);
        throw TimeoutException('RPC $method timed out');
      },
    );
  }

  void _onLine(String line) {
    if (line.trim().isEmpty) return;
    final Map<String, dynamic> message;
    try {
      message = jsonDecode(line) as Map<String, dynamic>;
    } catch (_) {
      return; // 无法解析的行直接忽略，不打断流
    }
    final id = message['id'];
    // 契约点：无 id 行 = 服务端通知（JSON-RPC 2.0 notification，proto/v1/README「通知」节），
    // 进 notifications 流由上层按 method 分发；带 id 行按请求应答配对。
    if (id is! int) {
      _notifications.add(message);
      return;
    }
    final completer = _pending.remove(id);
    if (completer == null) return;
    try {
      completer.complete(decodeResult(message));
    } catch (e) {
      // RpcException 或畸形信封导致的 TypeError：都让调用方收到错误而不是悬挂
      completer.completeError(e);
    }
  }

  void _onExit(int code) {
    for (final completer in _pending.values) {
      completer.completeError(StateError('daemon exited with code $code'));
    }
    _pending.clear();
  }

  @override
  Future<PingResult> ping() async => PingResult.fromJson(await _call('ping'));

  @override
  Future<List<DeviceInfo>> listDevices() async {
    final result = await _call('device.list');
    return (result['devices'] as List)
        .map((e) => DeviceInfo.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  @override
  Future<ScanStartResult> scanStart(
    String device, {
    String mode = 'quick',
  }) async => ScanStartResult.fromJson(
    await _call('scan.start', {'device': device, 'mode': mode}),
  );

  @override
  Future<ScanStatusResult> scanStatus(int taskId) async =>
      ScanStatusResult.fromJson(await _call('scan.status', {'taskId': taskId}));

  @override
  Future<ScanResultsPage> scanResults(
    int taskId, {
    int offset = 0,
    int limit = 200,
    bool deletedOnly = false,
  }) async => ScanResultsPage.fromJson(
    await _call('scan.results', {
      'taskId': taskId,
      'offset': offset,
      'limit': limit,
      'deletedOnly': deletedOnly,
    }),
  );

  @override
  Future<void> scanPause(int taskId) => _call('scan.pause', {'taskId': taskId});

  @override
  Future<void> scanResume(int taskId) =>
      _call('scan.resume', {'taskId': taskId});

  @override
  Future<void> scanCancel(int taskId) =>
      _call('scan.cancel', {'taskId': taskId});

  @override
  Future<FsReadResult> fsRead(
    int taskId,
    int idx, {
    int offset = 0,
    int length = 1048576,
  }) async => FsReadResult.fromJson(
    await _call('fs.read', {
      'taskId': taskId,
      'idx': idx,
      'offset': offset,
      'length': length,
    }),
  );

  @override
  Future<ExportStartResult> exportStart(
    int taskId,
    List<int> idxs,
    String targetDir,
  ) async => ExportStartResult.fromJson(
    await _call('export.start', {
      'taskId': taskId,
      'idxs': idxs,
      'targetDir': targetDir,
    }),
  );

  @override
  Future<void> exportCancel(int exportId) =>
      _call('export.cancel', {'exportId': exportId});

  /// EACCES（-32001）重试路径：关掉当前（非特权）daemon，经 pkexec 以**原路径/
  /// 原参数**重启并以 root 拉起；无原始路径（测试构造）时返回 null。
  /// 真实 polkit 认证路径未验证（需真机 + 安装后的 policy 文件）。
  @override
  Future<CoreClient?> restartPrivileged() async {
    final path = _daemonPath;
    if (path == null) return null;
    await close();
    return startPrivileged(daemonPath: path, extraArgs: _extraArgs);
  }

  /// 关闭 daemon（结束时调用，避免 UI 退出留下孤儿进程）。
  @override
  Future<void> close() async {
    await _sub.cancel();
    _process.kill();
    await _process.exitCode;
    await _notifications.close();
  }
}
