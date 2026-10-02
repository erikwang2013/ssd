// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 契约 v1 golden（Dart 侧）：只钉 JSON 形态，不实现行为。
// 传输层契约点：**无 id 行 = 通知**（JSON-RPC 2.0 notification），由 IpcCoreClient 按 method
// 分发；通知路由的实现归 M1d 的 IpcTransport，本文件仅断言通知信封与 params 形状。
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';

Map<String, dynamic> golden(String name) =>
    jsonDecode(File('../proto/v1/examples/$name').readAsStringSync())
        as Map<String, dynamic>;

// —— v1 最小模型（仅契约钉形用；产品侧模型归 M1d）——
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
  });

  final int idx;
  final String name;
  final String path;
  final String ext;
  final int sizeBytes;
  final bool deleted;
  final bool isDir;
  final String quality; // complete | maybeDamaged
  final int firstCluster;

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
  };
}

class ScanProgress {
  const ScanProgress({
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

  factory ScanProgress.fromJson(Map<String, dynamic> json) => ScanProgress(
    taskId: json['taskId'] as int,
    state: json['state'] as String,
    readBytes: json['readBytes'] as int,
    foundCount: json['foundCount'] as int,
    elapsedMs: json['elapsedMs'] as int,
  );

  Map<String, dynamic> toJson() => {
    'taskId': taskId,
    'state': state,
    'readBytes': readBytes,
    'foundCount': foundCount,
    'elapsedMs': elapsedMs,
  };
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

void expectRpcError(String file, int code, String message) {
  try {
    decodeResult(golden(file));
    fail('expected RpcException for $file');
  } on RpcException catch (e) {
    expect(e.code, code, reason: file);
    expect(e.message, message, reason: file);
  }
}

void main() {
  test('golden set is exactly the 36 contract files', () {
    // 与 Rust 侧 golden_set_is_exactly_the_36_contract_files 对称：
    // 新增/删除契约文件必须同步两侧测试（flutter test 的 cwd 为 ui/）。
    final names = Directory(
      '../proto/v1/examples',
    ).listSync().map((e) => e.uri.pathSegments.last).toList()..sort();
    final expected = [
      'device_list.request.json',
      'device_list.response.json',
      'error_device_permission.response.json',
      'error_entry_not_found.response.json',
      'error_entry_too_large.response.json',
      'error_insufficient_space.response.json',
      'error_target_not_writable.response.json',
      'error_target_on_source.response.json',
      'error_task_not_active.response.json',
      'error_unallocated_unavailable.response.json',
      'error_unsupported_fs.response.json',
      'export_cancel.request.json',
      'export_cancel.response.json',
      'export_finished.notification.json',
      'export_progress.notification.json',
      'export_start.request.json',
      'export_start.response.json',
      'fs_read.request.json',
      'fs_read.response.json',
      'ping.request.json',
      'ping.response.json',
      'scan_cancel.request.json',
      'scan_cancel.response.json',
      'scan_finished.notification.json',
      'scan_pause.request.json',
      'scan_pause.response.json',
      'scan_progress.notification.json',
      'scan_results.request.json',
      'scan_results.response.json',
      'scan_results_carved.response.json',
      'scan_resume.request.json',
      'scan_resume.response.json',
      'scan_start.request.json',
      'scan_start.response.json',
      'scan_status.request.json',
      'scan_status.response.json',
    ]..sort();
    expect(names, expected);
  });

  test('ping request/response goldens decode (protocol=1)', () {
    expect(
      jsonDecode(encodeRequest(id: 1, method: 'ping', params: null)),
      golden('ping.request.json'),
    );
    final ping = PingResult.fromJson(
      decodeResult(golden('ping.response.json')),
    );
    expect(ping.pong, isTrue);
    expect(ping.version, isNotEmpty); // 值随发版变动（"<VERSION>" 占位归 Rust 侧归一）
    expect(ping.protocol, 1); // 协议号直接取自 v1 golden，与 UI 常量解耦
  });

  test('device_list request/response goldens decode (transport)', () {
    expect(
      jsonDecode(encodeRequest(id: 2, method: 'device.list', params: null)),
      golden('device_list.request.json'),
    );
    final devices =
        (decodeResult(golden('device_list.response.json'))['devices'] as List)
            .cast<Map<String, dynamic>>();
    expect(devices, hasLength(2));
    final image = DeviceInfo.fromJson(devices[0]);
    expect(image.kind, 'image');
    expect(image.toJson(), devices[0]); // 镜像无 transport 键（缺失=未知）
    expect(devices[1]['transport'], 'usb'); // 物理设备 transport 契约值
    expect(DeviceInfo.fromJson(devices[1]).name, 'USB Disk');
  });

  test('scan request goldens: method/params verbatim', () {
    final expected = {
      'scan_start.request.json': [
        'scan.start',
        {'device': 'unix:/dev/sdb', 'mode': 'quick'},
      ],
      'scan_status.request.json': [
        'scan.status',
        {'taskId': 1},
      ],
      'scan_results.request.json': [
        'scan.results',
        {'taskId': 1, 'offset': 0, 'limit': 2, 'deletedOnly': false},
      ],
      'scan_pause.request.json': [
        'scan.pause',
        {'taskId': 1},
      ],
      'scan_resume.request.json': [
        'scan.resume',
        {'taskId': 1},
      ],
      'scan_cancel.request.json': [
        'scan.cancel',
        {'taskId': 1},
      ],
    };
    expected.forEach((file, expectedMethod) {
      final msg = golden(file);
      expect(msg['jsonrpc'], '2.0');
      expect(msg['method'], expectedMethod[0], reason: file);
      expect(msg['params'], expectedMethod[1], reason: file);
    });
  });

  test('scan_start response golden decodes', () {
    final start = ScanStartResult.fromJson(
      decodeResult(golden('scan_start.response.json')),
    );
    expect(start.taskId, 1);
    expect(start.fs, 'exfat');
    expect(start.totalBytes, 3907029168);
  });

  test(
    'scan_status response / scan_progress notification: ScanProgress shape',
    () {
      final status = ScanProgress.fromJson(
        decodeResult(golden('scan_status.response.json')),
      );
      expect(status.state, 'scanning');
      expect(status.readBytes, 123456);
      expect(status.foundCount, 42);
      expect(status.elapsedMs, 1500);
      expect(status.toJson(), golden('scan_status.response.json')['result']);

      final n = golden('scan_progress.notification.json');
      expect(n.containsKey('id'), isFalse, reason: '通知无 id（传输层按此分流）');
      expect(n['jsonrpc'], '2.0');
      expect(n['method'], 'scan.progress');
      final p = ScanProgress.fromJson(n['params'] as Map<String, dynamic>);
      expect(p.toJson(), n['params']);
    },
  );

  test('scan_finished notification golden decodes', () {
    final n = golden('scan_finished.notification.json');
    expect(n.containsKey('id'), isFalse, reason: '通知无 id（传输层按此分流）');
    expect(n['method'], 'scan.finished');
    final p = n['params'] as Map<String, dynamic>;
    expect(p['state'], 'completed');
    expect(p['taskId'], 1);
    expect(p['foundCount'], 42);
    expect(p['elapsedMs'], 5000);
  });

  test('scan_results response golden decodes (entries + total)', () {
    final result = decodeResult(golden('scan_results.response.json'));
    expect(result['total'], 42);
    final entries = (result['entries'] as List)
        .map((e) => ScanEntry.fromJson(e as Map<String, dynamic>))
        .toList();
    expect(entries, hasLength(2));
    expect(entries[0].name, 'IMG_0001.JPG');
    expect(entries[0].path, '/DCIM');
    expect(entries[0].ext, 'jpg');
    expect(entries[0].deleted, isTrue);
    expect(entries[0].quality, 'complete');
    expect(entries[0].firstCluster, 6);
    expect(entries[1].name, 'READ_ME.TXT');
    expect(entries[1].deleted, isFalse);
    expect(
      entries.map((e) => e.toJson()).toList(),
      result['entries'],
      reason: 'encode 方向同形',
    );
  });

  test('pause/resume/cancel responses pin state strings', () {
    const expected = {
      'scan_pause.response.json': 'paused',
      'scan_resume.response.json': 'scanning',
      'scan_cancel.response.json': 'canceled',
    };
    expected.forEach((file, state) {
      final result = decodeResult(golden(file));
      expect(result['taskId'], 1, reason: file);
      expect(result['state'], state, reason: file);
    });
  });

  test('error goldens throw RpcException with contract codes/messages', () {
    expectRpcError(
      'error_device_permission.response.json',
      -32001,
      'Device permission denied: unix:/dev/sdb',
    );
    expectRpcError(
      'error_unsupported_fs.response.json',
      -32002,
      'Unsupported file system',
    );
    expectRpcError(
      'error_task_not_active.response.json',
      -32004,
      'Task not active: 1',
    );
    expectRpcError(
      'error_unallocated_unavailable.response.json',
      -32005,
      'Cannot determine free space',
    );
  });
}
