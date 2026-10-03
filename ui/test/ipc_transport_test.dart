// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 传输层单元测试：用 /bin/sh 假 daemon 在进程级钉住「无 id 行 = 通知」分流、
// 应答配对与线上请求逐字（参数构造）。真 daemon 往返见 ipc_integration_test.dart。
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/ipc_transport.dart';

void main() {
  test('无 id 行进 notifications 流；带 id 行配对应答；请求参数线上逐字', () async {
    final dir = Directory.systemTemp.createTempSync('xd_transport');
    addTearDown(() => dir.deleteSync(recursive: true));
    final log = File('${dir.path}/request.jsonl');
    // 假 daemon：收一行请求 → 原样落盘 → 先发通知（无 id，readBytes=999 与应答可区分）
    // 再发应答（id=1）。
    const script = r'''
read line
echo "$line" > "$XD_FAKE_LOG"
echo '{"jsonrpc":"2.0","method":"scan.progress","params":{"taskId":1,"state":"scanning","readBytes":999,"foundCount":7,"elapsedMs":8}}'
echo '{"jsonrpc":"2.0","id":1,"result":{"taskId":1,"state":"scanning","readBytes":5,"foundCount":1,"elapsedMs":1}}'
sleep 5
''';
    final client = await IpcCoreClient.start(
      daemonPath: '/bin/sh',
      extraArgs: ['-c', script],
      environment: {'XD_FAKE_LOG': log.path},
    );
    addTearDown(client.close);
    final events = <Map<String, dynamic>>[];
    final firstEvent = Completer<void>();
    final sub = client.notifications.listen((event) {
      events.add(event);
      if (!firstEvent.isCompleted) firstEvent.complete();
    });
    addTearDown(sub.cancel);
    // 多监听者：notifications 必须 broadcast（T5 扫描页 + T8 导出页可同时在场）
    final sub2 = client.notifications.listen((_) {});
    addTearDown(sub2.cancel);

    final status = await client.scanStatus(1);
    // 通知先于应答写出（假 daemon 顺序固定）——等到达而非固定睡眠
    await firstEvent.future.timeout(const Duration(seconds: 5));

    // 应答只由带 id 行完成（通知不得污染在途请求）
    expect(status.readBytes, 5);
    // 通知按信封原样进流
    expect(events, hasLength(1));
    expect(events.single['jsonrpc'], '2.0');
    expect(events.single['method'], 'scan.progress');
    expect(events.single.containsKey('id'), isFalse);
    expect(events.single['params'], {
      'taskId': 1,
      'state': 'scanning',
      'readBytes': 999,
      'foundCount': 7,
      'elapsedMs': 8,
    });
    // 线上请求逐字（参数构造的值级锚，与 scan_status.request.json golden 同形）
    expect(jsonDecode(log.readAsStringSync()), {
      'jsonrpc': '2.0',
      'id': 1,
      'method': 'scan.status',
      'params': {'taskId': 1},
    });
    // close 语义 = 通知流 onDone（T5 进度订阅以此收尾）；double-close 幂等（tearDown 兜底）
    var done = false;
    client.notifications.listen(null, onDone: () => done = true);
    await client.close();
    expect(done, isTrue);
  }, skip: Platform.isWindows ? 'POSIX sh 假 daemon' : null);
}
