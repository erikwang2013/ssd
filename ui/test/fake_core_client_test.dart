// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 消费方保护套：Fake 的分页语义对齐 daemon（ORDER BY idx / limit 1..=1000 /
// total = 过滤后总数），乱序 fixture 恒按 idx 出页。
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';

import 'fake_core_client.dart';

ScanEntry entry(int idx, {required bool deleted}) => ScanEntry(
  idx: idx,
  name: 'E$idx.TXT',
  path: '/',
  ext: 'txt',
  sizeBytes: 10,
  deleted: deleted,
  isDir: false,
  quality: 'complete',
  firstCluster: 0,
);

/// 乱序注入：物理序 [3,0,4,1,2]，偶数为删除项。
FakeCoreClient makeClient() => FakeCoreClient(
  entries: [
    entry(3, deleted: false),
    entry(0, deleted: true),
    entry(4, deleted: true),
    entry(1, deleted: false),
    entry(2, deleted: true),
  ],
);

void main() {
  test('offset/limit 分页：total = 全量，页内容按 idx 序', () async {
    final page = await makeClient().scanResults(1, offset: 1, limit: 2);
    expect(page.total, 5);
    expect(page.entries.map((e) => e.idx).toList(), [
      1,
      2,
    ], reason: '分页排序键 = idx（README ScanEntry 节）');
  });

  test('deletedOnly：过滤后再分页，total 为过滤后总数', () async {
    final page = await makeClient().scanResults(1, deletedOnly: true);
    expect(page.total, 3);
    expect(page.entries.map((e) => e.idx).toList(), [0, 2, 4]);
  });

  test('offset 越尾 = 空页（total 不变）', () async {
    final page = await makeClient().scanResults(1, offset: 99);
    expect(page.total, 5);
    expect(page.entries, isEmpty);
  });

  test('limit 越界 → -32602 且文案与 daemon 逐字', () async {
    for (final bad in [0, 1001]) {
      await expectLater(
        makeClient().scanResults(1, limit: bad),
        throwsA(
          isA<RpcException>()
              .having((e) => e.code, 'code', -32602)
              .having(
                (e) => e.message,
                'message',
                'Invalid params: limit out of range 1..=1000',
              ),
        ),
      );
    }
  });
}
