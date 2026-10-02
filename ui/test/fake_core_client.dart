// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:async';
import 'dart:typed_data';

import 'package:xiaodun_ui/core_client/core_client.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';

/// 测试公用内存态客户端：fixture 可注入、调用序列可断言、通知可手动 emit。
/// 各任务按需扩展（新增字段优于新增子类）。
class FakeCoreClient implements CoreClient {
  FakeCoreClient({
    List<DeviceInfo> devices = const [],
    List<ScanEntry> entries = const [],
    this.failWith,
  }) : devices = List.of(devices),
       entries = List.of(entries);

  final List<DeviceInfo> devices;

  /// scanResults 的数据源（内存分页 + deletedOnly 过滤）。
  final List<ScanEntry> entries;

  /// total 覆盖（契约外场景注入：total 与实际条目不符——分页封口守卫用）。
  int? totalOverride;

  /// 错误注入：按 method 返回非 null 时该次调用抛它（全失败/按方法/flaky 都靠它）。
  final Object? Function(String method)? failWith;

  /// 调用序列（`scanStart(image:a.img, mode:deep)`、`scanPause(3)` …）。
  final List<String> calls = [];

  /// 参数级记录（断言透传值用）：scanResults/fsRead/exportStart/exportCancel。
  final List<({int taskId, int offset, int limit, bool deletedOnly})>
  scanResultQueries = [];
  final List<({int taskId, int idx, int offset, int length})> fsReadQueries =
      [];
  final List<({int taskId, List<int> idxs, String targetDir})> exportStarts =
      [];
  final List<int> canceledExports = [];

  /// 可配置返回值。
  ScanStartResult scanStartResult = const ScanStartResult(
    taskId: 1,
    fs: 'exfat',
    totalBytes: 0,
  );
  ScanStatusResult scanStatusResult = const ScanStatusResult(
    taskId: 1,
    state: 'scanning',
    readBytes: 0,
    foundCount: 0,
    elapsedMs: 0,
  );
  ExportStartResult exportStartResult = const ExportStartResult(
    exportId: 1,
    fileCount: 0,
    estimatedBytes: 0,
  );

  /// fs.read 分片脚本；缺省 = 空交付且 eof。
  FsReadResult Function(int taskId, int idx, int offset, int length)? onFsRead;

  final StreamController<Map<String, dynamic>> _notifications =
      StreamController<Map<String, dynamic>>.broadcast();

  @override
  Stream<Map<String, dynamic>> get notifications => _notifications.stream;

  /// 手动发一条服务端通知（无 id 信封）。
  void emitNotification(Map<String, dynamic> message) =>
      _notifications.add(message);

  /// EACCES（-32001）重试：记录一次特权重启，缺省返回自身（测试用 failWith
  /// 计数控制首次 -32001，重试即成功）。真实 pkexec 路径未验证（需真机 polkit）。
  CoreClient? privilegedClient;

  /// 注入「实现不支持提权重启」（restartPrivileged → null）。
  bool restartReturnsNull = false;

  @override
  Future<CoreClient?> restartPrivileged() async {
    calls.add('restartPrivileged()');
    if (restartReturnsNull) return null;
    return privilegedClient ?? this;
  }

  void _fail(String method) {
    final error = failWith?.call(method);
    if (error != null) throw error;
  }

  @override
  Future<PingResult> ping() async {
    calls.add('ping');
    _fail('ping');
    return const PingResult(pong: true, version: 'fake', protocol: 1);
  }

  @override
  Future<List<DeviceInfo>> listDevices() async {
    calls.add('listDevices');
    _fail('listDevices');
    return List.of(devices);
  }

  @override
  Future<ScanStartResult> scanStart(
    String device, {
    String mode = 'quick',
  }) async {
    calls.add('scanStart($device, mode:$mode)');
    _fail('scanStart');
    return scanStartResult;
  }

  @override
  Future<ScanStatusResult> scanStatus(int taskId) async {
    calls.add('scanStatus($taskId)');
    _fail('scanStatus');
    return scanStatusResult;
  }

  @override
  Future<ScanResultsPage> scanResults(
    int taskId, {
    int offset = 0,
    int limit = 200,
    bool deletedOnly = false,
  }) async {
    calls.add(
      'scanResults($taskId, offset:$offset, limit:$limit, deletedOnly:$deletedOnly)',
    );
    scanResultQueries.add((
      taskId: taskId,
      offset: offset,
      limit: limit,
      deletedOnly: deletedOnly,
    ));
    _fail('scanResults');
    if (limit < 1 || limit > 1000) {
      // daemon 契约（xd-core handlers.rs）：limit ∈ 1..=1000，文案逐字
      throw const RpcException(
        -32602,
        'Invalid params: limit out of range 1..=1000',
      );
    }
    // daemon 侧 ORDER BY idx：分页排序键 = idx，乱序注入也按 idx 出页
    final filtered =
        (deletedOnly ? entries.where((e) => e.deleted) : entries).toList()
          ..sort((a, b) => a.idx.compareTo(b.idx));
    return ScanResultsPage(
      total: totalOverride ?? filtered.length,
      entries: filtered.skip(offset).take(limit).toList(),
    );
  }

  @override
  Future<void> scanPause(int taskId) async {
    calls.add('scanPause($taskId)');
    _fail('scanPause');
  }

  @override
  Future<void> scanResume(int taskId) async {
    calls.add('scanResume($taskId)');
    _fail('scanResume');
  }

  @override
  Future<void> scanCancel(int taskId) async {
    calls.add('scanCancel($taskId)');
    _fail('scanCancel');
  }

  @override
  Future<FsReadResult> fsRead(
    int taskId,
    int idx, {
    int offset = 0,
    int length = 1048576,
  }) async {
    calls.add('fsRead($taskId, $idx, offset:$offset, length:$length)');
    fsReadQueries.add((
      taskId: taskId,
      idx: idx,
      offset: offset,
      length: length,
    ));
    _fail('fsRead');
    return onFsRead?.call(taskId, idx, offset, length) ??
        FsReadResult(bytes: Uint8List(0), eof: true);
  }

  @override
  Future<ExportStartResult> exportStart(
    int taskId,
    List<int> idxs,
    String targetDir,
  ) async {
    calls.add('exportStart($taskId, $idxs, $targetDir)');
    exportStarts.add((
      taskId: taskId,
      idxs: List.of(idxs),
      targetDir: targetDir,
    ));
    _fail('exportStart');
    return exportStartResult;
  }

  @override
  Future<void> exportCancel(int exportId) async {
    calls.add('exportCancel($exportId)');
    canceledExports.add(exportId);
    _fail('exportCancel');
  }

  @override
  Future<void> close() async {
    if (!_notifications.isClosed) await _notifications.close();
  }
}
