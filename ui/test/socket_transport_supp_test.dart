// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// qual-m1e-t1 补测（最小集）：封住 SocketCoreClient 会话断开的一个未钉缺口。
// 放置：新文件 `ui/test/socket_transport_supp_test.dart`（与 socket_transport_test.dart 平级）。
//
// 钉子与对应变异：`in_flight_call_fails_fast_when_server_dies_without_32001`
// 杀「SocketCoreClient 构造器里 `_socket.done → _failPending` 接线摘除」。
// 真 daemon 崩溃/被 kill 时不会留下 -32001（那是认证失败专用）；在飞请求必须由
// socket.done 兜底立刻失败，否则悬挂到 10s RPC 超时——扫描/恢复流程会假死 10s。
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/ipc_transport.dart';

void main() {
  test('握手后服务端直接断开（无 -32001）：在飞调用快速失败而非 10s 超时', () async {
    final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(server.close);
    server.listen((socket) {
      final lines = socket
          .cast<List<int>>()
          .transform(const Utf8Decoder(allowMalformed: true))
          .transform(const LineSplitter());
      lines.listen((line) {
        // 首行是 auth 握手；见到业务请求行即断开（模拟 daemon 死亡，无任何应答）
        if (!line.contains('"auth"')) {
          unawaited(socket.flush().then((_) => socket.close()));
        }
      });
    });

    final client = await SocketCoreClient.start(
      addr: '127.0.0.1:${server.port}',
      token: 'tok',
    );
    addTearDown(client.close);

    final sw = Stopwatch()..start();
    Object? err;
    try {
      await client.ping(); // 服务端读到该请求行后立刻断开、绝不回包
    } catch (e) {
      err = e;
    }
    sw.stop();

    expect(err, isNotNull, reason: '会话死亡必须让在飞调用失败');
    expect(
      err,
      isNot(isA<TimeoutException>()),
      reason: '必须由断开事件快速失败，而不是 10s RPC 超时（实收 $err）',
    );
    expect(
      sw.elapsed,
      lessThan(const Duration(seconds: 3)),
      reason: '快速失败（实耗 ${sw.elapsed}）',
    );
  });
}
