import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/core_client.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/home_page.dart';

class FakeCoreClient implements CoreClient {
  FakeCoreClient(this.devices);
  final List<DeviceInfo> devices;

  @override
  Future<PingResult> ping() async =>
      const PingResult(pong: true, version: 'test', protocol: 0);

  @override
  Future<List<DeviceInfo>> listDevices() async => devices;

  @override
  Future<void> close() async {}
}

class FailingCoreClient implements CoreClient {
  @override
  Future<PingResult> ping() async => throw const RpcException(-1, 'boom');

  @override
  Future<List<DeviceInfo>> listDevices() async =>
      throw const RpcException(-1, 'boom');

  @override
  Future<void> close() async {}
}

void main() {
  testWidgets('shows device list from client', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: HomePage(
          client: FakeCoreClient(const [
            DeviceInfo(
              id: 'image:test.img',
              name: 'test.img',
              kind: 'image',
              sizeBytes: 4096,
              removable: false,
            ),
          ]),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('test.img'), findsOneWidget);
    expect(find.textContaining('image · 4.0 KB'), findsOneWidget);
  });

  testWidgets('shows error state with retry', (tester) async {
    await tester.pumpWidget(
      MaterialApp(home: HomePage(client: FailingCoreClient())),
    );
    await tester.pumpAndSettle();
    expect(find.textContaining('boom'), findsOneWidget);
    expect(find.text('重试'), findsOneWidget);
  });
}
