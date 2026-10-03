// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// T9 全链路集成（真 daemon，XD_DAEMON_BIN 守卫，未设则 skip）：
// 镜像生成（Rust 夹具）→ 起 daemon → scan(quick) → results → fsRead 逐字节 → 导出 → 报告。
//
// 夹具 = `cargo run -p xd-fixtures --example make_carve_fixture -- <path>`（Dart 无镜像构建
// 能力，计划 T9 裁定交给 Rust 小工具）：live/删除/雕刻三埋点 + 侧车期望字节
// （<img>.live.bin/.deleted.bin/.carved.bin）；BIG.TXT 目录项声明 >64MiB 且无真实数据，
// 专供 -32009 契约拒绝线（图片路径另有本地 32MiB 截先行，见 T7 移交）。
//
// 覆盖移交点：T7「雕刻件预览断言 fsRead(idx)+字节」「-32009 用文本件」；T8「真 daemon exportId
// 过滤 + 抢跑时序」——通知订阅**先于**发令、全量寄存后按 exportId 回捞（同 RecoverController
// 的寄存回放语义；真序由 daemon 决定，两序皆不得丢终报）；`-32006/-32010` 文案已由 T8 测试钉死，
// 不在此重复（真环回断言归 scripts/e2e-loop.sh）。
import 'dart:async';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/core_client.dart';
import 'package:xiaodun_ui/core_client/ipc_transport.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';

/// 通知观察器（寄存回放）：订阅先于发令；`waitFor` 先回捞已寄存项（抢跑通知不丢），
/// 再挂等后续——按 exportId/taskId 过滤由调用方谓词给出。
class _Notifier {
  _Notifier(CoreClient client) {
    _sub = client.notifications.listen((m) {
      _seen.add(m);
      for (final w in List.of(_waiters)) {
        if (w.pred(m)) {
          _waiters.remove(w);
          w.completer.complete(m);
        }
      }
    });
  }

  late final StreamSubscription<Map<String, dynamic>> _sub;
  final List<Map<String, dynamic>> _seen = [];
  final List<
    ({
      bool Function(Map<String, dynamic>) pred,
      Completer<Map<String, dynamic>> completer,
    })
  >
  _waiters = [];

  Future<Map<String, dynamic>> waitFor(
    bool Function(Map<String, dynamic>) pred,
  ) {
    for (final m in _seen) {
      if (pred(m)) return Future.value(m); // 寄存回放
    }
    final c = Completer<Map<String, dynamic>>();
    _waiters.add((pred: pred, completer: c));
    return c.future.timeout(const Duration(seconds: 30));
  }

  Future<void> dispose() => _sub.cancel();
}

Map<String, dynamic> _params(Map<String, dynamic> m) =>
    m['params'] as Map<String, dynamic>;

void main() {
  final bin = Platform.environment['XD_DAEMON_BIN'];
  test(
    '全链路：镜像 → 扫描 → 预览读 → 导出 → 报告（真 daemon）',
    () async {
      final tmp = Directory.systemTemp.createTempSync('xd_export_it');
      addTearDown(() => tmp.deleteSync(recursive: true));

      // 1) Rust 夹具（含侧车期望字节）。cargo 自 ui/ 向上找 workspace 根（CI flutter job 同款）。
      final image = File('${tmp.path}/carve.img');
      final gen = Process.runSync('cargo', [
        'run',
        '-q',
        '--locked',
        '-p',
        'xd-fixtures',
        '--example',
        'make_carve_fixture',
        '--',
        image.path,
      ]);
      expect(gen.exitCode, 0, reason: '夹具生成失败: ${gen.stderr}');
      final liveBytes = File('${image.path}.live.bin').readAsBytesSync();
      final deletedBytes = File('${image.path}.deleted.bin').readAsBytesSync();
      final carvedBytes = File('${image.path}.carved.bin').readAsBytesSync();

      // root 宿主上 daemon 的 --image 走 PKEXEC_UID 校验（privcheck，失败关闭）；注入本进程
      // euid，让 root 环境也覆盖同一生产校验路径（同 ipc_integration_test）。
      String? euid;
      if (Platform.isLinux) {
        try {
          euid = Process.runSync('id', ['-u']).stdout.toString().trim();
        } on ProcessException {
          stderr.writeln('skip: `id` 不可用，不注入 PKEXEC_UID');
        }
      }
      // --db 必带：export 子进程按 --db 只读打开同一库，内存库降级时 export.start 诚实 -32603。
      final client = await IpcCoreClient.start(
        daemonPath: bin,
        extraArgs: ['--image', image.path, '--db', '${tmp.path}/tasks.db'],
        environment: euid == null ? null : {'PKEXEC_UID': euid},
      );
      final notifier = _Notifier(client);
      final outA = Directory('${tmp.path}/out-a')..createSync();
      final outB = Directory('${tmp.path}/out-b')..createSync();
      try {
        final devices = await client.listDevices();
        final dev = devices.firstWhere((d) => d.kind == 'image');

        // 2) quick 扫描：等 finished 通知（订阅先于发令，抢跑也不丢）→ results 全量。
        final quick = await client.scanStart(dev.id);
        await notifier.waitFor(
          (m) =>
              m['method'] == 'scan.finished' &&
              _params(m)['taskId'] == quick.taskId,
        );
        final page = await client.scanResults(quick.taskId, limit: 100);
        expect(page.total, 4, reason: 'live/删除/目录/大件声明 共 4 条');
        final live = page.entries.singleWhere(
          (e) => !e.deleted && !e.isDir && e.name.endsWith('.JPG'),
        );
        final deleted = page.entries.singleWhere((e) => e.deleted);
        final big = page.entries.singleWhere((e) => e.name == 'BIG.TXT');

        // 3) fsRead 逐字节：live 与删除件（恢复路径的两种读形态）。
        final liveRead = await client.fsRead(quick.taskId, live.idx);
        expect(liveRead.eof, isTrue);
        expect(liveRead.bytes, equals(liveBytes), reason: 'live 回读须逐字节相等');
        final deletedRead = await client.fsRead(quick.taskId, deleted.idx);
        expect(deletedRead.eof, isTrue);
        expect(deletedRead.bytes, equals(deletedBytes), reason: '删除件回读须逐字节相等');

        // 4) -32009 契约拒绝线：声明 >64MiB 的文本件（读取前按声明 size 拒绝）。
        await expectLater(
          client.fsRead(quick.taskId, big.idx),
          throwsA(
            isA<RpcException>()
                .having((e) => e.code, 'code', -32009)
                .having(
                  (e) => e.message,
                  'message',
                  'Entry too large: ${big.sizeBytes}',
                ),
          ),
        );

        // 5) 深扫雕刻链路：quality==carved 恰 1 条 → fsRead 回读 == 埋点原字节（T7 移交）。
        final deep = await client.scanStart(dev.id, mode: 'deep');
        await notifier.waitFor(
          (m) =>
              m['method'] == 'scan.finished' &&
              _params(m)['taskId'] == deep.taskId,
        );
        final deepPage = await client.scanResults(deep.taskId, limit: 100);
        final carved = deepPage.entries
            .where((e) => e.quality == 'carved')
            .toList();
        expect(carved, hasLength(1), reason: '深扫须恰雕 1 条（埋点唯一，假阳性 0）');
        expect(carved.single.byteOffset, isNotNull);
        final carvedRead = await client.fsRead(deep.taskId, carved.single.idx);
        expect(carvedRead.eof, isTrue);
        expect(
          carvedRead.bytes,
          equals(carvedBytes),
          reason: '雕刻件回读须逐字节等于埋点原字节',
        );

        // 6) 导出：quick 任务（live+删除）与深扫任务（雕刻件）并发两笔——真 daemon 的
        // exportId 过滤：两笔终报各归其 id/targetDir，落盘不串目录。
        final startA = client.exportStart(quick.taskId, [
          live.idx,
          deleted.idx,
        ], outA.path);
        final startB = client.exportStart(deep.taskId, [
          carved.single.idx,
        ], outB.path);
        final resA = await startA;
        final resB = await startB;
        expect(resA.exportId, isNot(resB.exportId));
        expect(resA.fileCount, 2);
        expect(resB.fileCount, 1);

        Future<Map<String, dynamic>> finishedOf(int exportId) =>
            notifier.waitFor(
              (m) =>
                  m['method'] == 'export.finished' &&
                  _params(m)['exportId'] == exportId,
            );
        final finA = ExportFinished.fromJson(
          _params(await finishedOf(resA.exportId)),
        );
        final finB = ExportFinished.fromJson(
          _params(await finishedOf(resB.exportId)),
        );

        // 7) 报告 + 落盘逐字节精确。
        expect(finA.succeeded, 2);
        expect(finA.degraded, 0);
        expect(finA.failed, 0);
        expect(finA.canceled, isFalse);
        expect(finA.targetDir, outA.path);
        expect(finA.items, isEmpty, reason: '成功件不进 items（契约）');
        expect(finB.succeeded, 1);
        expect(finB.degraded, 0);
        expect(finB.failed, 0);
        expect(finB.targetDir, outB.path);

        expect(
          File('${outA.path}/${live.name}').readAsBytesSync(),
          equals(liveBytes),
        );
        expect(
          File('${outA.path}/${deleted.name}').readAsBytesSync(),
          equals(deletedBytes),
        );
        // 雕刻件落盘名（carved_{idx:06}.{ext}）由 worker 决定：目录内恰一件，逐字节比对。
        final outBfiles = outB.listSync().whereType<File>().toList();
        expect(outBfiles, hasLength(1));
        expect(outBfiles.single.readAsBytesSync(), equals(carvedBytes));
      } finally {
        await client.close();
        await notifier.dispose();
      }
    },
    skip: bin == null ? 'XD_DAEMON_BIN 未设置，跳过' : null,
    timeout: const Timeout(Duration(minutes: 5)),
  );
}
