// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 恢复页 widget 测试（Fake 驱动；目录选择走 FileSelectorPlatform.instance 注入的 fake——
// 平台插件在 widget 测试直调会挂）：
// 1) 目标选择与启动参数（未选禁用 / 选后启用 + remount 提示 / exportStart 参数逐项）；
// 2) 进度与报告（progress 1/2 → 50% + 文案；finished 三计数与清单逐字，落盘名用
//    ExportReportItem.name；canceled → 「已取消」且计数保留；finished 抢跑响应不丢）；
// 3) 错误码映射（-32006/-32010 专用引导文案，逐字）；
// 4) 取消 → exportCancel(exportId)。
import 'dart:async';

import 'package:file_selector_platform_interface/file_selector_platform_interface.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/features/recover/recover_page.dart';

import 'fake_core_client.dart';

/// 目录选择测试缝：覆盖平台实例（顶层 getDirectoryPath → 本 fake）。
/// 只覆盖非废弃的 WithOptions 入口（file_selector 顶层函数即走这条）。
class _FakeFileSelector extends FileSelectorPlatform {
  String? dir;
  int calls = 0;

  @override
  Future<String?> getDirectoryPathWithOptions(FileDialogOptions options) async {
    calls++;
    return dir;
  }
}

late _FakeFileSelector _fakeSelector;

Map<String, dynamic> progressNotif({
  int exportId = 1,
  int done = 1,
  int total = 2,
  int writtenBytes = 12000,
}) => {
  'jsonrpc': '2.0',
  'method': 'export.progress',
  'params': {
    'exportId': exportId,
    'done': done,
    'total': total,
    'writtenBytes': writtenBytes,
    'elapsedMs': 300,
  },
};

Map<String, dynamic> finishedNotif({
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

Future<void> flush(WidgetTester tester) async {
  for (var i = 0; i < 4; i++) {
    await tester.pump();
  }
}

Future<void> unload(WidgetTester tester) async {
  await tester.pumpWidget(const SizedBox());
  await tester.pump();
}

Future<FakeCoreClient> pumpRecover(
  WidgetTester tester, {
  FakeCoreClient? client,
  int taskId = 9,
  List<int> idxs = const [2, 4],
}) async {
  final fake = client ?? FakeCoreClient();
  await tester.pumpWidget(
    MaterialApp(
      home: RecoverPage(client: fake, taskId: taskId, idxs: idxs),
    ),
  );
  await flush(tester);
  return fake;
}

/// 选目录（fake 返回 [dir]）→ 点开始 → 等 exportStart 响应落地。
Future<void> startExport(WidgetTester tester, String dir) async {
  _fakeSelector.dir = dir;
  await tester.tap(find.text('选择'));
  await flush(tester);
  await tester.tap(find.text('开始恢复'));
  await flush(tester);
}

/// exportStart 挂起直到测试显式完成（通知抢跑响应竞序用）。
class _SlowStartClient extends FakeCoreClient {
  final startCompleter = Completer<ExportStartResult>();

  @override
  Future<ExportStartResult> exportStart(
    int taskId,
    List<int> idxs,
    String targetDir,
  ) {
    exportStarts.add((
      taskId: taskId,
      idxs: List.of(idxs),
      targetDir: targetDir,
    ));
    return startCompleter.future;
  }
}

String countOf(WidgetTester tester, String key) =>
    tester.widget<Text>(find.byKey(ValueKey(key))).data!;

void main() {
  setUp(() {
    _fakeSelector = _FakeFileSelector();
    FileSelectorPlatform.instance = _fakeSelector;
  });

  testWidgets('未选目录：「开始恢复」禁用', (tester) async {
    await pumpRecover(tester);
    expect(find.text('选择目标文件夹…'), findsOneWidget);
    final start = tester.widget<FilledButton>(
      find.widgetWithText(FilledButton, '开始恢复'),
    );
    expect(start.onPressed, isNull, reason: '未选目标目录不得开始');
    await unload(tester);
  });

  testWidgets('选择目录 → 路径与 remount 提示 + 「开始恢复」启用', (tester) async {
    await pumpRecover(tester);
    _fakeSelector.dir = '/mnt/target/rec';
    expect(find.textContaining('源设备所在的盘'), findsNothing, reason: '未选目录不显示提示');

    await tester.tap(find.text('选择'));
    await flush(tester);

    expect(_fakeSelector.calls, 1);
    expect(find.text('/mnt/target/rec'), findsOneWidget);
    expect(
      find.textContaining('源设备所在的盘'),
      findsOneWidget,
      reason: 'remount 提示',
    );
    final start = tester.widget<FilledButton>(
      find.widgetWithText(FilledButton, '开始恢复'),
    );
    expect(start.onPressed, isNotNull);
    await unload(tester);
  });

  testWidgets('点开始 → exportStart(taskId, idxs, dir) 参数逐项 + 预计大小展示', (
    tester,
  ) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 2,
        estimatedBytes: 3145728,
      );
    await pumpRecover(tester, client: fake, taskId: 9, idxs: [2, 4]);
    await startExport(tester, '/tmp/rec');

    expect(fake.exportStarts.length, 1);
    expect(fake.exportStarts.single.taskId, 9);
    expect(fake.exportStarts.single.idxs, [2, 4]);
    expect(fake.exportStarts.single.targetDir, '/tmp/rec');
    expect(
      find.textContaining('预计 3.0 MB'),
      findsOneWidget,
      reason: '预估来自 estimatedBytes',
    );
    await unload(tester);
  });

  testWidgets('progress(1/2) → 进度 50% 与文案', (tester) async {
    final fake = await pumpRecover(tester);
    await startExport(tester, '/tmp/rec');

    fake.emitNotification(
      progressNotif(done: 1, total: 2, writtenBytes: 12000),
    );
    await flush(tester);

    final bar = tester.widget<LinearProgressIndicator>(
      find.byType(LinearProgressIndicator),
    );
    expect(bar.value, 0.5);
    expect(find.text('已完成 1/2 · 11.7 KB'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('finished：三计数与清单逐字（落盘名 name；成功件不在清单）', (tester) async {
    final fake = await pumpRecover(tester);
    await startExport(tester, '/tmp/rec');

    fake.emitNotification(
      finishedNotif(
        succeeded: 1,
        degraded: 1,
        items: const [
          {
            'idx': 2,
            'name': 'IMG_0002_1.JPG',
            'status': 'degraded',
            'reason': 'short read',
          },
        ],
      ),
    );
    await flush(tester);

    expect(find.text('恢复完成'), findsOneWidget);
    expect(countOf(tester, 'count-succeeded'), '1');
    expect(countOf(tester, 'count-degraded'), '1');
    expect(countOf(tester, 'count-failed'), '0');
    expect(find.text('成功'), findsOneWidget);
    expect(find.text('降级'), findsOneWidget);
    expect(find.text('失败'), findsOneWidget);
    expect(find.byType(ListTile), findsOneWidget, reason: '清单只含降级/失败件');
    expect(find.text('IMG_0002_1.JPG'), findsOneWidget, reason: '实际落盘名');
    expect(find.text('short read'), findsOneWidget, reason: 'reason 文案');
    expect(find.byIcon(Icons.warning_amber_rounded), findsOneWidget);
    expect(find.text('打开目标文件夹'), findsOneWidget);
    expect(find.text('取消恢复'), findsNothing);
    await unload(tester);
  });

  testWidgets('canceled=true → 「已取消」标题且计数保留', (tester) async {
    final fake = await pumpRecover(tester);
    await startExport(tester, '/tmp/rec');

    fake.emitNotification(
      finishedNotif(
        canceled: true,
        succeeded: 1,
        degraded: 1,
        items: const [
          {
            'idx': 4,
            'name': 'B_1.JPG',
            'status': 'degraded',
            'reason': 'short read',
          },
        ],
      ),
    );
    await flush(tester);

    expect(find.text('已取消'), findsOneWidget);
    expect(find.text('恢复完成'), findsNothing);
    expect(countOf(tester, 'count-succeeded'), '1');
    expect(countOf(tester, 'count-degraded'), '1');
    expect(countOf(tester, 'count-failed'), '0');
    expect(find.text('B_1.JPG'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('finished 抢在 exportStart 响应前到达（T3 底盘竞序）→ 报告不丢', (tester) async {
    final fake = _SlowStartClient();
    await pumpRecover(tester, client: fake);

    _fakeSelector.dir = '/tmp/rec';
    await tester.tap(find.text('选择'));
    await flush(tester);
    await tester.tap(find.text('开始恢复'));
    await flush(tester);
    expect(find.text('正在启动…'), findsOneWidget, reason: '响应未到前的在途态');

    // 通知先于响应（daemon 先起后台线程再回响应）
    fake.emitNotification(finishedNotif(succeeded: 1, targetDir: '/tmp/rec'));
    await flush(tester);
    fake.startCompleter.complete(
      const ExportStartResult(exportId: 1, fileCount: 1, estimatedBytes: 1024),
    );
    await flush(tester);

    expect(find.text('恢复完成'), findsOneWidget, reason: '抢跑通知须寄存回放，不得永久卡在导出中');
    expect(countOf(tester, 'count-succeeded'), '1');
    await unload(tester);
  });

  testWidgets('-32006 → 「目标不能是源设备所在的盘，请换一个文件夹」（RpcException 只显示译文）', (
    tester,
  ) async {
    final fake = FakeCoreClient(
      failWith: (m) => m == 'exportStart'
          ? const RpcException(-32006, 'raw daemon text')
          : null,
    );
    await pumpRecover(tester, client: fake);
    await startExport(tester, '/mnt/same-disk');

    expect(find.text('目标不能是源设备所在的盘，请换一个文件夹'), findsOneWidget);
    expect(find.textContaining('raw daemon text'), findsNothing);
    final start = tester.widget<FilledButton>(
      find.widgetWithText(FilledButton, '开始恢复'),
    );
    expect(start.onPressed, isNotNull, reason: '换目标后可重试');
    await unload(tester);
  });

  testWidgets('-32010 → 「目标盘剩余空间不足」', (tester) async {
    final fake = FakeCoreClient(
      failWith: (m) =>
          m == 'exportStart' ? const RpcException(-32010, 'no space') : null,
    );
    await pumpRecover(tester, client: fake);
    await startExport(tester, '/tmp/rec');

    expect(find.text('目标盘剩余空间不足'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('取消恢复 → exportCancel(exportId)', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 2,
        estimatedBytes: 2048,
      );
    await pumpRecover(tester, client: fake);
    await startExport(tester, '/tmp/rec');

    await tester.tap(find.text('取消恢复'));
    await flush(tester);

    expect(fake.canceledExports, [42]);
    await unload(tester);
  });
}
