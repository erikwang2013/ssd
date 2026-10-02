import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/ipc_transport.dart';

void main() {
  final bin = Platform.environment['XD_DAEMON_BIN'];
  test('handshake with real daemon (ping + device.list)', () async {
    final dir = Directory.systemTemp.createTempSync('xd_ui_it');
    final image = File('${dir.path}/test.img')
      ..writeAsBytesSync(List<int>.filled(4096, 0));
    final client = await IpcCoreClient.start(
      daemonPath: bin,
      extraArgs: ['--image', image.path],
    );
    try {
      final ping = await client.ping();
      expect(ping.pong, isTrue);
      expect(ping.protocol, 0);
      final devices = await client.listDevices();
      expect(devices, hasLength(1));
      expect(devices.single.kind, 'image');
      expect(devices.single.sizeBytes, 4096);
    } finally {
      await client.close();
    }
  }, skip: bin == null ? 'XD_DAEMON_BIN 未设置，跳过' : null);
}
