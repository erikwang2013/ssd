// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../core_client/core_client.dart';
import '../../core_client/protocol.dart';

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

/// 错误展示映射（T4 移交）：传输层文案保持诊断原样，展示层在此翻译。
/// - `StateError`：daemon 退出（-15 是用户在途退出的正常路径，直接 `'$e'`
///   会渲染 `Bad state: daemon exited with code -15`）；
/// - `TimeoutException`：close 后的新调用是挂 10s 超时，非 StateError；
/// - 其余（RpcException 契约文案等）：沿用现有文案。
String describeScanError(Object error) {
  if (error is StateError) return '核心服务已退出，请重启应用';
  if (error is TimeoutException) return '核心服务无响应';
  return '$error';
}

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
  }) {
    _subscribe();
  }

  final String deviceId;

  /// 客户端被特权重启替换时回调（main.dart 换用新 client 供后续页面使用）。
  final void Function(CoreClient client)? onClientReplaced;

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
        _needsElevation = true;
        _state = ScanUiState.idle;
      } else {
        _error = describeScanError(e);
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

  /// 对话框 [授权后重试]：pkexec 以同参数重启客户端（旧 client 已由实现关闭），
  /// 通知应用层换用新 client，再重试 scanStart（mode 保持）。
  Future<void> retryWithPrivileges() async {
    _needsElevation = false;
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
      _error = describeScanError(e);
      _state = ScanUiState.failed;
      _notify();
      return;
    }
    await start();
  }

  void _subscribe() {
    _sub = _client.notifications.listen(_onNotification);
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
      _applyStatus(await _client.scanStatus(id));
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
    if (error != null) _error = describeScanError(error);
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
    super.dispose();
  }
}
