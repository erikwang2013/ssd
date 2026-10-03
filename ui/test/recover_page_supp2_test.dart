// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 恢复页补测 B（qual-m1d-t8 交付）：渲染/文案/计数/摘要/边界。
// 助手（FakeDirSelector、progress/finished、flush/pumpPage/unload/countOf/
// selectAndStart）复用 recover_page_supp_test.dart。
import 'package:file_selector_platform_interface/file_selector_platform_interface.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/features/recover/recover_page.dart';
import 'package:xiaodun_ui/util/format.dart';

import 'fake_core_client.dart';
import 'recover_page_supp_test.dart';

void main() {
  setUp(() {
    sel = FakeDirSelector();
    FileSelectorPlatform.instance = sel;
  });

  testWidgets('total=0 → 不定态（不除零）；随后正常进度', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 2,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification(progress(42, 0, 0, 0));
    await flush(tester);
    expect(
      tester
          .widget<LinearProgressIndicator>(find.byType(LinearProgressIndicator))
          .value,
      isNull,
    );
    expect(find.text('正在启动…'), findsOneWidget);
    fake.emitNotification(progress(42, 2, 2, 900));
    await flush(tester);
    expect(find.text('已完成 2/2 · 900.0 B'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('畸形 progress 通知 → 忽略且不炸；随后合法进度照常', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 1,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification({
      'jsonrpc': '2.0',
      'method': 'export.progress',
      'params': {'exportId': 42, 'done': 'x', 'total': 2, 'writtenBytes': 0},
    });
    await flush(tester);
    expect(tester.takeException(), isNull);
    fake.emitNotification(progress(42, 1, 2, 100));
    await flush(tester);
    expect(find.text('已完成 1/2 · 100.0 B'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('三计数可区分（3/2/1 各自落位；图标按 status 分派）', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 6,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification(
      finished(
        exportId: 42,
        succeeded: 3,
        degraded: 2,
        failed: 1,
        items: const [
          {'idx': 1, 'name': 'f1', 'status': 'failed', 'reason': 'io'},
          {'idx': 2, 'name': 'd1', 'status': 'degraded', 'reason': 'short'},
        ],
      ),
    );
    await flush(tester);
    expect(countOf(tester, 'count-succeeded'), '3');
    expect(countOf(tester, 'count-degraded'), '2');
    expect(countOf(tester, 'count-failed'), '1');
    expect(find.byIcon(Icons.error_outline), findsOneWidget);
    expect(find.byIcon(Icons.warning_amber_rounded), findsOneWidget);
    await unload(tester);
  });

  testWidgets('契约外 status=succeeded 的 item → 降级样式兜底（不崩）；reason 空客不显', (
    tester,
  ) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 1,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification(
      finished(
        exportId: 42,
        succeeded: 1,
        items: const [
          {'idx': 1, 'name': 'a.jpg', 'status': 'succeeded', 'reason': null},
        ],
      ),
    );
    await flush(tester);
    expect(find.text('a.jpg'), findsOneWidget);
    expect(find.byIcon(Icons.warning_amber_rounded), findsOneWidget);
    await unload(tester);
  });

  testWidgets('itemsTruncated=true → 提示行；false → 无提示', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 1,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification(finished(exportId: 42, succeeded: 1));
    await flush(tester);
    expect(find.textContaining('清单过长'), findsNothing);
    await tester.pumpWidget(const SizedBox());
    final fake2 = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 1,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake2);
    await selectAndStart(tester, '/tmp/rec');
    fake2.emitNotification(
      finished(exportId: 42, succeeded: 1000, itemsTruncated: true),
    );
    await flush(tester);
    expect(find.textContaining('清单过长'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('canceled → 标题「已取消」+ 副文案；计数保留', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 1,
        estimatedBytes: 10,
      );
    await pumpPage(tester, fake);
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification(finished(exportId: 42, canceled: true, succeeded: 1));
    await flush(tester);
    expect(find.text('已取消'), findsOneWidget);
    expect(find.text('已完成的文件保留在目标文件夹'), findsOneWidget);
    expect(countOf(tester, 'count-succeeded'), '1');
    await unload(tester);
  });

  testWidgets('摘要卡片：源任务/已选逐字；未启动无「预计」', (tester) async {
    final fake = FakeCoreClient();
    await pumpPage(tester, fake);
    expect(find.text('源任务 #9 · 已选 2 项'), findsOneWidget);
    expect(find.textContaining('预计'), findsNothing);
    await selectAndStart(tester, '/tmp/rec');
    expect(find.textContaining('预计'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('报告「完成」→ 页面 pop', (tester) async {
    final fake = FakeCoreClient()
      ..exportStartResult = const ExportStartResult(
        exportId: 42,
        fileCount: 1,
        estimatedBytes: 10,
      );
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => Scaffold(
            body: Center(
              child: ElevatedButton(
                onPressed: () => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (_) =>
                        RecoverPage(client: fake, taskId: 9, idxs: const [2]),
                  ),
                ),
                child: const Text('进入'),
              ),
            ),
          ),
        ),
      ),
    );
    await tester.tap(find.text('进入'));
    await tester.pumpAndSettle();
    await selectAndStart(tester, '/tmp/rec');
    fake.emitNotification(finished(exportId: 42, succeeded: 1));
    await flush(tester);
    expect(find.text('恢复完成'), findsOneWidget);
    await tester.tap(find.text('完成'));
    await tester.pumpAndSettle();
    expect(find.text('恢复完成'), findsNothing, reason: '「完成」应返回上一页');
    await unload(tester);
  });

  test('formatBytes 进位边界（恢复页预计大小/进度共用）', () {
    expect(formatBytes(0), '0.0 B');
    expect(formatBytes(1023), '1023.0 B');
    expect(formatBytes(1024), '1.0 KB');
    expect(formatBytes(1048575), '1024.0 KB');
    expect(formatBytes(1048576), '1.0 MB');
    expect(formatBytes(1099511627776), '1.0 TB');
  });
}
