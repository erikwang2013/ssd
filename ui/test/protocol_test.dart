// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';

Map<String, dynamic> golden(String name) =>
    jsonDecode(File('../proto/v0/examples/$name').readAsStringSync())
        as Map<String, dynamic>;

void main() {
  test('ping response golden decodes', () {
    final msg = golden('ping.response.json');
    final ping = PingResult.fromJson(decodeResult(msg));
    expect(ping.pong, isTrue);
    expect(ping.version, isNotEmpty); // 版本值随发版变动（与 Rust 侧 env! 策略对齐），此处只验证字段解析
    expect(ping.protocol, 0);
  });

  test('device_list response golden decodes', () {
    final msg = golden('device_list.response.json');
    final devices = (decodeResult(msg)['devices'] as List)
        .map((e) => DeviceInfo.fromJson(e as Map<String, dynamic>))
        .toList();
    expect(devices, hasLength(1));
    expect(devices[0].name, 'test.img');
    expect(devices[0].kind, 'image');
    expect(devices[0].sizeBytes, 4096);
    expect(devices[0].fsGuess, isNull);
    expect(devices[0].toJson(), msg['result']['devices'][0]);
  });

  test('encodeRequest matches golden request', () {
    final encoded = jsonDecode(
      encodeRequest(id: 1, method: 'ping', params: null),
    );
    expect(encoded, golden('ping.request.json'));
  });

  test('encodeRequest matches device_list request golden', () {
    final encoded = jsonDecode(
      encodeRequest(id: 2, method: 'device.list', params: null),
    );
    expect(encoded, golden('device_list.request.json'));
  });

  test('error golden throws RpcException with code', () {
    final msg = golden('error_method_not_found.response.json');
    expect(
      () => decodeResult(msg),
      throwsA(isA<RpcException>().having((e) => e.code, 'code', -32601)),
    );
  });
}
