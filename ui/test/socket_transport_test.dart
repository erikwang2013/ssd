// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 传输层：SocketCoreClient（TCP 回环提权会话）协议级钉死——用 Dart 假 daemon
// （ServerSocket.bind 回环随机端口）照真 daemon 的握手语义：首行必须 {"auth":<token>}，
// 否则回 -32001 即断；通过后逐行应答 + 可推通知。真 daemon 全链路见 Rust 侧
// crates/xd-daemon/tests/tcp_session.rs（CI 三平台真跑）。
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/ipc_transport.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';

import 'fake_daemon.dart';

void main() {
  test('握手：auth 首行逐字、调用应答、通知进 notifications 流', () async {
    const progress =
        '{"jsonrpc":"2.0","method":"scan.progress","params":{"taskId":1,"state":"scanning","readBytes":9,"foundCount":1,"elapsedMs":2}}';
    final daemon = await FakeDaemon.start(
      token: 'sekret',
      // 应答前先推通知（真 daemon 同款抢序）：两序都由客户端容忍
      onRequest: (req, send) {
        if (req['method'] == 'ping') {
          send(progress);
          send(pongLine(req));
        }
      },
    );
    addTearDown(daemon.close);
    final client = await SocketCoreClient.start(
      addr: '127.0.0.1:${daemon.port}',
      token: 'sekret',
    );
    addTearDown(client.close);

    final events = <Map<String, dynamic>>[];
    final sub = client.notifications.listen(events.add);
    addTearDown(sub.cancel);
    // 多监听者：notifications 必须 broadcast（扫描页 + 导出页可同时在场）
    final sub2 = client.notifications.listen((_) {});
    addTearDown(sub2.cancel);

    final pong = await client.ping();
    expect(pong.pong, isTrue);
    expect(pong.protocol, 1);

    // ping 的响应到达时，先行的通知必已处理（同一 TCP 序），无需等
    expect(events.single['method'], 'scan.progress');
    expect(events.single.containsKey('id'), isFalse);
    expect(daemon.authLines.single, '{"auth":"sekret"}');
  });

  test('错误令牌：-32001 抛 RpcException，后续调用快速失败不悬挂', () async {
    final daemon = await FakeDaemon.start(token: 'right');
    addTearDown(daemon.close);
    final client = await SocketCoreClient.start(
      addr: '127.0.0.1:${daemon.port}',
      token: 'wrong',
    );
    addTearDown(client.close);

    final matcher = throwsA(
      isA<RpcException>().having((e) => e.code, 'code', -32001),
    );
    await expectLater(client.ping(), matcher);
    // 认证已失败：第二次调用必须立即错（而非 10s 超时）
    await expectLater(client.ping(), matcher);
  });

  test('port-file：解析纯函数与 start(portFile:) 全链路', () async {
    expect(SocketCoreClient.parsePortFile('41234 aabbcc\n'), (
      port: 41234,
      token: 'aabbcc',
    ));
    expect(() => SocketCoreClient.parsePortFile(''), throwsFormatException);
    expect(
      () => SocketCoreClient.parsePortFile('41234'),
      throwsFormatException,
    );
    expect(
      () => SocketCoreClient.parsePortFile('41234 a b'),
      throwsFormatException,
    );
    expect(
      () => SocketCoreClient.parsePortFile('0 tok'),
      throwsFormatException,
    );
    expect(
      () => SocketCoreClient.parsePortFile('70000 tok'),
      throwsFormatException,
    );

    final dir = Directory.systemTemp.createTempSync('xd_socket');
    addTearDown(() => dir.deleteSync(recursive: true));
    final daemon = await FakeDaemon.start(
      token: 'tok42',
      onRequest: (req, send) => send(pongLine(req)),
    );
    addTearDown(daemon.close);
    final pf = File('${dir.path}/session.port');
    pf.writeAsStringSync('${daemon.port} tok42\n');

    final client = await SocketCoreClient.start(portFile: pf.path);
    addTearDown(client.close);
    expect((await client.ping()).pong, isTrue);
  });

  test('给源缺失：start() 无 portFile 也无 addr+token → StateError', () async {
    await expectLater(SocketCoreClient.start(), throwsStateError);
    await expectLater(
      SocketCoreClient.start(addr: '127.0.0.1:1'),
      throwsStateError,
    );
  });
}
