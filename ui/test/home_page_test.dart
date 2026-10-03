// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/home_page.dart';

import 'fake_core_client.dart';

void main() {
  testWidgets('shows device list from client', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: HomePage(
          client: FakeCoreClient(
            devices: const [
              DeviceInfo(
                id: 'image:test.img',
                name: 'test.img',
                kind: 'image',
                sizeBytes: 4096,
                removable: false,
              ),
            ],
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('test.img'), findsOneWidget);
    expect(find.textContaining('image · 4.0 KB'), findsOneWidget);
  });

  testWidgets('shows error state with retry', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: HomePage(
          client: FakeCoreClient(
            failWith: (_) => const RpcException(-1, 'boom'),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.textContaining('boom'), findsOneWidget);
    expect(find.text('重试'), findsOneWidget);
  });

  testWidgets('retry re-invokes the client', (tester) async {
    var calls = 0;
    final client = FakeCoreClient(
      devices: const [
        DeviceInfo(
          id: 'image:retry.img',
          name: 'retry.img',
          kind: 'image',
          sizeBytes: 2048,
          removable: false,
        ),
      ],
      failWith: (method) => method == 'listDevices' && calls++ == 0
          ? const RpcException(-1, 'boom')
          : null,
    );
    await tester.pumpWidget(MaterialApp(home: HomePage(client: client)));
    await tester.pumpAndSettle();
    expect(find.text('重试'), findsOneWidget);
    await tester.tap(find.text('重试'));
    await tester.pumpAndSettle();
    expect(find.text('retry.img'), findsOneWidget);
  });

  testWidgets('shows empty state when no devices', (tester) async {
    await tester.pumpWidget(
      MaterialApp(home: HomePage(client: FakeCoreClient())),
    );
    await tester.pumpAndSettle();
    expect(find.text('未发现设备'), findsOneWidget);
  });

  testWidgets('tapping a device opens the scan page (M1d T5)', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: HomePage(
          client: FakeCoreClient(
            devices: const [
              DeviceInfo(
                id: 'image:nav.img',
                name: 'nav.img',
                kind: 'image',
                sizeBytes: 1024,
                removable: false,
              ),
            ],
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('nav.img'));
    await tester.pumpAndSettle();
    expect(find.text('nav.img'), findsOneWidget); // 扫描页 AppBar
    expect(find.text('快速扫描'), findsOneWidget);
    expect(find.text('开始扫描'), findsOneWidget);
  });
}
