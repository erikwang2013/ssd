// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/ipc_transport.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';

void main() {
  final bin = Platform.environment['XD_DAEMON_BIN'];
  test('handshake with real daemon (ping + device.list)', () async {
    final dir = Directory.systemTemp.createTempSync('xd_ui_it');
    addTearDown(() => dir.deleteSync(recursive: true));
    final image = File('${dir.path}/test.img')
      ..writeAsBytesSync(List<int>.filled(4096, 0));
    // root 宿主上 daemon 的 --image 会走 PKEXEC_UID 校验（privcheck，失败关闭）；以本进程
    // euid 注入，让 root 环境也覆盖同一生产校验路径（非 root 时该分支走不到，注入无害）。
    String? euid;
    if (Platform.isLinux) {
      try {
        euid = Process.runSync('id', ['-u']).stdout.toString().trim();
      } on ProcessException {
        stderr.writeln(
          'skip: `id` 不可用，不注入 PKEXEC_UID',
        ); // 与 Rust harness 的 early-return 对称
      }
    }
    final client = await IpcCoreClient.start(
      daemonPath: bin,
      extraArgs: ['--image', image.path],
      environment: euid == null ? null : {'PKEXEC_UID': euid},
    );
    try {
      final ping = await client.ping();
      expect(ping.pong, isTrue);
      // M1b：协议号 0→1 的机械波及（真 daemon 的活契约断言；CI 设 XD_DAEMON_BIN 时执行）。
      // Dart 侧无协议常量（protocol 仅透传解码）；fake 断言已在 T5 同步为 1。
      expect(ping.protocol, 1);
      final devices = await client.listDevices();
      // M1e 起 Linux 启动时枚举物理盘，总数依宿主而异；断言收敛为「镜像设备在列且正确」
      final images = devices.where((d) => d.kind == 'image').toList();
      expect(images, hasLength(1));
      expect(images.single.sizeBytes, 4096);

      // M1d v1.2 新链路真 daemon 往返：方法路由/参数形状/错误解码全过真线上。
      // 零填充镜像非合法 FS → scan.start 确定返回 -32002；happy path 归 Rust 侧
      // scan_ipc/export_ipc 与 e2e-loop（Dart 侧无镜像夹具生成能力）。
      await expectLater(
        client.scanStart(images.single.id),
        throwsA(
          isA<RpcException>()
              .having((e) => e.code, 'code', -32002)
              .having((e) => e.message, 'message', 'Unsupported file system'),
        ),
      );
      Future<void> expectTaskNotFound(Future<Object?> call) => expectLater(
        call,
        throwsA(
          isA<RpcException>()
              .having((e) => e.code, 'code', -32003)
              .having((e) => e.message, 'message', 'Task not found: 7'),
        ),
      );
      await expectTaskNotFound(client.scanStatus(7));
      await expectTaskNotFound(client.scanResults(7));
      await expectTaskNotFound(client.scanPause(7));
      await expectTaskNotFound(client.fsRead(7, 0));
      await expectTaskNotFound(client.exportStart(7, const [0], dir.path));
      // -32602 的 message 为诊断文本（非契约，README「错误」节）——只钉码
      await expectLater(
        client.exportCancel(9),
        throwsA(isA<RpcException>().having((e) => e.code, 'code', -32602)),
      );
    } finally {
      await client.close();
    }
  }, skip: bin == null ? 'XD_DAEMON_BIN 未设置，跳过' : null);
}
