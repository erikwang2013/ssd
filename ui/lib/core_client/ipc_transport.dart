// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'core_client.dart';
import 'protocol.dart';

/// 行协议公共基类：`lines`（响应/通知行流）与 `write`（发一行）由子类适配
/// （process stdio / TCP socket），`_call`、应答配对与通知流不再逐实现复制。
abstract class LineCoreClient implements CoreClient {
  // 写出函数走位置参数（`this._write`）：命名参数不能是私有初始化形参。
  LineCoreClient(this._write, {required Stream<String> lines}) {
    _sub = lines.listen(_onLine);
  }

  final void Function(String) _write;
  late final StreamSubscription<String> _sub;
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
    _write(encodeRequest(id: id, method: method, params: params));
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

  /// 在飞请求整体失败（进程退出 / socket 断开 / 认证失败）；此后调用方不再悬挂。
  void _failPending(Object error) {
    for (final completer in _pending.values) {
      completer.completeError(error);
    }
    _pending.clear();
  }

  @override
  Future<void> close() async {
    await _sub.cancel();
    await _notifications.close();
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

  /// 提权重启：不支持 in-place 重启的实现返回 null（契约见 CoreClient）。
  @override
  Future<CoreClient?> restartPrivileged();
}

/// 桌面实现：spawn 特权 daemon，stdio 上每行一条 JSON-RPC 消息。
class IpcCoreClient extends LineCoreClient {
  IpcCoreClient._(this._process, [this._daemonPath, this._extraArgs = const []])
    : super(
        (line) => _process.stdin.writeln(line),
        lines: _process.stdout
            .transform(const Utf8Decoder(allowMalformed: true))
            .transform(const LineSplitter()),
      ) {
    _process.stderr
        .transform(const Utf8Decoder(allowMalformed: true))
        .transform(const LineSplitter())
        .listen((line) => stderrLines.add(line));
    _process.exitCode.then(
      (code) => _failPending(StateError('daemon exited with code $code')),
    );
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
  final List<String> stderrLines = [];

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
    _process.kill();
    await _process.exitCode;
    await super.close();
  }
}

/// 提权会话实现：TCP 回环 + 令牌握手（协议与威胁模型见
/// crates/xd-daemon/src/transport.rs 头注与 docs/security）。
///
/// 首行必须 `{"auth":"<token>"}`；认证失败时 daemon 回 -32001（id=null）即断——该行无 id，
/// 不能走基类「无 id = 业务通知」分流（会被静默丢进通知流），在此升级为会话失败：
/// 在飞请求即错、后续调用快速失败。
class SocketCoreClient extends LineCoreClient {
  SocketCoreClient._(this._socket)
    : super(
        (line) => _socket.writeln(line),
        lines: _socket
            .cast<List<int>>()
            .transform(const Utf8Decoder(allowMalformed: true))
            .transform(const LineSplitter()),
      ) {
    // 断开（含写错误）即在飞请求失败；onError 兜底防错误向 zone 泄漏。
    _socket.done.then(
      (_) => _failPending(StateError('socket closed')),
      onError: (_) => _failPending(StateError('socket closed')),
    );
  }

  /// 连接提权 daemon。两种给源：
  /// ① [portFile]：读 daemon 原子写的 `<port> <token>`（提权路径；文件须已存在——
  ///    轮询到文件出现归调用方，见提权引导）；
  /// ② [addr]（`host:port`）+ [token]：直连（测试/内嵌）。
  static Future<SocketCoreClient> start({
    String? addr,
    String? token,
    String? portFile,
  }) async {
    if (portFile != null) {
      final info = parsePortFile(await File(portFile).readAsString());
      addr = '127.0.0.1:${info.port}';
      token = info.token;
    }
    if (addr == null || token == null) {
      throw StateError('SocketCoreClient.start 需要 portFile 或 addr+token');
    }
    final sep = addr.lastIndexOf(':');
    if (sep <= 0) throw FormatException('addr 需为 host:port：$addr');
    final socket = await Socket.connect(
      addr.substring(0, sep),
      int.parse(addr.substring(sep + 1)),
    );
    final client = SocketCoreClient._(socket);
    // 握手无 ack（协议如此）：认证失败以 -32001 行 + 断开表达，见 _onLine。
    client._write('{"auth":"$token"}');
    return client;
  }

  final Socket _socket;
  RpcException? _authError;

  /// 提权会话本身即已提权（TCP 传输就是提权形态）：无「重启为提权」概念，返回 null
  /// （契约见 CoreClient：不支持 in-place 重启的实现返回 null）。提权引导链路中，
  /// 上层拿到 port-file 后是**新建** SocketCoreClient，不走这里。
  @override
  Future<CoreClient?> restartPrivileged() async => null;

  /// port-file 内容解析：一行 `<port> <token>`（纯函数，可单测）。
  static ({int port, String token}) parsePortFile(String content) {
    final parts = content
        .split(RegExp(r'\s+'))
        .where((s) => s.isNotEmpty)
        .toList();
    if (parts.length != 2) {
      throw FormatException(
        'port-file 须为一行 "<port> <token>"：${content.trim()}',
      );
    }
    final port = int.tryParse(parts[0]);
    if (port == null || port <= 0 || port > 65535) {
      throw FormatException('port-file 端口非法：${parts[0]}');
    }
    return (port: port, token: parts[1]);
  }

  @override
  void _onLine(String line) {
    if (line.trim().isEmpty) return;
    try {
      final message = jsonDecode(line) as Map<String, dynamic>;
      final error = message['error'];
      // 认证失败：daemon 对该连接只发这一条（-32001，id=null）后断开。
      if (message['id'] is! int && error is Map && error['code'] == -32001) {
        final e = RpcException(
          error['code'] as int,
          error['message'] as String,
        );
        _authError = e;
        _failPending(e);
        return;
      }
    } catch (_) {
      // 非 JSON 行：交给基类（忽略）
    }
    super._onLine(line);
  }

  @override
  Future<Map<String, dynamic>> _call(String method, [Object? params]) {
    final error = _authError;
    if (error != null) return Future.error(error);
    return super._call(method, params);
  }

  @override
  Future<void> close() async {
    // 对端已断时 close 会带错完成：会话失败已由 _failPending 交付，关闭噪声不再上抛。
    await _socket.close().catchError((Object _) {});
    _socket.destroy(); // 立即释放 fd，不等对端 FIN
    await super.close();
  }
}
