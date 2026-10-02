// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'core_client.dart';
import 'protocol.dart';

/// 桌面实现：spawn 特权 daemon，stdio 上每行一条 JSON-RPC 消息。
class IpcCoreClient implements CoreClient {
  IpcCoreClient._(this._process) {
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
    return IpcCoreClient._(process);
  }

  final Process _process;
  late final StreamSubscription<String> _sub;
  final List<String> stderrLines = [];
  final Map<int, Completer<Map<String, dynamic>>> _pending = {};
  int _nextId = 0;

  Future<Map<String, dynamic>> _call(String method) {
    final id = ++_nextId;
    final completer = Completer<Map<String, dynamic>>();
    _pending[id] = completer;
    _process.stdin.writeln(encodeRequest(id: id, method: method, params: null));
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
    if (id is! int) return;
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

  /// 关闭 daemon（结束时调用，避免 UI 退出留下孤儿进程）。
  @override
  Future<void> close() async {
    await _sub.cancel();
    _process.kill();
    await _process.exitCode;
  }
}
