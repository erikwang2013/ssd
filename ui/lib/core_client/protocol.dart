// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:convert';
import 'dart:typed_data';

class DeviceInfo {
  const DeviceInfo({
    required this.id,
    required this.name,
    required this.kind,
    required this.sizeBytes,
    required this.removable,
    this.fsGuess,
  });

  final String id;
  final String name;
  final String kind; // physical | volume | image
  final int sizeBytes;
  final bool removable;
  final String? fsGuess;

  factory DeviceInfo.fromJson(Map<String, dynamic> json) => DeviceInfo(
    id: json['id'] as String,
    name: json['name'] as String,
    kind: json['kind'] as String,
    sizeBytes: json['sizeBytes'] as int,
    removable: json['removable'] as bool,
    fsGuess: json['fsGuess'] as String?,
  );

  Map<String, dynamic> toJson() => {
    'id': id,
    'name': name,
    'kind': kind,
    'sizeBytes': sizeBytes,
    'removable': removable,
    'fsGuess': fsGuess,
  };
}

class PingResult {
  const PingResult({
    required this.pong,
    required this.version,
    required this.protocol,
  });

  final bool pong;
  final String version;
  final int protocol;

  factory PingResult.fromJson(Map<String, dynamic> json) => PingResult(
    pong: json['pong'] as bool,
    version: json['version'] as String,
    protocol: json['protocol'] as int,
  );
}

/// v1.2 扫描条目。`byteOffset`/`contiguous` 为可空增量字段：
/// 线上缺省省略（Rust 侧 `skip_serializing_if = "Option::is_none"`），解码为 null。
class ScanEntry {
  const ScanEntry({
    required this.idx,
    required this.name,
    required this.path,
    required this.ext,
    required this.sizeBytes,
    required this.deleted,
    required this.isDir,
    required this.quality,
    required this.firstCluster,
    this.byteOffset,
    this.contiguous,
  });

  final int idx;
  final String name;
  final String path;
  final String ext;
  final int sizeBytes;
  final bool deleted;
  final bool isDir;
  final String quality; // complete | maybeDamaged | carved
  final int firstCluster;

  /// 雕刻条目在未分配空间内的起始字节坐标；FS 条目恒 null。
  final int? byteOffset;

  /// exFAT 拓扑提示：true = NoFatChain（规范保证连续）；false = 走 FAT 链；
  /// null = 未知（fat/雕刻/迁移前旧行）——不得据此宣称「连续」。
  final bool? contiguous;

  factory ScanEntry.fromJson(Map<String, dynamic> json) => ScanEntry(
    idx: json['idx'] as int,
    name: json['name'] as String,
    path: json['path'] as String,
    ext: json['ext'] as String,
    sizeBytes: json['sizeBytes'] as int,
    deleted: json['deleted'] as bool,
    isDir: json['isDir'] as bool,
    quality: json['quality'] as String,
    firstCluster: json['firstCluster'] as int,
    byteOffset: json['byteOffset'] as int?,
    contiguous: json['contiguous'] as bool?,
  );

  Map<String, dynamic> toJson() => {
    'idx': idx,
    'name': name,
    'path': path,
    'ext': ext,
    'sizeBytes': sizeBytes,
    'deleted': deleted,
    'isDir': isDir,
    'quality': quality,
    'firstCluster': firstCluster,
    'byteOffset': ?byteOffset,
    'contiguous': ?contiguous,
  };

  /// 展示名：雕刻件无名 → 与导出落盘命名一致（`carved_{idx:06}.{ext}`，ext 空回退 `bin`）。
  String get displayName => name.isEmpty
      ? 'carved_${idx.toString().padLeft(6, '0')}.${ext.isEmpty ? 'bin' : ext}'
      : name;
}

class ScanStartResult {
  const ScanStartResult({
    required this.taskId,
    required this.fs,
    required this.totalBytes,
  });

  final int taskId;
  final String fs; // fat | exfat
  final int totalBytes;

  factory ScanStartResult.fromJson(Map<String, dynamic> json) =>
      ScanStartResult(
        taskId: json['taskId'] as int,
        fs: json['fs'] as String,
        totalBytes: json['totalBytes'] as int,
      );
}

/// `scan.status` 响应与 `scan.progress` 通知 params 同形。
class ScanStatusResult {
  const ScanStatusResult({
    required this.taskId,
    required this.state,
    required this.readBytes,
    required this.foundCount,
    required this.elapsedMs,
  });

  final int taskId;
  final String state; // pending|scanning|paused|canceled|completed|failed
  final int readBytes;
  final int foundCount;
  final int elapsedMs;

  factory ScanStatusResult.fromJson(Map<String, dynamic> json) =>
      ScanStatusResult(
        taskId: json['taskId'] as int,
        state: json['state'] as String,
        readBytes: json['readBytes'] as int,
        foundCount: json['foundCount'] as int,
        elapsedMs: json['elapsedMs'] as int,
      );
}

class ScanResultsPage {
  const ScanResultsPage({required this.total, required this.entries});

  final int total;
  final List<ScanEntry> entries;

  factory ScanResultsPage.fromJson(Map<String, dynamic> json) =>
      ScanResultsPage(
        total: json['total'] as int,
        entries: (json['entries'] as List)
            .map((e) => ScanEntry.fromJson(e as Map<String, dynamic>))
            .toList(),
      );
}

class FsReadResult {
  const FsReadResult({required this.bytes, required this.eof});

  final Uint8List bytes;

  /// 已交付到该条目可得数据的末端（短链/损坏件可能早于 sizeBytes）。
  final bool eof;

  factory FsReadResult.fromJson(Map<String, dynamic> json) => FsReadResult(
    bytes: base64Decode(json['bytesBase64'] as String),
    eof: json['eof'] as bool,
  );
}

class ExportStartResult {
  const ExportStartResult({
    required this.exportId,
    required this.fileCount,
    required this.estimatedBytes,
  });

  final int exportId;
  final int fileCount;

  /// Σ sizeBytes，交付字节上界（降级件实际可能更短）。
  final int estimatedBytes;

  factory ExportStartResult.fromJson(Map<String, dynamic> json) =>
      ExportStartResult(
        exportId: json['exportId'] as int,
        fileCount: json['fileCount'] as int,
        estimatedBytes: json['estimatedBytes'] as int,
      );
}

/// `export.finished` 通知中的降级/失败条目（成功件不回传）。
class ExportReportItem {
  const ExportReportItem({
    required this.idx,
    required this.name,
    required this.status,
    this.reason,
  });

  final int idx;

  /// 实际落盘名（净化/去重之后，可与库中条目名不同）。
  final String name;
  final String status; // degraded | failed
  final String? reason;

  factory ExportReportItem.fromJson(Map<String, dynamic> json) =>
      ExportReportItem(
        idx: json['idx'] as int,
        name: json['name'] as String,
        status: json['status'] as String,
        reason: json['reason'] as String?,
      );
}

class ExportFinished {
  const ExportFinished({
    required this.exportId,
    required this.succeeded,
    required this.degraded,
    required this.failed,
    required this.canceled,
    required this.targetDir,
    required this.items,
    required this.itemsTruncated,
  });

  final int exportId;
  final int succeeded;
  final int degraded;
  final int failed;
  final bool canceled;
  final String targetDir;
  final List<ExportReportItem> items;
  final bool itemsTruncated;

  factory ExportFinished.fromJson(Map<String, dynamic> json) => ExportFinished(
    exportId: json['exportId'] as int,
    succeeded: json['succeeded'] as int,
    degraded: json['degraded'] as int,
    failed: json['failed'] as int,
    canceled: json['canceled'] as bool,
    targetDir: json['targetDir'] as String,
    items: (json['items'] as List)
        .map((e) => ExportReportItem.fromJson(e as Map<String, dynamic>))
        .toList(),
    itemsTruncated: json['itemsTruncated'] as bool,
  );
}

class RpcException implements Exception {
  const RpcException(this.code, this.message);
  final int code;
  final String message;
  @override
  String toString() => 'RpcException($code): $message';
}

String encodeRequest({
  required Object id,
  required String method,
  Object? params,
}) => jsonEncode({
  'jsonrpc': '2.0',
  'id': id,
  'method': method,
  'params': params,
});

/// 从一条完整响应消息中取出 result；错误响应抛出 [RpcException]。
/// 输入须是本 daemon 产出的合法信封（error 为对象、result 为对象）；
/// 畸形输入会抛 [TypeError]——上层按通用异常捕获处理。
Map<String, dynamic> decodeResult(Map<String, dynamic> message) {
  final error = message['error'];
  if (error != null) {
    final e = error as Map<String, dynamic>;
    throw RpcException(e['code'] as int, e['message'] as String);
  }
  return message['result'] as Map<String, dynamic>;
}
