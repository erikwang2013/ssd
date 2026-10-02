// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 结果浏览页 widget 测试（Fake 驱动）：服务端分页（200/页，滚动触底触发 loadMore）/
// deletedOnly 重置分页并清选择 / 客户端质量过滤（只作用已加载集合，loadMore 增量扩大，
// 服务端 offset 取已加载数）/ 多选与导航参数 / 五组铁律文案 + 禁词 grep 断言 /
// 空态与失败重试（RpcException 只显示 message）。
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/features/preview/preview_page.dart';
import 'package:xiaodun_ui/features/recover/recover_page.dart';
import 'package:xiaodun_ui/features/results/entry_tile.dart';
import 'package:xiaodun_ui/features/results/results_page.dart';

import 'fake_core_client.dart';

ScanEntry entry({
  required int idx,
  String? name,
  String path = '/DCIM',
  String ext = 'jpg',
  int size = 1024,
  bool deleted = false,
  bool isDir = false,
  String quality = 'complete',
  bool? contiguous,
  int? byteOffset,
}) => ScanEntry(
  idx: idx,
  name: name ?? 'IMG_${idx.toString().padLeft(4, '0')}.jpg',
  path: path,
  ext: ext,
  sizeBytes: size,
  deleted: deleted,
  isDir: isDir,
  quality: quality,
  firstCluster: 2,
  byteOffset: byteOffset,
  contiguous: contiguous,
);

List<ScanEntry> manyEntries(int n) => List.generate(n, (i) => entry(idx: i));

/// 三档质量循环铺 n 条：idx%3==0 → complete，==1 → maybeDamaged，==2 → carved。
List<ScanEntry> mixedEntries(int n) => List.generate(
  n,
  (i) => entry(
    idx: i,
    quality: const ['complete', 'maybeDamaged', 'carved'][i % 3],
  ),
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

Future<FakeCoreClient> pumpResults(
  WidgetTester tester, {
  FakeCoreClient? client,
  int taskId = 7,
}) async {
  final fake = client ?? FakeCoreClient();
  await tester.pumpWidget(
    MaterialApp(
      home: ResultsPage(client: fake, taskId: taskId),
    ),
  );
  await flush(tester);
  return fake;
}

/// 滚动触底（拖拽量远超总高，位置停在 maxScrollExtent → 必越过 80% 阈值）。
Future<void> dragToBottom(WidgetTester tester) async {
  await tester.drag(find.byType(ListView), const Offset(0, -100000));
  await flush(tester);
}

void main() {
  testWidgets('分页：200/页，触底 loadMore offset 200/400，全量后不再请求', (tester) async {
    final fake = await pumpResults(
      tester,
      client: FakeCoreClient(entries: manyEntries(450)),
    );
    expect(fake.scanResultQueries.single, (
      taskId: 7,
      offset: 0,
      limit: 200,
      deletedOnly: false,
    ));
    expect(find.text('已加载 200 / 共 450 项'), findsOneWidget);

    await dragToBottom(tester);
    expect(fake.scanResultQueries.map((q) => q.offset), [0, 200]);
    expect(find.text('已加载 400 / 共 450 项'), findsOneWidget);

    await dragToBottom(tester);
    expect(fake.scanResultQueries.map((q) => q.offset), [0, 200, 400]);
    expect(find.text('已加载 450 / 共 450 项'), findsOneWidget);

    await dragToBottom(tester); // 到底展示；全量已加载不得再请求
    expect(find.text('已到底'), findsOneWidget);
    expect(fake.scanResultQueries.length, 3);
    await unload(tester);
  });

  testWidgets('loadMore 只在越过 80% 时触发：55% 处不请求、85% 处请求', (tester) async {
    final fake = await pumpResults(
      tester,
      client: FakeCoreClient(entries: manyEntries(450)),
    );
    ScrollPosition position() =>
        tester.state<ScrollableState>(find.byType(Scrollable)).position;
    final max = position().maxScrollExtent;
    expect(max, greaterThan(0), reason: '200 条必须超出视口');

    await tester.drag(find.byType(ListView), Offset(0, -max * 0.55));
    await flush(tester);
    expect(fake.scanResultQueries.length, 1, reason: '55% 未达 80% 阈值，不得预载');

    await tester.drag(find.byType(ListView), Offset(0, -max * 0.3));
    await flush(tester);
    expect(fake.scanResultQueries.map((q) => q.offset), [0, 200]);
    await unload(tester);
  });

  testWidgets('对端 total 虚高且返回空页：就地封口，不无限重拉', (tester) async {
    // 契约外场景（daemon 缺陷注入）：首屏满页但 total 虚高，触底增量拿到空页。
    final fake = FakeCoreClient(entries: manyEntries(200))..totalOverride = 450;
    await pumpResults(tester, client: fake);
    expect(find.text('已加载 200 / 共 450 项'), findsOneWidget);

    await dragToBottom(tester);
    expect(fake.scanResultQueries.map((q) => q.offset), [0, 200]);
    expect(
      find.text('已加载 200 / 共 200 项'),
      findsOneWidget,
      reason: '空页封口回真实已加载数',
    );
    expect(find.text('已到底'), findsOneWidget);

    await dragToBottom(tester);
    expect(fake.scanResultQueries.length, 2, reason: '封口后不得对同一 offset 重拉');
    await unload(tester);
  });

  testWidgets('「仅删除」走服务端过滤：重置分页 offset=0 且清选择', (tester) async {
    final fake = await pumpResults(
      tester,
      client: FakeCoreClient(
        entries: [
          for (var i = 0; i < 5; i++) entry(idx: i, name: 'live_$i.jpg'),
          for (var i = 5; i < 8; i++)
            entry(idx: i, name: 'gone_$i.jpg', deleted: true),
        ],
      ),
    );
    expect(find.text('已加载 8 / 共 8 项'), findsOneWidget);

    await tester.longPress(find.text('live_0.jpg'));
    await flush(tester);
    expect(find.text('恢复所选 (1)'), findsOneWidget);

    await tester.tap(find.widgetWithText(FilterChip, '仅删除'));
    await flush(tester);
    expect(fake.scanResultQueries.last, (
      taskId: 7,
      offset: 0,
      limit: 200,
      deletedOnly: true,
    ));
    expect(find.text('已加载 3 / 共 3 项'), findsOneWidget);
    expect(find.text('gone_5.jpg'), findsOneWidget);
    expect(find.text('live_0.jpg'), findsNothing);
    expect(
      find.textContaining('恢复所选'),
      findsNothing,
      reason: '过滤切换即清选择（多选语义显式）',
    );
    await unload(tester);
  });

  testWidgets('质量过滤：客户端只作用已加载集合，loadMore 增量扩大命中', (tester) async {
    final fake = await pumpResults(
      tester,
      client: FakeCoreClient(entries: mixedEntries(450)),
    );
    await tester.tap(find.widgetWithText(FilterChip, '完整'));
    await flush(tester);
    // 切换质量过滤 = 重置分页（offset 回 0，服务端无 quality 参数——契约不扩）
    expect(fake.scanResultQueries.last, (
      taskId: 7,
      offset: 0,
      limit: 200,
      deletedOnly: false,
    ));
    expect(find.text('过滤命中 67 · 已加载 200 / 共 450 项'), findsOneWidget);
    expect(find.textContaining('质量过滤仅作用于已加载项'), findsOneWidget);

    // loadMore 用「已加载数」为 offset（非过滤命中数）→ 过滤集合增量扩大
    await dragToBottom(tester);
    expect(fake.scanResultQueries.map((q) => q.offset), [0, 0, 200]);
    expect(find.text('过滤命中 134 · 已加载 400 / 共 450 项'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('多选：长按进入、计数、导航参数 (taskId, [idx...])', (tester) async {
    await pumpResults(
      tester,
      client: FakeCoreClient(entries: manyEntries(5)),
      taskId: 3,
    );
    await tester.longPress(find.text('IMG_0002.jpg'));
    await flush(tester);
    expect(find.text('恢复所选 (1)'), findsOneWidget);

    await tester.tap(find.text('IMG_0004.jpg'));
    await flush(tester);
    expect(find.text('恢复所选 (2)'), findsOneWidget);

    await tester.tap(find.text('恢复所选 (2)'));
    await tester.pumpAndSettle();
    final recover = tester.widget<RecoverPage>(find.byType(RecoverPage));
    expect(recover.taskId, 3);
    expect(recover.idxs, [2, 4]);
    await unload(tester);
  });

  testWidgets('点击条目 → PreviewPage(taskId, entry)；不在多选态', (tester) async {
    await pumpResults(
      tester,
      client: FakeCoreClient(entries: manyEntries(3)),
      taskId: 9,
    );
    await tester.tap(find.text('IMG_0001.jpg'));
    await tester.pumpAndSettle();
    final preview = tester.widget<PreviewPage>(find.byType(PreviewPage));
    expect(preview.taskId, 9);
    expect(preview.entry.idx, 1);
    await unload(tester);
  });

  testWidgets('铁律文案五组：删除连续 / 删除非连续 / 拓扑未知 / carved / live 静默', (tester) async {
    await pumpResults(
      tester,
      client: FakeCoreClient(
        entries: [
          entry(idx: 0, name: 'a.jpg', deleted: true, contiguous: true),
          entry(idx: 1, name: 'b.jpg', deleted: true, contiguous: false),
          // contiguous == null：fat / 迁移前旧行——拓扑未知，不得借用 exFAT 证据
          entry(idx: 2, name: 'c.jpg', deleted: true),
          entry(
            idx: 3,
            name: '',
            path: '',
            quality: 'carved',
            deleted: true,
            size: 2048,
            byteOffset: 835584,
          ),
          entry(idx: 4, name: 'e.jpg'),
        ],
      ),
    );

    expect(find.text('已删除 · 簇未被占用（完整性高）'), findsOneWidget);
    expect(find.text('已删除 · 按删除链恢复，可能不完整'), findsOneWidget);
    expect(find.text('已删除 · 恢复质量见分级'), findsOneWidget);
    expect(find.text('仅雕刻 · 可能不完整'), findsOneWidget);

    // 5) live + complete 不加任何警示
    final liveTile = find.ancestor(
      of: find.text('e.jpg'),
      matching: find.byType(EntryTile),
    );
    expect(
      find.descendant(of: liveTile, matching: find.textContaining('已删除')),
      findsNothing,
    );
    expect(
      find.descendant(of: liveTile, matching: find.textContaining('仅雕刻')),
      findsNothing,
    );

    // 4) 雕刻件：subtitle 无路径，展示名与导出命名同构（仅供展示）
    expect(find.text('carved_000003.jpg'), findsOneWidget);
    final carvedTile = find.ancestor(
      of: find.text('carved_000003.jpg'),
      matching: find.byType(EntryTile),
    );
    expect(
      find.descendant(of: carvedTile, matching: find.textContaining('/DCIM')),
      findsNothing,
    );
    expect(
      find.descendant(of: carvedTile, matching: find.text('2.0 KB')),
      findsOneWidget,
    );

    // 徽标：complete 4 枚（idx 0/1/2/4）+ carved 1 枚（idx 3）
    expect(
      find.descendant(of: find.byType(EntryTile), matching: find.text('完整')),
      findsNWidgets(4),
    );
    expect(
      find.descendant(of: find.byType(EntryTile), matching: find.text('仅雕刻')),
      findsOneWidget,
    );
    await unload(tester);
  });

  testWidgets('空结果与加载失败：空态文案 + 重试（RpcException 只显示 message）', (tester) async {
    await pumpResults(tester, client: FakeCoreClient());
    expect(find.text('没有找到结果'), findsOneWidget);
    await unload(tester);

    var fail = true;
    final fake = FakeCoreClient(
      entries: manyEntries(3),
      failWith: (m) => m == 'scanResults' && fail
          ? const RpcException(-32003, 'Task not found: 7')
          : null,
    );
    await pumpResults(tester, client: fake);
    expect(find.text('加载失败：Task not found: 7'), findsOneWidget);
    expect(
      find.textContaining('RpcException'),
      findsNothing,
      reason: 'RpcException 只显示契约 message',
    );

    fail = false;
    await tester.tap(find.text('重试'));
    await flush(tester);
    expect(find.text('IMG_0000.jpg'), findsOneWidget);
    expect(find.text('已加载 3 / 共 3 项'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('loadMore 失败：保留已加载页、页脚错误与重试', (tester) async {
    var failNext = false;
    final fake = FakeCoreClient(
      entries: manyEntries(450),
      failWith: (m) => m == 'scanResults' && failNext
          ? const RpcException(-32011, 'Internal error')
          : null,
    );
    await pumpResults(tester, client: fake);
    expect(find.text('已加载 200 / 共 450 项'), findsOneWidget);

    failNext = true;
    await dragToBottom(tester);
    expect(find.text('加载失败：Internal error'), findsOneWidget);
    expect(find.text('已加载 200 / 共 450 项'), findsOneWidget, reason: '已加载页保留');

    failNext = false;
    await tester.tap(find.text('重试'));
    await flush(tester);
    expect(fake.scanResultQueries.map((q) => q.offset), [
      0,
      200,
      200,
    ], reason: '重试仍按已加载数为 offset');
    expect(find.text('已加载 400 / 共 450 项'), findsOneWidget);
    await unload(tester);
  });

  test('铁律禁词：lib/ 全部 Dart 源码不得整串出现（拼接构造，测试自身不构成命中）', () {
    final forbidden = ['连', '续', '假设'].join();
    final hits = <String>[];
    for (final entity in Directory('lib').listSync(recursive: true)) {
      if (entity is! File || !entity.path.endsWith('.dart')) continue;
      if (entity.readAsStringSync().contains(forbidden)) hits.add(entity.path);
    }
    expect(hits, isEmpty, reason: '禁词表示对拓扑的未经证实断言，源码文案与注释均不得出现');
  });

  test('质量徽标映射：完整=绿 / 可能损坏=橙 / 仅雕刻=蓝灰；未知档保守归橙色', () {
    expect(qualityBadgeFor('complete'), (
      label: '完整',
      color: const Color(0xFF2E7D32),
    ));
    expect(qualityBadgeFor('maybeDamaged'), (
      label: '可能损坏',
      color: const Color(0xFFE65100),
    ));
    expect(qualityBadgeFor('carved'), (
      label: '仅雕刻',
      color: const Color(0xFF546E7A),
    ));
    expect(qualityBadgeFor('futureValue').label, '可能损坏');
  });

  test('质量文案纯函数：五条铁律与兜底档', () {
    expect(
      entryQualityNote(entry(idx: 0, deleted: true, contiguous: true))?.text,
      '已删除 · 簇未被占用（完整性高）',
    );
    expect(
      entryQualityNote(entry(idx: 1, deleted: true, contiguous: false))?.text,
      '已删除 · 按删除链恢复，可能不完整',
    );
    expect(
      entryQualityNote(entry(idx: 2, deleted: true))?.text,
      '已删除 · 恢复质量见分级',
    );
    expect(
      entryQualityNote(entry(idx: 3, quality: 'carved'))?.text,
      '仅雕刻 · 可能不完整',
    );
    expect(
      entryQualityNote(entry(idx: 4)),
      isNull,
      reason: 'live + complete 静默',
    );
    expect(
      entryQualityNote(entry(idx: 5, quality: 'maybeDamaged')),
      isNull,
      reason: 'live 由徽标呈现，不加删除/雕刻语境文案',
    );
  });
}
