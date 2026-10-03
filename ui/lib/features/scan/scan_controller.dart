// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../../core_client/core_client.dart';
import '../../core_client/elevation.dart';
import '../../core_client/protocol.dart';
import '../../util/errors.dart';

/// 扫描页 UI 状态机（Dart 只做状态机：任何扫描逻辑不进 Dart——设计 §6 铁律）。
enum ScanUiState {
  idle,
  starting,
  scanning,
  paused,
  completed,
  canceled,
  failed,
}

// 错误展示映射已迁至 `util/errors.dart`（`describeCoreError`；T6 起 ResultsPage/T8 RecoverPage 共用）。

/// 扫描任务状态机 + 通知/轮询对账。
///
/// 正确性不依赖通知按时到达：`scan.progress` 只做加速，扫描中每 1s 用
/// `scanStatus` 对账（通知丢失/迟到不影响状态收敛）；`scan.finished` 至多一条，
/// 终态先到先得。
class ScanController extends ChangeNotifier {
  ScanController(
    this._client, {
    required this.deviceId,
    this.onClientReplaced,
    ElevationLauncher? elevationLauncher,
    ElevationSessionConnector? elevationConnect,
    this.elevationTimeout = kElevationTimeout,
    this.elevationPollInterval = kElevationPollInterval,
  }) : _launchElevation = elevationLauncher ?? spawnElevation,
       _elevationConnect = elevationConnect ?? connectElevatedSession {
    _subscribe();
  }

  final String deviceId;

  /// 客户端被特权重启替换时回调（main.dart 换用新 client 供后续页面使用）。
  final void Function(CoreClient client)? onClientReplaced;

  /// 提权进程启动器（测试注入假启动器；缺省真起 UAC/osascript/pkexec）。
  final ElevationLauncher _launchElevation;

  /// 提权会话连接器（测试注入假连接器；缺省真连 TCP 会话）。
  final ElevationSessionConnector _elevationConnect;

  /// 提权会话建立超时/轮询间隔（测试注入小值）。
  final Duration elevationTimeout;
  final Duration elevationPollInterval;

  CoreClient _client;
  StreamSubscription<Map<String, dynamic>>? _sub;
  Timer? _pollTimer;
  bool _disposed = false;

  ScanUiState _state = ScanUiState.idle;
  ScanUiState get state => _state;

  String _mode = 'quick';
  String get mode => _mode;

  int? _taskId;
  int? get taskId => _taskId;

  int? _totalBytes;
  int? get totalBytes => _totalBytes;

  int _readBytes = 0;
  int get readBytes => _readBytes;

  int _foundCount = 0;
  int get foundCount => _foundCount;

  int _elapsedMs = 0;
  int get elapsedMs => _elapsedMs;

  String? _error;
  String? get error => _error;

  bool _needsElevation = false;

  /// -32001：设备需要管理员权限（页面据此弹「授权后重试」对话框）。
  bool get needsElevation => _needsElevation;

  /// 提权会话建立中（认证框可能久置 30s）：页面显示「等待授权…」，交互保持禁用。
  bool _elevationPending = false;
  bool get elevationPending => _elevationPending;

  /// 会话已提权（提权引导成功后为真）：再收 -32001 = 已 root 仍无权限（macOS：缺 FDA）。
  bool _elevatedSession = false;
  bool get elevatedSession => _elevatedSession;

  /// 提权会话目录（UI 自建的 0700 临时目录，root daemon 把 port-file 属主交还其属主）。
  Directory? _elevationDir;

  /// 生效中的客户端（特权重启/提权会话替换后为新的；页面转结果页须用它而非旧引用）。
  CoreClient get client => _client;

  /// 进行中（含 starting）：模式选择与开始按钮禁用。
  bool get busy =>
      _state == ScanUiState.starting ||
      _state == ScanUiState.scanning ||
      _state == ScanUiState.paused;

  /// 确定进度 = readBytes/totalBytes；totalBytes 未知或为 0 → null（不确定进度条）。
  double? get percent {
    final total = _totalBytes;
    if (total == null || total == 0) return null;
    return (_readBytes / total).clamp(0.0, 1.0);
  }

  void setMode(String value) {
    if (value == _mode) return;
    _mode = value;
    _notify();
  }

  /// 开始（或终态后重扫）。-32001 时不进 failed：置 [needsElevation] 交页面引导。
  Future<void> start() async {
    if (busy) return;
    await _startScan();
  }

  /// 无 busy 守卫的扫描启动：提权引导路径持着 [ScanUiState.starting]（提权期间禁交互）
  /// 走到这里，直接启动而不回退状态机（守卫会把它挡回去）。
  Future<void> _startScan() async {
    _state = ScanUiState.starting;
    _needsElevation = false;
    _error = null;
    _taskId = null;
    _totalBytes = null;
    _readBytes = 0;
    _foundCount = 0;
    _elapsedMs = 0;
    _notify();
    try {
      final result = await _client.scanStart(deviceId, mode: _mode);
      _taskId = result.taskId;
      _totalBytes = result.totalBytes;
      _state = ScanUiState.scanning;
      _startPolling();
    } catch (e) {
      if (e is RpcException && e.code == -32001) {
        if (_elevatedSession && defaultTargetPlatform == TargetPlatform.macOS) {
          // osascript 提权 ≠ FDA（T3 移交）：root 之后仍 EPERM ⇒ 指路系统设置，
          // 再弹一轮提权框没有意义（提权已完成）。
          _error = '已提权但仍缺完全磁盘访问（系统设置 > 隐私与安全性）';
          _state = ScanUiState.failed;
        } else {
          _needsElevation = true;
          _state = ScanUiState.idle;
        }
      } else {
        _error = describeCoreError(e);
        _state = ScanUiState.failed;
      }
    }
    _notify();
  }

  Future<void> pause() async {
    final id = _taskId;
    if (id == null || _state != ScanUiState.scanning) return;
    try {
      await _client.scanPause(id);
      _state = ScanUiState.paused;
      _stopPolling();
    } catch (e) {
      _finish(ScanUiState.failed, error: e);
    }
    _notify();
  }

  Future<void> resume() async {
    final id = _taskId;
    if (id == null || _state != ScanUiState.paused) return;
    try {
      await _client.scanResume(id);
      _state = ScanUiState.scanning;
      _startPolling();
    } catch (e) {
      _finish(ScanUiState.failed, error: e);
    }
    _notify();
  }

  Future<void> cancel() async {
    final id = _taskId;
    if (id == null) return;
    if (_state != ScanUiState.scanning && _state != ScanUiState.paused) return;
    try {
      await _client.scanCancel(id);
      _state = ScanUiState.canceled;
      _stopPolling();
    } catch (e) {
      _finish(ScanUiState.failed, error: e);
    }
    _notify();
  }

  /// 对话框 [取消]：退出提权引导，回到可重试的 idle。
  void dismissElevation() {
    _needsElevation = false;
    _notify();
  }

  /// 对话框 [授权后重试]，两条路径：
  ///
  /// ① **三平台提权引导**（客户端报得出 daemonPath）：平台命令（Windows UAC / macOS osascript /
  ///    Linux pkexec，见 [elevationPlanFor]）→ 起提权进程 → 轮询 port-file（≤[elevationTimeout]，
  ///    两种半行形态都重试）→ TCP 握手 → 替换为提权会话 client → 重试 scanStart。
  ///    用户取消/超时 ⇒ 文案「未获得授权」（不静默、不悬挂）。
  /// ② **旧路径**（拿不到 daemonPath：测试 fake/内嵌）：[CoreClient.restartPrivileged]
  ///    以同参数 in-place 重启（pkexec stdio；stdio 句柄不经文件，无 port-file 属主问题）。
  Future<void> retryWithPrivileges() async {
    _needsElevation = false;
    final daemonPath = _client.daemonPath;
    if (daemonPath == null) {
      await _retryViaClientRestart();
      return;
    }
    // 提权期间禁交互（认证框可能久置）：starting 即 busy，文案见 elevationPending。
    _state = ScanUiState.starting;
    _elevationPending = true;
    _error = null;
    _notify();
    Directory? dir;
    try {
      // UI 自建 0700 会话目录：root daemon 写 port-file 时把属主交还目录属主（本用户），
      // 否则 0600 属主=root，UI 读不到令牌（见 crates/xd-daemon/src/portfile.rs）。
      dir = Directory.systemTemp.createTempSync('xiaodun-elev-');
      final portFile = '${dir.path}/session.port';
      final plan = elevationPlanFor(
        defaultTargetPlatform,
        daemonPath: daemonPath,
        portFile: portFile,
        ownerPid: pid,
      );
      final fresh = await _elevationConnect(
        portFile: portFile,
        launcherExit: _launchElevation(plan), // 不 await：与轮询并行，取消即刻唤醒
        timeout: elevationTimeout,
        interval: elevationPollInterval,
      );
      final old = _client;
      final oldDir = _elevationDir;
      // 不 await：广播流订阅的 cancel future 在 fake async 下不收敛（Dart null-future），
      // 且取消订阅本就无需等待。
      unawaited(_sub?.cancel());
      _client = fresh;
      _subscribe();
      _elevatedSession = true;
      _elevationDir = dir;
      if (oldDir != null) _cleanupElevationDir(oldDir);
      onClientReplaced?.call(fresh);
      // 旧（非提权）daemon 已无用途：按契约关闭；失败不影响新会话
      unawaited(old.close().catchError((Object _) {}));
      _elevationPending = false;
    } catch (e) {
      if (dir != null) _cleanupElevationDir(dir);
      if (_disposed) return;
      _elevationPending = false;
      _error = switch (e) {
        ElevationDeniedException() ||
        ElevationTimeoutException() => '未获得授权（$e）',
        _ => describeCoreError(e),
      };
      _state = ScanUiState.failed;
      _notify();
      return;
    }
    await _startScan();
  }

  /// 旧路径：pkexec 以同参数重启客户端（旧 client 已由实现关闭），
  /// 通知应用层换用新 client，再重试 scanStart（mode 保持）。
  Future<void> _retryViaClientRestart() async {
    try {
      final fresh = await _client.restartPrivileged();
      if (fresh == null) {
        _error = '无法以管理员权限重启核心服务';
        _state = ScanUiState.failed;
        _notify();
        return;
      }
      // 不 await：广播流订阅的 cancel future 在 fake async 下不收敛（Dart null-future），
      // 且取消订阅本就无需等待。
      unawaited(_sub?.cancel());
      _client = fresh;
      _subscribe();
      onClientReplaced?.call(fresh);
    } catch (e) {
      _error = describeCoreError(e);
      _state = ScanUiState.failed;
      _notify();
      return;
    }
    await start();
  }

  void _subscribe() {
    _sub = _client.notifications.listen(_onNotification);
  }

  /// 会话目录收尾（尽力而为；port-file 清理归属见 docs/security §10：daemon 自退时删自己的
  /// port-file，UI 在会话结束后删整个目录）。失败只留一个空目录（系统临时目录有清理策略），
  /// 不打断流程。
  void _cleanupElevationDir(Directory dir) {
    try {
      dir.deleteSync(recursive: true);
    } on FileSystemException {
      // 忽略：目录/文件可能已被并发清理
    }
  }

  /// 通知分发。T4 实测：契约片段里 `"id":null` 的应答行会入流 → `method` 可能为
  /// null；只认本任务（taskId 过滤）+ 已知 method，畸形/陌生消息一律忽略（轮询兜底）。
  void _onNotification(Map<String, dynamic> message) {
    final method = message['method'];
    final params = message['params'];
    if (method is! String || params is! Map<String, dynamic>) return;
    if (_taskId == null || params['taskId'] != _taskId) return;
    try {
      switch (method) {
        case 'scan.progress':
          _applyStatus(ScanStatusResult.fromJson(params));
        case 'scan.finished': // 契约上无 readBytes 字段，单独取值
          _apply(
            state: params['state'] as String,
            foundCount: params['foundCount'] as int,
            elapsedMs: params['elapsedMs'] as int,
          );
      }
    } catch (_) {
      // 畸形通知不打断扫描：轮询对账兜底
    }
  }

  void _startPolling() {
    _stopPolling();
    _pollTimer = Timer.periodic(
      const Duration(seconds: 1),
      (_) => _reconcile(),
    );
  }

  void _stopPolling() {
    _pollTimer?.cancel();
    _pollTimer = null;
  }

  /// 通知丢失/迟到的兜底：以 scan.status 为准对账。
  Future<void> _reconcile() async {
    final id = _taskId;
    if (id == null || _state != ScanUiState.scanning) return;
    try {
      final status = await _client.scanStatus(id);
      // 在途响应可能过期（已暂停/取消/重扫）→ 丢弃，防状态回跳
      if (id != _taskId || _state != ScanUiState.scanning) return;
      _applyStatus(status);
    } catch (e) {
      // 对账失败 = 传输层已死（daemon 退出/超时）→ 终态失败，不静默悬挂
      _finish(ScanUiState.failed, error: e);
      _notify();
    }
  }

  void _applyStatus(ScanStatusResult status) => _apply(
    state: status.state,
    foundCount: status.foundCount,
    elapsedMs: status.elapsedMs,
    readBytes: status.readBytes,
  );

  void _apply({
    required String state,
    required int foundCount,
    required int elapsedMs,
    int? readBytes,
  }) {
    if (_isTerminal(_state)) return; // 终态先到先得（finished 至多一条）
    _foundCount = foundCount;
    _elapsedMs = elapsedMs;
    if (readBytes != null) _readBytes = readBytes;
    switch (state) {
      case 'scanning' || 'pending':
        _state = ScanUiState.scanning;
        _startPolling();
      case 'paused':
        _state = ScanUiState.paused;
        _stopPolling();
      case 'completed':
        _finish(ScanUiState.completed);
      case 'canceled':
        _finish(ScanUiState.canceled);
      case 'failed':
        _finish(ScanUiState.failed);
    }
    _notify();
  }

  /// 统一终态迁移：停表（轮询）+ 记录错误文案（可空）。
  void _finish(ScanUiState state, {Object? error}) {
    _stopPolling();
    _state = state;
    if (error != null) _error = describeCoreError(error);
  }

  static bool _isTerminal(ScanUiState state) =>
      state == ScanUiState.completed ||
      state == ScanUiState.canceled ||
      state == ScanUiState.failed;

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _stopPolling();
    _sub?.cancel();
    final dir = _elevationDir;
    if (dir != null) _cleanupElevationDir(dir);
    super.dispose();
  }
}
