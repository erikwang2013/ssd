// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// M1d v1.2 值级覆盖（T1 移交硬要求）：产品模型逐字段解码 + 四类逐字断言。
// 手法沿用 protocol_v1_test.dart（golden()/expectRpcError() 同款）；集合断言仍以 v1 文件为准。
// 传输层契约点：**无 id 行 = 通知**（进程级验证见 ipc_transport_test.dart）。
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';

Map<String, dynamic> golden(String name) =>
    jsonDecode(File('../proto/v1/examples/$name').readAsStringSync())
        as Map<String, dynamic>;

void expectRpcError(String file, int code, String message) {
  try {
    decodeResult(golden(file));
    fail('expected RpcException for $file');
  } on RpcException catch (e) {
    expect(e.code, code, reason: file);
    expect(e.message, message, reason: file);
  }
}

/// 响应 golden 的产品模型解码分发（值级断言归各专测）。
void decodeResponse(String name, Map<String, dynamic> message) {
  final result = decodeResult(message);
  switch (name) {
    case 'ping.response.json':
      expect(PingResult.fromJson(result).protocol, 1, reason: name);
    case 'device_list.response.json':
      expect(
        (result['devices'] as List)
            .map((e) => DeviceInfo.fromJson(e as Map<String, dynamic>))
            .toList(),
        hasLength(2),
        reason: name,
      );
    case 'scan_start.response.json':
      expect(ScanStartResult.fromJson(result).fs, 'exfat', reason: name);
    case 'scan_status.response.json':
      expect(ScanStatusResult.fromJson(result).readBytes, 123456, reason: name);
    case 'scan_results.response.json':
    case 'scan_results_carved.response.json':
    case 'scan_results_ntfs.response.json':
    case 'scan_results_ext4.response.json':
      expect(
        ScanResultsPage.fromJson(result).entries,
        hasLength((result['entries'] as List).length),
        reason: name,
      );
    case 'scan_pause.response.json':
      expect(result, {'taskId': 1, 'state': 'paused'}, reason: name);
    case 'scan_resume.response.json':
      expect(result, {'taskId': 1, 'state': 'scanning'}, reason: name);
    case 'scan_cancel.response.json':
      expect(result, {'taskId': 1, 'state': 'canceled'}, reason: name);
    case 'fs_read.response.json':
      expect(FsReadResult.fromJson(result).eof, isTrue, reason: name);
    case 'export_start.response.json':
      expect(ExportStartResult.fromJson(result).exportId, 1, reason: name);
    case 'export_cancel.response.json':
      expect(result, {'exportId': 1, 'state': 'canceled'}, reason: name);
    case 'daemon_shutdown.response.json': // v1.3：无产品模型（T7 才落地消费），逐字
      expect(result, {'accepted': true}, reason: name);
    default:
      fail('响应 golden 未分发：$name');
  }
}

void main() {
  test('40 golden 全解码（产品模型；请求/响应/通知三态分流）', () {
    final names = Directory(
      '../proto/v1/examples',
    ).listSync().map((e) => e.uri.pathSegments.last).toList()..sort();
    expect(names, hasLength(40), reason: '集合断言归 protocol_v1_test.dart；此处消费');
    for (final name in names) {
      final message = golden(name);
      expect(message['jsonrpc'], '2.0', reason: name);
      if (name.endsWith('.request.json')) {
        expect(message['id'], isA<int>(), reason: name);
        expect(message['method'], isA<String>(), reason: name);
        continue; // 请求侧逐字形状归下一条专测
      }
      if (message.containsKey('error')) {
        expect(
          () => decodeResult(message),
          throwsA(isA<RpcException>()),
          reason: name,
        );
        continue;
      }
      if (!message.containsKey('id')) {
        expect(message['method'], isA<String>(), reason: name);
        final params = message['params'] as Map<String, dynamic>;
        // 有产品模型的即刻解码（scan.finished 无 readBytes，形状归 v1 专测）
        switch (message['method']) {
          case 'scan.progress':
            expect(ScanStatusResult.fromJson(params).taskId, 1, reason: name);
          case 'export.finished':
            expect(ExportFinished.fromJson(params).exportId, 1, reason: name);
        }
        continue;
      }
      decodeResponse(name, message);
    }
  });

  test('scan_results_carved：雕刻条目值级 + displayName 与落盘命名一致', () {
    final result = decodeResult(golden('scan_results_carved.response.json'));
    final page = ScanResultsPage.fromJson(result);
    expect(page.total, 2);
    expect(page.entries, hasLength(2));
    final carved = page.entries[0];
    expect(carved.idx, 0);
    expect(carved.name, isEmpty);
    expect(carved.path, isEmpty);
    expect(carved.ext, 'jpg');
    expect(carved.sizeBytes, 24410);
    expect(carved.deleted, isTrue);
    expect(carved.isDir, isFalse);
    expect(carved.quality, 'carved');
    expect(carved.firstCluster, 0);
    expect(carved.byteOffset, 835584);
    expect(
      carved.contiguous,
      isNull,
      reason: 'golden 无 contiguous = 未知（不得宣称连续）',
    );
    expect(carved.displayName, 'carved_000000.jpg');
    expect(page.entries[1].byteOffset, 892928);
    expect(page.entries[1].displayName, 'carved_000001.png');
    expect(
      page.entries.map((e) => e.toJson()).toList(),
      result['entries'],
      reason: '编码方向同形（null 缺省省略键）',
    );
  });

  test('ScanEntry：FS 条目原名/byteOffset 缺省 null；空 ext 回退 bin', () {
    final fs = ScanEntry.fromJson({
      'idx': 3,
      'name': 'IMG_0001.JPG',
      'path': '/DCIM',
      'ext': 'jpg',
      'sizeBytes': 12000,
      'deleted': true,
      'isDir': false,
      'quality': 'complete',
      'firstCluster': 6,
    });
    expect(fs.byteOffset, isNull, reason: '缺省 = null（v1.1 前旧客户端/FS 条目）');
    expect(fs.displayName, 'IMG_0001.JPG');
    expect(fs.toJson().containsKey('byteOffset'), isFalse);
    const carvedNoExt = ScanEntry(
      idx: 42,
      name: '',
      path: '',
      ext: '',
      sizeBytes: 1,
      deleted: true,
      isDir: false,
      quality: 'carved',
      firstCluster: 0,
      byteOffset: 0,
    );
    expect(carvedNoExt.displayName, 'carved_000042.bin');
  });

  test('contiguous 三态：缺省 null / true / false 原样保留', () {
    Map<String, dynamic> entryJson(Object? contiguous) => {
      'idx': 1,
      'name': 'A.TXT',
      'path': '/',
      'ext': 'txt',
      'sizeBytes': 4,
      'deleted': true,
      'isDir': false,
      'quality': 'complete',
      'firstCluster': 6,
      'contiguous': ?contiguous,
    };
    expect(ScanEntry.fromJson(entryJson(null)).contiguous, isNull);
    expect(ScanEntry.fromJson(entryJson(true)).contiguous, isTrue);
    final noChain = ScanEntry.fromJson(entryJson(true));
    expect(noChain.toJson()['contiguous'], isTrue);
    expect(ScanEntry.fromJson(entryJson(false)).contiguous, isFalse);
    expect(
      ScanEntry.fromJson(entryJson(null)).toJson().containsKey('contiguous'),
      isFalse,
      reason: '序列化省略 null（与 Rust skip_serializing_if 同形）',
    );
  });

  test('v1.2 请求 golden：encodeRequest 逐字', () {
    expect(
      jsonDecode(
        encodeRequest(
          id: 21,
          method: 'fs.read',
          params: {'taskId': 1, 'idx': 0, 'offset': 0, 'length': 16},
        ),
      ),
      golden('fs_read.request.json'),
    );
    expect(
      jsonDecode(
        encodeRequest(
          id: 22,
          method: 'export.start',
          params: {
            'taskId': 1,
            'idxs': [0, 1],
            'targetDir': '/home/user/Recovered',
          },
        ),
      ),
      golden('export_start.request.json'),
    );
    expect(
      jsonDecode(
        encodeRequest(id: 23, method: 'export.cancel', params: {'exportId': 1}),
      ),
      golden('export_cancel.request.json'),
    );
  });

  test('fs_read.response：bytesBase64 值级解码 + eof（15 字节真值）', () {
    final result = decodeResult(golden('fs_read.response.json'));
    expect(result['bytesBase64'], 'aGVsbG8sIHhpYW9kdW4h');
    final read = FsReadResult.fromJson(result);
    expect(read.bytes, hasLength(15));
    expect(utf8.decode(read.bytes), 'hello, xiaodun!');
    expect(read.eof, isTrue);
  });

  test('export_start/export_cancel 响应：字段逐字段', () {
    final start = ExportStartResult.fromJson(
      decodeResult(golden('export_start.response.json')),
    );
    expect(start.exportId, 1);
    expect(start.fileCount, 2);
    expect(start.estimatedBytes, 16007);
    expect(decodeResult(golden('export_cancel.response.json')), {
      'exportId': 1,
      'state': 'canceled',
    });
  });

  test('export.progress 通知：无 id 信封 + params 形状', () {
    final n = golden('export_progress.notification.json');
    expect(n.containsKey('id'), isFalse, reason: '通知无 id（传输层按此分流）');
    expect(n['jsonrpc'], '2.0');
    expect(n['method'], 'export.progress');
    expect(n['params'], {
      'exportId': 1,
      'done': 1,
      'total': 2,
      'writtenBytes': 12000,
      'elapsedMs': 300,
    });
  });

  test('export.finished 通知：无 id 信封 + params/items 逐字段', () {
    final n = golden('export_finished.notification.json');
    expect(n.containsKey('id'), isFalse, reason: '通知无 id（传输层按此分流）');
    expect(n['jsonrpc'], '2.0');
    expect(n['method'], 'export.finished');
    final finished = ExportFinished.fromJson(
      n['params'] as Map<String, dynamic>,
    );
    expect(finished.exportId, 1);
    expect(finished.succeeded, 1);
    expect(finished.degraded, 1);
    expect(finished.failed, 0);
    expect(finished.canceled, isFalse);
    expect(finished.targetDir, '/home/user/Recovered');
    expect(finished.itemsTruncated, isFalse);
    expect(finished.items, hasLength(1));
    final item = finished.items.single;
    expect(item.idx, 0);
    expect(item.name, 'IMG_0001.JPG');
    expect(item.status, 'degraded');
    expect(item.reason, 'short read');
  });

  test('M1d 五错误码 -32006..-32010：文案逐字', () {
    expectRpcError(
      'error_target_on_source.response.json',
      -32006,
      'Target is on the source device: /mnt/usb/Recovered',
    );
    expectRpcError(
      'error_target_not_writable.response.json',
      -32007,
      'Target not writable: /root/nope',
    );
    expectRpcError(
      'error_entry_not_found.response.json',
      -32008,
      'Entry not found: 999',
    );
    expectRpcError(
      'error_entry_too_large.response.json',
      -32009,
      'Entry too large: 1073741824',
    );
    expectRpcError(
      'error_insufficient_space.response.json',
      -32010,
      'Insufficient space on target: need 16007 bytes',
    );
  });
}
