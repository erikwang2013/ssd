// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// qual-m1d-t5 补测（封 NO-KILL 缺口）：终态先到先得 / 轮询兜底 / deep 保持 /
// restart 为 null / 错误文案映射 / percent 封顶 / 对话框取消 / 在途对账过期。
import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/core_client.dart';
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

Future<void> flush(WidgetTester tester) async {
  for (var i = 0; i < 4; i++) {
    await tester.pump();
  }
}

Future<void> unload(WidgetTester tester) async {
  await tester.pumpWidget(const SizedBox());
  await tester.pump();
}

Map<String, dynamic> progressNotification(
  int taskId, {
  required int readBytes,
  required int foundCount,
  required int elapsedMs,
  String state = 'scanning',
}) => {
  'jsonrpc': '2.0',
  'method': 'scan.progress',
  'params': {
    'taskId': taskId,
    'state': state,
    'readBytes': readBytes,
    'foundCount': foundCount,
    'elapsedMs': elapsedMs,
  },
};

Future<FakeCoreClient> pumpPage(WidgetTester tester, CoreClient fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: ScanPage(client: fake, device: _device),
    ),
  );
  await tester.pump();
  return fake as FakeCoreClient;
}

/// scanStatus 挂在 Completer 上——模拟「对账请求在途」窗口。
class GatedStatusFake extends FakeCoreClient {
  GatedStatusFake({super.failWith});

  Completer<ScanStatusResult>? gate;

  @override
  Future<ScanStatusResult> scanStatus(int taskId) {
    calls.add('scanStatus($taskId)');
    return gate!.future;
  }
}

void main() {
  testWidgets('A 终态先到先得：完成后迟到的 scan.progress 不得回退状态', (tester) async {
    final fake = await pumpPage(tester, FakeCoreClient());
    fake.scanStartResult = const ScanStartResult(
      taskId: 5,
      fs: 'exfat',
      totalBytes: 10 * 1024 * 1024,
    );
    await tester.tap(find.text('开始扫描'));
    await flush(tester);
    fake.emitNotification({
      'jsonrpc': '2.0',
      'method': 'scan.finished',
      'params': {
        'taskId': 5,
        'state': 'completed',
        'foundCount': 7,
        'elapsedMs': 2000,
      },
    });
    await flush(tester);
    expect(find.text('查看结果 (7)'), findsOneWidget);

    // 迟到的 scanning 通知（对端竞序/陈旧通知）
    fake.emitNotification(
      progressNotification(
        5,
        readBytes: 9 * 1024 * 1024,
        foundCount: 99,
        elapsedMs: 2500,
      ),
    );
    await flush(tester);
    expect(
      find.byType(LinearProgressIndicator),
      findsNothing,
      reason: '终态不可回退',
    );
    expect(find.text('查看结果 (7)'), findsOneWidget);
    expect(find.text('已找到 99 项'), findsNothing);
    await unload(tester);
  });

  testWidgets('B 轮询兜底：通知全丢，1s 轮询也能收敛到 completed', (tester) async {
    final fake = await pumpPage(tester, FakeCoreClient());
    fake.scanStartResult = const ScanStartResult(
      taskId: 5,
      fs: 'exfat',
      totalBytes: 10 * 1024 * 1024,
    );
    await tester.tap(find.text('开始扫描'));
    await flush(tester);
    fake.scanStatusResult = const ScanStatusResult(
      taskId: 5,
      state: 'completed',
      readBytes: 10 * 1024 * 1024,
      foundCount: 4,
      elapsedMs: 3000,
    );
    await tester.pump(const Duration(seconds: 1));
    await flush(tester);
    expect(find.text('查看结果 (4)'), findsOneWidget);
    expect(find.byType(LinearProgressIndicator), findsNothing);
    await unload(tester);
  });

  testWidgets('C EACCES 重试保持 deep 模式（两次 scanStart 均为 mode:deep）', (
    tester,
  ) async {
    var attempts = 0;
    final fake = FakeCoreClient(
      failWith: (method) => method == 'scanStart' && attempts++ == 0
          ? const RpcException(
              -32001,
              'Device permission denied: image:test.img',
            )
          : null,
    );
    await pumpPage(tester, fake);
    await tester.tap(find.text('深度扫描'));
    await tester.pump();
    await tester.tap(find.text('开始扫描'));
    await flush(tester);
    await tester.tap(find.text('授权后重试'));
    await flush(tester);
    expect(
      fake.calls
          .where((c) => c == 'scanStart(image:test.img, mode:deep)')
          .length,
      2,
      reason: '重试必须沿用 deep，不得回落 quick',
    );
    await unload(tester);
  });

  testWidgets('D restartPrivileged 返回 null → failed + 可重扫，且不再调 scanStart', (
    tester,
  ) async {
    var attempts = 0;
    final fake = FakeCoreClient(
      failWith: (method) => method == 'scanStart' && attempts++ == 0
          ? const RpcException(
              -32001,
              'Device permission denied: image:test.img',
            )
          : null,
    )..restartReturnsNull = true;
    await pumpPage(tester, fake);
    await tester.tap(find.text('开始扫描'));
    await flush(tester);
    await tester.tap(find.text('授权后重试'));
    await flush(tester);
    expect(find.text('扫描失败：无法以管理员权限重启核心服务'), findsOneWidget);
    expect(find.text('重新扫描'), findsOneWidget);
    expect(fake.calls.where((c) => c.startsWith('scanStart')).length, 1);
    await unload(tester);
  });

  testWidgets('E 失败文案：StateError/Timeout/RpcException 映射（契约 message 无前缀）', (
    tester,
  ) async {
    Future<String> failCopy(Object error) async {
      final fake = FakeCoreClient(
        failWith: (m) => m == 'scanStart' ? error : null,
      );
      await pumpPage(tester, fake);
      await tester.tap(find.text('开始扫描'));
      await flush(tester);
      final text = tester.widget<Text>(find.textContaining('扫描失败')).data!;
      await unload(tester);
      return text;
    }

    expect(
      await failCopy(StateError('daemon exited with code -15')),
      '扫描失败：核心服务已退出，请重启应用',
    );
    expect(await failCopy(TimeoutException('10s')), '扫描失败：核心服务无响应');
    expect(
      await failCopy(const RpcException(-32603, 'Invalid params: mode')),
      '扫描失败：Invalid params: mode',
      reason: 'RpcException 只显示契约 message（lead 裁定）',
    );
  });

  testWidgets('G EACCES 对话框[取消]：退出引导回到可重试态（不循环弹窗）', (tester) async {
    var attempts = 0;
    final fake = FakeCoreClient(
      failWith: (method) => method == 'scanStart' && attempts++ == 0
          ? const RpcException(
              -32001,
              'Device permission denied: image:test.img',
            )
          : null,
    );
    await pumpPage(tester, fake);
    await tester.tap(find.text('开始扫描'));
    await flush(tester);
    expect(find.text('需要管理员权限访问该设备'), findsOneWidget);

    await tester.tap(find.text('取消'));
    await flush(tester);
    expect(find.text('需要管理员权限访问该设备'), findsNothing, reason: '取消后不得复弹');
    expect(find.text('开始扫描'), findsOneWidget);
    // 再次开始（这次不再 -32001）→ 可进扫描中
    await tester.tap(find.text('开始扫描'));
    await flush(tester);
    expect(find.text('暂停'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('H 在途对账过期：暂停后，压在 await 前的旧 scanning 响应不得把状态拉回', (tester) async {
    final fake = GatedStatusFake();
    fake.gate = Completer<ScanStatusResult>();
    await pumpPage(tester, fake);
    await tester.tap(find.text('开始扫描'));
    await flush(tester);

    // 1s 轮询发起（scanStatus 在途，未完成）
    await tester.pump(const Duration(seconds: 1));
    expect(fake.calls.where((c) => c.startsWith('scanStatus')).length, 1);

    // 用户暂停：scanPause 立即返回 → paused
    await tester.tap(find.text('暂停'));
    await flush(tester);
    expect(find.text('恢复'), findsOneWidget);

    // 在途响应此刻才回来（server 尚未处理 pause，state=scanning）
    fake.gate!.complete(
      const ScanStatusResult(
        taskId: 1,
        state: 'scanning',
        readBytes: 1024,
        foundCount: 1,
        elapsedMs: 1100,
      ),
    );
    await flush(tester);
    expect(find.text('恢复'), findsOneWidget, reason: '过期对账不得复活 scanning');
    expect(find.text('暂停'), findsNothing);
    await unload(tester);
  });

  testWidgets('F percent 封顶：readBytes > totalBytes 时进度不超过 1.0', (tester) async {
    final fake = await pumpPage(tester, FakeCoreClient());
    fake.scanStartResult = const ScanStartResult(
      taskId: 5,
      fs: 'exfat',
      totalBytes: 10 * 1024 * 1024,
    );
    await tester.tap(find.text('开始扫描'));
    await flush(tester);
    fake.emitNotification(
      progressNotification(
        5,
        readBytes: 12 * 1024 * 1024,
        foundCount: 1,
        elapsedMs: 100,
      ),
    );
    await flush(tester);
    final value = tester
        .widget<LinearProgressIndicator>(find.byType(LinearProgressIndicator))
        .value;
    expect(value, isNotNull);
    expect(value!, lessThanOrEqualTo(1.0));
    await unload(tester);
  });
}
