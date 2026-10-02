// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 预览页 widget 测试（Fake 驱动）：按 ext 分派（图片分片组装到 eof / 文本前缀 /
// 其它仅信息卡）、坏图 errorBuilder、>32MiB 不拉、短交付提示、雕刻件信息卡与
// 导航参数（写路径只传 idx——displayName 仅供展示）。
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/features/preview/preview_page.dart';
import 'package:xiaodun_ui/features/recover/recover_page.dart';

import 'fake_core_client.dart';

/// 有效 1×1 灰度 PNG（67B，可真解码的最小件）。
final Uint8List tinyPng = base64Decode(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGNgAAAAAgABSK+kcQAAAABJRU5ErkJggg==',
);

ScanEntry entry({
  int idx = 0,
  String name = 'a.jpg',
  String path = '/DCIM',
  String ext = 'jpg',
  int size = 67,
  bool deleted = false,
  String quality = 'complete',
  int? byteOffset,
}) => ScanEntry(
  idx: idx,
  name: name,
  path: path,
  ext: ext,
  sizeBytes: size,
  deleted: deleted,
  isDir: false,
  quality: quality,
  firstCluster: 2,
  byteOffset: byteOffset,
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

Future<FakeCoreClient> pumpPreview(
  WidgetTester tester, {
  FakeCoreClient? client,
  ScanEntry? e,
  int taskId = 7,
}) async {
  final fake = client ?? FakeCoreClient();
  await tester.pumpWidget(
    MaterialApp(
      home: PreviewPage(client: fake, taskId: taskId, entry: e ?? entry()),
    ),
  );
  await flush(tester);
  return fake;
}

void main() {
  testWidgets('图片：两片组装到 eof（offset 按实收递增）→ Image.memory 完整字节', (tester) async {
    const split = 40;
    final fake = FakeCoreClient()
      ..onFsRead = (taskId, idx, offset, length) => offset == 0
          ? FsReadResult(bytes: tinyPng.sublist(0, split), eof: false)
          : FsReadResult(bytes: tinyPng.sublist(offset), eof: true);
    await pumpPreview(
      tester,
      client: fake,
      e: entry(idx: 3, size: tinyPng.length),
    );

    expect(
      fake.fsReadQueries.map((q) => (q.taskId, q.idx, q.offset, q.length)),
      [(7, 3, 0, 1048576), (7, 3, split, 1048576)],
      reason: '1MiB/片，次片 offset = 首片实收字节数',
    );
    final image = tester.widget<Image>(find.byType(Image));
    expect((image.image as MemoryImage).bytes, tinyPng, reason: '两片须按序无损拼装');
    expect(find.text('实际数据短于声明大小'), findsNothing, reason: 'eof 且实收==声明大小');
    await unload(tester);
  });

  testWidgets('坏图：errorBuilder 显示「数据损坏，无法预览」而非红屏', (tester) async {
    final fake = FakeCoreClient()
      ..onFsRead = (taskId, idx, offset, length) => FsReadResult(
        bytes: Uint8List.fromList(List.filled(64, 0xAB)),
        eof: true,
      );
    await pumpPreview(tester, client: fake, e: entry(size: 64));

    // 图像解码走真实异步（fake async 区外），runAsync 让引擎完成解码失败回调
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 100)),
    );
    await tester.pump();
    await tester.pump();
    expect(find.text('数据损坏，无法预览'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('>32MiB：不调用 fsRead，显示「文件过大，暂不支持预览」', (tester) async {
    final fake = FakeCoreClient();
    await pumpPreview(
      tester,
      client: fake,
      e: entry(size: 33 * 1024 * 1024),
    );
    expect(fake.fsReadQueries, isEmpty, reason: '声明超限即不拉');
    expect(find.text('文件过大，暂不支持预览'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('文本：单次 256KiB 前缀 + utf8(allowMalformed) → SelectableText 中文', (
    tester,
  ) async {
    const content = '你好，小盾 · 预览文本';
    final bytes = Uint8List.fromList(utf8.encode(content));
    final fake = FakeCoreClient()
      ..onFsRead = (taskId, idx, offset, length) =>
          FsReadResult(bytes: bytes, eof: true);
    await pumpPreview(
      tester,
      client: fake,
      e: entry(name: 'note.txt', ext: 'txt', size: bytes.length),
    );
    expect(fake.fsReadQueries.single, (
      taskId: 7,
      idx: 0,
      offset: 0,
      length: 262144,
    ));
    final text = tester.widget<SelectableText>(find.byType(SelectableText));
    expect(text.data, content);
    await unload(tester);
  });

  testWidgets('雕刻件：displayName 展示名 / 信息卡「偏移」/ 「仅雕刻」；回读按 idx', (tester) async {
    final fake = FakeCoreClient()
      ..onFsRead = (taskId, idx, offset, length) =>
          FsReadResult(bytes: tinyPng, eof: true);
    await pumpPreview(
      tester,
      client: fake,
      e: entry(
        idx: 1,
        name: '',
        path: '',
        ext: 'jpeg',
        size: tinyPng.length,
        deleted: true,
        quality: 'carved',
        byteOffset: 835584,
      ),
    );
    expect(fake.fsReadQueries.single.idx, 1);
    expect(find.text('carved_000001.jpeg'), findsWidgets);
    expect(find.text('偏移'), findsOneWidget);
    expect(find.text('835584'), findsOneWidget);
    expect(find.text('仅雕刻'), findsOneWidget);
    expect(find.text('仅雕刻 · 可能不完整'), findsOneWidget);
    await unload(tester);
  });

  testWidgets('短交付：eof 到而实收 < 声明大小 → 提示（不猜 VDL）；未到 eof 的前缀不提示', (tester) async {
    final fake = FakeCoreClient()
      ..onFsRead = (taskId, idx, offset, length) =>
          FsReadResult(bytes: Uint8List.fromList(utf8.encode('短')), eof: true);
    await pumpPreview(
      tester,
      client: fake,
      e: entry(name: 'cut.txt', ext: 'txt', size: 100),
    );
    expect(find.text('实际数据短于声明大小'), findsOneWidget);
    await unload(tester);

    // 大文件前缀读（eof=false，256KiB < 声明大小）：还有后续数据 → 不得误报短交付
    final big = FakeCoreClient()
      ..onFsRead = (taskId, idx, offset, length) => FsReadResult(
        bytes: Uint8List.fromList(utf8.encode('a' * 262144)),
        eof: false,
      );
    await pumpPreview(
      tester,
      client: big,
      e: entry(name: 'big.log', ext: 'log', size: 300 * 1024),
    );
    expect(big.fsReadQueries.single.length, 262144);
    expect(
      find.text('实际数据短于声明大小'),
      findsNothing,
      reason: 'eof 未到即还有数据可读，实收短于声明是截断而非短交付',
    );
    await unload(tester);
  });

  testWidgets('底部「恢复此文件」→ RecoverPage(taskId, [idx])；写路径只传 idx', (
    tester,
  ) async {
    await pumpPreview(
      tester,
      client: FakeCoreClient(),
      e: entry(idx: 5, name: 'x.txt', ext: 'txt', size: 0),
      taskId: 9,
    );
    await tester.tap(find.text('恢复此文件'));
    await tester.pumpAndSettle();
    final recover = tester.widget<RecoverPage>(find.byType(RecoverPage));
    expect(recover.taskId, 9);
    expect(recover.idxs, [5]);
    await unload(tester);
  });
}
