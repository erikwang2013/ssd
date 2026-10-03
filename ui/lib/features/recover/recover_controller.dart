// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 恢复页状态机（无 UI）：picking → exporting → done | failed | canceled。
// - selectTarget()：file_selector 目录对话框（测试缝 = FileSelectorPlatform.instance 注入 fake）；
// - start() → exportStart(taskId, idxs, dir)；订阅 export.progress/finished（按 exportId 过滤）；
// - finished 抢跑响应（T3 底盘竞序：daemon 先起后台线程再回响应）→ 在途通知寄存回放；
//   导出无轮询兜底，丢一条 finished 即永久停在导出中，故必须寄存；
// - 错误文案：-32006/-32010 走专用引导，其余 describeCoreError（RpcException 只显示 message）；
// - 通知订阅取消一律 unawaited（T5 铁律：fake async 下 await cancel 不收敛）。
//
// displayName 铁律（T4 移交）：写路径只传 idx；报告落盘名一律 ExportReportItem.name。
import 'dart:async';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/foundation.dart';

import '../../core_client/core_client.dart';
import '../../core_client/protocol.dart';
import '../../util/errors.dart';

enum RecoverUiState { picking, exporting, done, failed, canceled }

class RecoverController extends ChangeNotifier {
  RecoverController(this._client, {required this.taskId, required this.idxs}) {
    _sub = _client.notifications.listen(_onNotification);
  }

  final CoreClient _client;
  final int taskId;
  final List<int> idxs;

  StreamSubscription<Map<String, dynamic>>? _sub;
  bool _disposed = false;

  RecoverUiState _state = RecoverUiState.picking;
  String? _targetDir;
  String? _error;
  int? _exportId;
  bool _starting = false;
  int? _estimatedBytes;
  int _done = 0;
  int _total = 0;
  int _writtenBytes = 0;
  ExportFinished? _report;
  Map<String, dynamic>? _pendingFinished;

  RecoverUiState get state => _state;
  String? get targetDir => _targetDir;

  /// 开始失败/取消失败的展示文案（[describeCoreError] 体系）。
  String? get error => _error;

  /// 启动响应携带的交付字节上界（未启动为 null）。
  int? get estimatedBytes => _estimatedBytes;

  int get done => _done;
  int get total => _total;
  int get writtenBytes => _writtenBytes;

  /// 终态报告（done/canceled 时非空）。
  ExportFinished? get report => _report;

  /// 已拿到 exportId 才可取消（启动响应到达前无可取消对象）。
  bool get canCancel => _exportId != null;

  Future<void> selectTarget() async {
    final dir = await getDirectoryPath();
    if (dir == null || dir.isEmpty || _disposed) return;
    _targetDir = dir;
    _error = null;
    _state = RecoverUiState.picking; // 失败后换目标即回到可开始
    _notify();
  }

  Future<void> start() async {
    final dir = _targetDir;
    if (dir == null || _state == RecoverUiState.exporting) return;
    _state = RecoverUiState.exporting;
    _error = null;
    _pendingFinished = null;
    _starting = true;
    _notify();
    try {
      final res = await _client.exportStart(taskId, idxs, dir);
      if (_disposed) return;
      _exportId = res.exportId;
      _estimatedBytes = res.estimatedBytes;
      _starting = false;
      final pending = _pendingFinished;
      _pendingFinished = null;
      if (pending != null) _applyFinished(pending); // 内含 exportId 二次校验
    } catch (e) {
      if (_disposed) return;
      _starting = false;
      _pendingFinished = null;
      _error = _describeStartError(e);
      _state = RecoverUiState.failed;
    }
    _notify();
  }

  Future<void> cancel() async {
    final id = _exportId;
    if (id == null) return;
    try {
      await _client.exportCancel(id);
    } catch (e) {
      if (_disposed) return;
      _error = describeCoreError(e);
      _notify();
    }
  }

  /// -32006/-32010 专用引导（计划 773 逐字）；其余交给 describeCoreError。
  static String _describeStartError(Object e) {
    if (e is RpcException) {
      switch (e.code) {
        case -32006:
          return '目标不能是源设备所在的盘，请换一个文件夹';
        case -32010:
          return '目标盘剩余空间不足';
      }
    }
    return describeCoreError(e);
  }

  void _onNotification(Map<String, dynamic> message) {
    final method = message['method'];
    final params = message['params'];
    if (method is! String || params is! Map<String, dynamic>) return;
    if (_exportId == null) {
      // 响应未到：只可能是本次导出的通知（本页同时只有一个导出）→ 寄存待回放
      if (_starting && method == 'export.finished') _pendingFinished = params;
      return;
    }
    if (params['exportId'] != _exportId) return;
    try {
      switch (method) {
        case 'export.progress':
          _done = params['done'] as int;
          _total = params['total'] as int;
          _writtenBytes = params['writtenBytes'] as int;
          _notify();
        case 'export.finished':
          _applyFinished(params);
      }
    } catch (_) {
      // 畸形通知忽略：终态仍由后续合法 finished 决定
    }
  }

  void _applyFinished(Map<String, dynamic> params) {
    if (params['exportId'] != _exportId) return;
    final report = ExportFinished.fromJson(params);
    _report = report;
    _state = report.canceled ? RecoverUiState.canceled : RecoverUiState.done;
    _notify();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    unawaited(_sub?.cancel()); // T5 铁律：fake async 下 await cancel 永不收敛
    super.dispose();
  }
}
