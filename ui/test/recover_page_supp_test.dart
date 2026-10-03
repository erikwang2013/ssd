// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 恢复页补测 A（qual-m1d-t8 交付）：重开路径（O1 修复回归网）/ 畸形回放（O3）/
// exportId 双检与寄存边界 / 取消前置 / 重入与重试 / dispose 生命周期。
// 前置：recover_controller.fixed.dart 已落（重开与畸形回放三枚与其绑定，未落则红）。
// 渲染/文案/边界补测见 recover_page_supp2_test.dart（复用本文件助手）。
import 'dart:async';

import 'package:file_selector_platform_interface/file_selector_platform_interface.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/features/recover/recover_page.dart';

import 'fake_core_client.dart';

/// 目录选择测试缝：覆盖平台实例（顶层 getDirectoryPath → 本 fake）。
class FakeDirSelector extends FileSelectorPlatform {
  String? dir;
  @override
  Future<String?> getDirectoryPathWithOptions(
    FileDialogOptions options,
  ) async => dir;
}

/// 每次 exportStart 挂起一个新 completer（重开 = 二次启动；抢跑竞序需卡响应）。
class _SeqSlowStart extends FakeCoreClient {
  final completers = <Completer<ExportStartResult>>[];
  @override
  Future<ExportStartResult> exportStart(int t, List<int> i, String d) {
    exportStarts.add((taskId: t, idxs: List.of(i), targetDir: d));
    final c = Completer<ExportStartResult>();
    completers.add(c);
    return c.future;
  }
}

/// 订阅取消探针：自定义 broadcast 源，onCancel 打旗（dispose → sub.cancel 生效）。
class _SubProbeClient extends FakeCoreClient {
  final controller = StreamController<Map<String, dynamic>>.broadcast();
  bool canceled = false;
  _SubProbeClient() {
    controller.onCancel = () => canceled = true;
  }

  @override
  Stream<Map<String, dynamic>> get notifications => controller.stream;
}

Map<String, dynamic> progress(int exportId, int done, int total, int written) =>
    {
      'jsonrpc': '2.0',
      'method': 'export.progress',
      'params': {
        'exportId': exportId,
        'done': done,
        'total': total,
        'writtenBytes': written,
        'elapsedMs': 1,
      },
    };

Map<String, dynamic> finished({
  int exportId = 1,
  int succeeded = 0,
  int degraded = 0,
  int failed = 0,
  bool canceled = false,
  String targetDir = '/tmp/rec',
  List<Map<String, dynamic>> items = const [],
  bool itemsTruncated = false,
}) => {
  'jsonrpc': '2.0',
  'method': 'export.finished',
  'params': {
    'exportId': exportId,
    'succeeded': succeeded,
    'degraded': degraded,
    'failed': failed,
    'canceled': canceled,
    'targetDir': targetDir,
    'items': items,
    'itemsTruncated': itemsTruncated,
  },
};

late FakeDirSelector sel;

Future<void> flush(WidgetTester t) async {
  for (var i = 0; i < 4; i++) {
    await t.pump();
  }
}

Future<void> pumpPage(WidgetTester t, FakeCoreClient c) async {
  await t.pumpWidget(
    MaterialApp(
      home: RecoverPage(client: c, taskId: 9, idxs: [2, 4]),
    ),
  );
  await flush(t);
}

Future<void> unload(WidgetTester t) async {
  await t.pumpWidget(const SizedBox());
  await t.pump();
}

String countOf(WidgetTester t, String key) =>
    t.widget<Text>(find.byKey(ValueKey(key))).data!;

Future<void> selectAndStart(WidgetTester t, String dir) async {
  sel.dir = dir;
  await t.tap(find.text('选择'));
  await flush(t);
  await t.tap(find.text('开始恢复'));
  await flush(t);
}

void main() {
  setUp(() {
    sel = FakeDirSelector();
    FileSelectorPlatform.instance = sel;
  });

  // ---- 重开路径（O1 修复回归网）------------------------------------------------

  testWidgets('重开：新导出的抢跑 finished 寄存回放不丢（旧 id 不得屏蔽）', (tester) async {
    final fake = _SeqSlowStart();
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.completers[0].complete(
      const ExportStartResult(exportId: 42, fileCount: 1, estimatedBytes: 10),
    );
    await flush(tester);
    fake.emitNotification(finished(exportId: 42, succeeded: 1));
    await flush(tester);
    expect(find.text('恢复完成'), findsOneWidget);

    // done 态「选择」→ picking → 二次导出（63）
    sel.dir = '/tmp/other';
    await tester.tap(find.text('选择'));
    await flush(tester);
    await tester.tap(find.text('开始恢复'));
    await flush(tester);
    expect(find.text('正在启动…'), findsOneWidget);
    final cancelBtn = tester.widget<OutlinedButton>(
      find.widgetWithText(OutlinedButton, '取消恢复'),
    );
    expect(cancelBtn.onPressed, isNull, reason: '响应未到不得把取消指到旧导出');

    fake.emitNotification(finished(exportId: 63, succeeded: 5)); // 抢跑响应
    await flush(tester);
    fake.completers[1].complete(
      const ExportStartResult(exportId: 63, fileCount: 1, estimatedBytes: 10),
    );
    await flush(tester);
    expect(find.text('恢复完成'), findsOneWidget, reason: '抢跑 finished 须寄存回放');
    expect(countOf(tester, 'count-succeeded'), '5');
    await unload(tester);
  });

  testWidgets('重开：计数从零起（旧导出进度不残留）', (tester) async {
    final fake = _SeqSlowStart();
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.completers[0].complete(
      const ExportStartResult(exportId: 42, fileCount: 2, estimatedBytes: 10),
    );
    await flush(tester);
    fake.emitNotification(progress(42, 1, 2, 12000));
    await flush(tester);
    fake.emitNotification(finished(exportId: 42, succeeded: 2));
    await flush(tester);
    expect(find.text('恢复完成'), findsOneWidget);

    sel.dir = '/tmp/other';
    await tester.tap(find.text('选择'));
    await flush(tester);
    await tester.tap(find.text('开始恢复'));
    await flush(tester);
    fake.completers[1].complete(
      const ExportStartResult(exportId: 63, fileCount: 9, estimatedBytes: 99),
    );
    await flush(tester);
    expect(find.text('已完成 1/2 · 11.7 KB'), findsNothing, reason: '不得残留旧计数');
    expect(find.text('正在启动…'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('畸形 finished 寄存回放 → 忽略（不得把在跑的导出误判 failed）', (tester) async {
    final fake = _SeqSlowStart();
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification({
      'jsonrpc': '2.0',
      'method': 'export.finished',
      'params': {'exportId': 42, 'succeeded': 1}, // 契约外：缺 items 等
    });
    await flush(tester);
    fake.completers[0].complete(
      const ExportStartResult(exportId: 42, fileCount: 1, estimatedBytes: 10),
    );
    await flush(tester);
    expect(find.text('开始恢复'), findsNothing, reason: '在跑的导出不得落入 failed');
    expect(find.text('正在启动…'), findsOneWidget);
    expect(find.textContaining('Null'), findsNothing, reason: '原始异常不得上 UI');

    fake.emitNotification(finished(exportId: 42, succeeded: 1)); // 合法终态自愈
    await flush(tester);
    expect(find.text('恢复完成'), findsOneWidget);
    await unload(tester);
  });

  // ---- exportId 双检与寄存边界 -------------------------------------------------

  testWidgets('响应后错 id finished → 忽略；本 id 到达仍收敛', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 1,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification(finished(exportId: 999, succeeded: 7));
    await flush(tester);
    expect(find.text('恢复完成'), findsNothing);
    fake.emitNotification(finished(exportId: 42, succeeded: 1));
    await flush(tester);
    expect(find.text('恢复完成'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('抢跑 finished 错 id → 回放双检拒绝；随后本 id 收敛', (tester) async {
    final fake = _SeqSlowStart();
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification(finished(exportId: 999, succeeded: 7));
    await flush(tester);
    fake.completers[0].complete(
      const ExportStartResult(exportId: 42, fileCount: 1, estimatedBytes: 10),
    );
    await flush(tester);
    expect(find.text('恢复完成'), findsNothing, reason: '回放须二次校验 exportId');
    fake.emitNotification(finished(exportId: 42, succeeded: 1));
    await flush(tester);
    expect(find.text('恢复完成'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('未开始（非在途）散落 finished → 不寄存、不污染后续启动', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 1, // 与散落通知同 id
        fileCount: 1,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake);
    fake.emitNotification(finished(exportId: 1, succeeded: 7));
    await flush(tester);
    expect(find.text('开始恢复'), findsOneWidget);
    await selectAndStart(tester, '/tmp/rec');
    expect(find.text('恢复完成'), findsNothing, reason: 'false 终态不得回放');
    expect(find.byType(LinearProgressIndicator), findsOneWidget);
    await unload(tester);
  });

  testWidgets('错 exportId 的 progress → 忽略（不得污染计数）', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 2,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification(progress(999, 5, 5, 500));
    await flush(tester);
    expect(find.textContaining('已完成'), findsNothing);
    fake.emitNotification(progress(42, 1, 2, 100));
    await flush(tester);
    expect(find.text('已完成 1/2 · 100.0 B'), findsOneWidget);
    await unload(tester);
  });

  // ---- dispose 生命周期 --------------------------------------------------------

  testWidgets('响应到达后卸载 → 通知仍到不炸（订阅取消 + _notify 守卫）', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 1,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    expect(find.byType(LinearProgressIndicator), findsOneWidget);
    await unload(tester);
    fake.emitNotification(finished(exportId: 42, succeeded: 1));
    await flush(tester);
    expect(
      tester.takeException(),
      isNull,
      reason: 'dispose 后 _notify 须为 no-op',
    );
  });

  testWidgets('启动在途卸载 → 响应/通知到达不炸（dispose 守卫）', (tester) async {
    final fake = _SeqSlowStart();
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    await unload(tester);
    fake.completers[0].complete(
      const ExportStartResult(exportId: 42, fileCount: 1, estimatedBytes: 10),
    );
    fake.emitNotification(finished(exportId: 42, succeeded: 1));
    await flush(tester);
    expect(tester.takeException(), isNull);
  });

  testWidgets('卸载 → 订阅已取消（dispose → sub.cancel 生效）', (tester) async {
    final fake = _SubProbeClient();
    await pumpPage(tester, fake);
    await unload(tester);
    expect(fake.canceled, isTrue, reason: 'dispose 须取消 notifications 订阅');
  });

  // ---- 取消前置 / 重入 / 重试 --------------------------------------------------

  testWidgets('启动响应前：取消禁用且点击无效；响应后启用', (tester) async {
    final fake = _SeqSlowStart();
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    final before = tester.widget<OutlinedButton>(
      find.widgetWithText(OutlinedButton, '取消恢复'),
    );
    expect(before.onPressed, isNull);
    await tester.tap(find.text('取消恢复'), warnIfMissed: false);
    await flush(tester);
    expect(fake.canceledExports, isEmpty, reason: '响应前无可取消对象');
    fake.completers[0].complete(
      const ExportStartResult(exportId: 42, fileCount: 1, estimatedBytes: 10),
    );
    await flush(tester);
    final after = tester.widget<OutlinedButton>(
      find.widgetWithText(OutlinedButton, '取消恢复'),
    );
    expect(after.onPressed, isNotNull);
    await unload(tester);
  });

  testWidgets('双击开始（同帧两次 onPressed）→ 仅一次 exportStart', (tester) async {
    final fake = _SeqSlowStart();
    await pumpPage(tester, fake);
    sel.dir = '/tmp/rec';
    await tester.tap(find.text('选择'));
    await flush(tester);
    await tester.tap(find.text('开始恢复'));
    await tester.tap(find.text('开始恢复'), warnIfMissed: false);
    await flush(tester);
    expect(fake.exportStarts.length, 1, reason: '重入守卫');
    await unload(tester);
  });

  testWidgets('导出中「选择」禁用（防中途换目标/二次启动）', (tester) async {
    final fake = _SeqSlowStart();
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    final btn = tester.widget<TextButton>(
      find.widgetWithText(TextButton, '选择'),
    );
    expect(btn.onPressed, isNull);
    await unload(tester);
  });

  testWidgets('启动失败后同目录重试：错误清除、回到导出态', (tester) async {
    var fail = true;
    final fake = FakeCoreClient(
      failWith: (m) => m == 'exportStart' && fail
          ? const RpcException(-32010, 'no space')
          : null,
    );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    expect(find.text('目标盘剩余空间不足'), findsOneWidget);
    fail = false;
    await tester.tap(find.text('开始恢复'));
    await flush(tester);
    expect(find.text('目标盘剩余空间不足'), findsNothing);
    expect(find.byType(LinearProgressIndicator), findsOneWidget);
    await unload(tester);
  });

  testWidgets('失败后换目录：错误清除、可重启（新目录透传）', (tester) async {
    var fail = true;
    final fake = FakeCoreClient(
      failWith: (m) =>
          m == 'exportStart' && fail ? const RpcException(-32006, 'raw') : null,
    );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/same');
    expect(find.text('目标不能是源设备所在的盘，请换一个文件夹'), findsOneWidget);
    fail = false;
    sel.dir = '/tmp/other';
    await tester.tap(find.text('选择'));
    await flush(tester);
    expect(find.text('目标不能是源设备所在的盘，请换一个文件夹'), findsNothing);
    expect(find.text('/tmp/other'), findsOneWidget);
    await tester.tap(find.text('开始恢复'));
    await flush(tester);
    expect(fake.exportStarts.last.targetDir, '/tmp/other');
    await unload(tester);
  });

  testWidgets('非 RpcException 启动失败 → describeCoreError 文案（无原始类型泄漏）', (
    tester,
  ) async {
    final fake = FakeCoreClient(
      failWith: (m) =>
          m == 'exportStart' ? StateError('daemon exited with code -15') : null,
    );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    expect(find.text('核心服务已退出，请重启应用'), findsOneWidget);
    expect(find.textContaining('Bad state'), findsNothing);
    await unload(tester);
  });
}
