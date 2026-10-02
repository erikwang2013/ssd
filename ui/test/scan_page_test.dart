// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 扫描页 widget 测试（Fake 驱动）：模式透传 / 通知+轮询对账进度 / 暂停恢复取消 /
// EACCES(-32001) 引导与特权重启 / 通知按 taskId 过滤（含 T4 实测的 "id":null 行）。
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/features/scan/scan_page.dart';

import 'fake_core_client.dart';

const _device = DeviceInfo(
  id: 'image:test.img',
  name: 'test.img',
  kind: 'image',
  sizeBytes: 4096,
  removable: false,
);

Future<FakeCoreClient> pumpScanPage(
  WidgetTester tester, {
  FakeCoreClient? client,
}) async {
  final fake = client ?? FakeCoreClient();
  await tester.pumpWidget(
    MaterialApp(
      home: ScanPage(client: fake, device: _device),
    ),
  );
  await tester.pump();
  return fake;
}

/// 扫描中挂着 1s 轮询定时器 → pumpAndSettle 永不收敛；显式泵足轮次即可。
Future<void> flush(WidgetTester tester) async {
  for (var i = 0; i < 4; i++) {
    await tester.pump();
  }
}

/// 用例收尾卸载页面：dispose 取消轮询定时器（无 pending timer 残留）。
Future<void> unload(WidgetTester tester) async {
  await tester.pumpWidget(const SizedBox());
  await tester.pump();
}

double? progressValue(WidgetTester tester) => tester
    .widget<LinearProgressIndicator>(find.byType(LinearProgressIndicator))
    .value;

Map<String, dynamic> progressNotification(
  int taskId, {
  required int readBytes,
  required int foundCount,
  required int elapsedMs,
}) => {
  'jsonrpc': '2.0',
  'method': 'scan.progress',
  'params': {
    'taskId': taskId,
    'state': 'scanning',
    'readBytes': readBytes,
    'foundCount': foundCount,
    'elapsedMs': elapsedMs,
  },
};

void main() {
  testWidgets('模式透传：切「深度扫描」后 scanStart(mode:deep)', (tester) async {
    final fake = await pumpScanPage(tester);
    expect(find.text('快速扫描'), findsOneWidget);
    await tester.tap(find.text('深度扫描'));
    await tester.pump();
    expect(find.textContaining('仅雕刻'), findsOneWidget, reason: '深度模式副文案在场');
    await tester.tap(find.text('开始扫描'));
    await flush(tester);
    expect(fake.calls, contains('scanStart(image:test.img, mode:deep)'));
    await unload(tester);
  });

  testWidgets('进度：通知 30% + 1s 轮询对账；finished → 查看结果 (N)', (tester) async {
    final fake = await pumpScanPage(tester);
    fake.scanStartResult = const ScanStartResult(
      taskId: 3,
      fs: 'exfat',
      totalBytes: 10 * 1024 * 1024,
    );
    await tester.tap(find.text('开始扫描'));
    await flush(tester);

    // 通知驱动：30% + 计数
    fake.emitNotification(
      progressNotification(
        3,
        readBytes: 3 * 1024 * 1024,
        foundCount: 12,
        elapsedMs: 1500,
      ),
    );
    await flush(tester);
    expect(progressValue(tester), closeTo(0.3, 0.001));
    expect(find.text('已扫 3.0 MB / 10.0 MB'), findsOneWidget);
    expect(find.text('已找到 12 项'), findsOneWidget);
    expect(find.text('用时 1.5 s'), findsOneWidget);

    // 轮询对账：下一拍 scanStatus 报 3.5MB/13 → 以对账值为准
    fake.scanStatusResult = const ScanStatusResult(
      taskId: 3,
      state: 'scanning',
      readBytes: 3670016, // 3.5 MiB
      foundCount: 13,
      elapsedMs: 1600,
    );
    await tester.pump(const Duration(seconds: 1));
    await flush(tester);
    expect(fake.calls.where((c) => c == 'scanStatus(3)'), isNotEmpty);
    expect(progressValue(tester), closeTo(0.35, 0.001));
    expect(find.text('已扫 3.5 MB / 10.0 MB'), findsOneWidget);
    expect(find.text('已找到 13 项'), findsOneWidget);

    // 终态通知（无 readBytes 字段）→ 完成态主按钮
    fake.emitNotification({
      'jsonrpc': '2.0',
      'method': 'scan.finished',
      'params': {
        'taskId': 3,
        'state': 'completed',
        'foundCount': 13,
        'elapsedMs': 1700,
      },
    });
    await flush(tester);
    expect(find.byType(LinearProgressIndicator), findsNothing);
    expect(find.text('查看结果 (13)'), findsOneWidget);

    // 导航到 T6 前的 ResultsPage 桩（taskId 透传）
    await tester.tap(find.text('查看结果 (13)'));
    await tester.pumpAndSettle(); // 终态后定时器已停，可收敛
    expect(find.text('扫描结果'), findsOneWidget);
    expect(find.text('任务 #3'), findsOneWidget);
  });

  testWidgets('暂停/恢复：按钮文案切换、调用各一次', (tester) async {
    final fake = await pumpScanPage(tester);
    await tester.tap(find.text('开始扫描'));
    await flush(tester);

    await tester.tap(find.text('暂停'));
    await flush(tester);
    expect(fake.calls, contains('scanPause(1)'));
    expect(find.text('恢复'), findsOneWidget);
    expect(find.text('暂停'), findsNothing);

    await tester.tap(find.text('恢复'));
    await flush(tester);
    expect(fake.calls, contains('scanResume(1)'));
    expect(find.text('暂停'), findsOneWidget);
    expect(fake.calls.where((c) => c.startsWith('scanPause')).length, 1);
    expect(fake.calls.where((c) => c.startsWith('scanResume')).length, 1);
    await unload(tester);
  });

  testWidgets('取消：二次确认后 scanCancel、状态 canceled', (tester) async {
    final fake = await pumpScanPage(tester);
    await tester.tap(find.text('开始扫描'));
    await flush(tester);

    await tester.tap(find.text('取消'));
    await flush(tester);
    expect(find.text('取消扫描？'), findsOneWidget, reason: '取消需二次确认');

    await tester.tap(find.text('取消扫描'));
    await flush(tester);
    expect(fake.calls, contains('scanCancel(1)'));
    expect(find.text('扫描已取消'), findsOneWidget);
    expect(find.text('重新扫描'), findsOneWidget);
  });

  testWidgets('-32001：EACCES 对话框 → 特权重启 → 重试 scanStart', (tester) async {
    var attempts = 0;
    final fake = FakeCoreClient(
      failWith: (method) => method == 'scanStart' && attempts++ == 0
          ? const RpcException(
              -32001,
              'Device permission denied: image:test.img',
            )
          : null,
    );
    await pumpScanPage(tester, client: fake);
    await tester.tap(find.text('开始扫描'));
    await flush(tester);
    expect(find.text('需要管理员权限访问该设备'), findsOneWidget);

    await tester.tap(find.text('授权后重试'));
    await flush(tester);
    expect(fake.calls, contains('restartPrivileged()'));
    expect(fake.calls.where((c) => c.startsWith('scanStart')).length, 2);
    expect(find.text('暂停'), findsOneWidget, reason: '重试成功进入扫描中');
    await unload(tester);
  });

  testWidgets('其他 taskId 的通知与 method==null 行不得影响本页', (tester) async {
    final fake = await pumpScanPage(tester);
    await tester.tap(find.text('开始扫描'));
    await flush(tester);

    fake.emitNotification(
      progressNotification(
        999,
        readBytes: 5 * 1024 * 1024,
        foundCount: 99,
        elapsedMs: 9000,
      ),
    );
    // T4 实测：契约片段里 "id":null 的应答行会进通知流 → 分发器必须容忍 method==null
    fake.emitNotification({'jsonrpc': '2.0', 'id': null});
    // 畸形通知（无 params / 无 taskId）同样不得打断
    fake.emitNotification({'jsonrpc': '2.0', 'method': 'scan.finished'});
    fake.emitNotification({
      'jsonrpc': '2.0',
      'method': 'scan.progress',
      'params': {'readBytes': 1},
    });
    await flush(tester);

    expect(find.text('已找到 99 项'), findsNothing);
    expect(find.text('已找到 0 项'), findsOneWidget);
    expect(
      find.text('已扫 0.0 B'),
      findsOneWidget,
      reason: 'totalBytes 缺省 0 → 不定进度条（文本不带总量）',
    );
    expect(progressValue(tester), isNull);
    await unload(tester);
  });
}
