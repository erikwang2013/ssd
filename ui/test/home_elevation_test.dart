// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// M2 Task 6：首页提权入口（缺陷②——Windows 非提权时枚举为空 ⇒ 首页无设备 ⇒ 走不到扫描页的
// -32001 引导）。widget 层：平台注入（`debugDefaultTargetPlatformOverride`）+ 假启动器（写假
// port-file）+ 假连接器 → 断言按钮可见性、换 client 并重列、取消文案与不换 client、macOS
// FDA 提示。提权流本体（命令构造/会话建立）的钉在 elevation_test.dart（T6 抽取共享流，零回归）。
// **未验证（需真机）**：真 UAC/osascript/pkexec 对话框与提权链、真 FDA 交互。
import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/elevation.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/home_page.dart';

import 'fake_core_client.dart';

const _daemon = '/opt/xiaodun/xd-daemon';

/// 提权后（管理员上下文）才列得出的设备——非提权时首页为空正是本任务的触发场景。
const _elevatedDevice = DeviceInfo(
  id: r'win:\\.\PhysicalDrive1',
  name: 'U 盘 · SanDisk',
  kind: 'physical',
  sizeBytes: 4096,
  removable: true,
);

const _localDevice = DeviceInfo(
  id: 'image:local.img',
  name: 'local.img',
  kind: 'image',
  sizeBytes: 2048,
  removable: false,
);

void main() {
  // `debugDefaultTargetPlatformOverride` 必须在测试体内/tearDown 前清掉（binding 在测试体结束、
  // addTearDown 之前校验 debug 变量）；这条兜底防早失败漏清导致的跨测试串台。
  tearDown(() => debugDefaultTargetPlatformOverride = null);

  /// 首页提权入口的公共铺垫：Fake 客户端（带 daemonPath）+ 假启动器（写假 port-file）+
  /// 假连接器（回提权会话 client）。
  Future<
    ({
      FakeCoreClient local,
      FakeCoreClient elevated,
      List<ElevationPlan> plans,
      List<Object> replaced,
    })
  >
  pumpHome(
    WidgetTester tester, {
    required TargetPlatform platform,
    List<DeviceInfo> localDevices = const [],
    List<DeviceInfo> elevatedDevices = const [_elevatedDevice],
    Future<int> Function()? launcherExit,
  }) async {
    debugDefaultTargetPlatformOverride = platform;
    final local = FakeCoreClient(
      devices: localDevices,
      daemonPathOverride: _daemon,
    );
    final elevated = FakeCoreClient(devices: elevatedDevices);
    final plans = <ElevationPlan>[];
    final replaced = <Object>[];
    await tester.pumpWidget(
      MaterialApp(
        home: HomePage(
          client: local,
          onClientReplaced: replaced.add,
          elevationLauncher: (plan) async {
            plans.add(plan);
            final code = launcherExit == null ? 0 : await launcherExit();
            // 授权成功才有 daemon 写 port-file（真启动器不做这个：daemon 写。这里模拟 daemon 一侧）
            if (code == 0) {
              File(plan.portFile).writeAsStringSync('41234 token\n');
            }
            return code;
          },
          // 假连接器照真 connectElevatedSession 的判定顺序：先看提权器是否被取消，
          // 再看 port-file 是否已就绪（真链路由 elevation_test 用真 socket 全跑）。
          elevationConnect:
              ({
                required portFile,
                required launcherExit,
                required timeout,
                required interval,
              }) async {
                final code = await launcherExit;
                if (code != 0) {
                  throw ElevationDeniedException('提权进程退出码 $code');
                }
                expect(
                  File(portFile).readAsStringSync(),
                  '41234 token\n',
                  reason: '连接前 port-file 必已由提权侧写出',
                );
                return elevated;
              },
        ),
      ),
    );
    await tester.pumpAndSettle();
    return (local: local, elevated: elevated, plans: plans, replaced: replaced);
  }

  /// 提权流有一跳真实 port-file I/O 与真异步：泵数轮即可（无对话框动画，20 拍足够）。
  Future<void> flush(WidgetTester tester) async {
    for (var i = 0; i < 20; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }
  }

  testWidgets('空列表 + 三平台 ⇒ 提权入口可见（Win/macOS/Linux 同一分支）', (tester) async {
    for (final platform in [
      TargetPlatform.windows,
      TargetPlatform.macOS,
      TargetPlatform.linux,
    ]) {
      await pumpHome(tester, platform: platform);
      expect(find.text('未发现设备'), findsOneWidget, reason: '$platform');
      expect(
        find.text('以管理员身份重启引擎'),
        findsOneWidget,
        reason: '$platform：空列表须给提权入口',
      );
      expect(find.textContaining('权限不足'), findsOneWidget, reason: '$platform');
    }
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('走通：点击入口 → 平台提权 → 换 client → 设备重列（旧 client 已关闭）', (tester) async {
    final ctx = await pumpHome(tester, platform: TargetPlatform.windows);

    await tester.tap(find.text('以管理员身份重启引擎'));
    await flush(tester);

    // 命令构造：Windows 平台 = UAC 命令，含字面 IP 与 --owner-pid（= UI pid）
    expect(ctx.plans.single.executable, 'powershell.exe');
    expect(ctx.plans.single.arguments.take(3), [
      '-NoProfile',
      '-NonInteractive',
      '-EncodedCommand',
    ]);
    expect(ctx.plans.single.portFile, isNotEmpty);
    // 换用提权会话 client：应用层经 onClientReplaced 换用（main.dart 的 app 级 _client），
    // 首页自身用新 client 重列设备
    expect(ctx.replaced.single, same(ctx.elevated));
    expect(ctx.elevated.calls, contains('listDevices'));
    expect(find.text('U 盘 · SanDisk'), findsOneWidget, reason: '提权后设备必须重列出来');
    // 旧（非提权）daemon 是 UI 子进程：换用后必须显式处置，否则残留孤儿进程
    expect(ctx.local.closed, isTrue, reason: '旧 client 必须被关闭');
    // qual-m2-t6 候选 C：换用后旧 client **只准 close**，不得再发任何调用（exact-match 钉住
    // 交接边界；T7 增 daemon.shutdown RPC 后此断言放宽为「恰好一次 shutdown」）。
    expect(
      ctx.local.calls,
      ['listDevices'],
      reason: '旧 client 在换用后只被关闭，不得再被调用（shutdown RPC 归 T7）',
    );
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('取消授权：提权器非零退出 → 「未获得授权」+ 不换 client + 会话目录已清', (tester) async {
    final ctx = await pumpHome(
      tester,
      platform: TargetPlatform.linux,
      launcherExit: () async => 1,
    );

    await tester.tap(find.text('以管理员身份重启引擎'));
    await flush(tester);

    expect(ctx.plans.single.executable, 'pkexec');
    expect(find.textContaining('未获得授权'), findsOneWidget);
    expect(ctx.replaced, isEmpty, reason: '没有提权会话就不该换 client');
    expect(ctx.local.closed, isFalse, reason: '取消后旧 client 仍在用（用户可重试），不得关掉');
    expect(find.text('以管理员身份重启引擎'), findsOneWidget, reason: '失败后仍可重试');
    // 失败路径的会话目录清理（T6 起归 elevateSession 共享流）：残留 = 令牌目录泄漏
    final dir = File(ctx.plans.single.portFile).parent;
    expect(dir.existsSync(), isFalse, reason: '失败时会话目录必须清理');
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('非空列表 ⇒ 无提权入口（正常枚举时不给无谓的 UAC）', (tester) async {
    await pumpHome(
      tester,
      platform: TargetPlatform.windows,
      localDevices: const [_localDevice],
    );

    expect(find.text('local.img'), findsOneWidget);
    expect(find.text('以管理员身份重启引擎'), findsNothing);
    expect(find.textContaining('权限不足'), findsNothing);
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('macOS：已提权仍列不到设备 ⇒ 「完全磁盘访问」提示（osascript ≠ FDA）', (tester) async {
    final ctx = await pumpHome(
      tester,
      platform: TargetPlatform.macOS,
      elevatedDevices: const [], // root 之后仍为空 = 缺 FDA
    );

    await tester.tap(find.text('以管理员身份重启引擎'));
    await flush(tester);

    expect(ctx.plans.single.executable, 'osascript');
    expect(ctx.replaced.single, same(ctx.elevated), reason: '提权本身成功');
    expect(find.textContaining('完全磁盘访问'), findsOneWidget);
    expect(find.textContaining('隐私与安全性'), findsOneWidget);
    expect(find.textContaining('未获得授权'), findsNothing, reason: '提权成功过，不是授权失败');
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('macOS：提权后列表非空 ⇒ 设备照常展示、不误显 FDA 提示（提示只属空列表态）', (tester) async {
    // qual-m2-t6 候选 B：FDA 文案的**时机**（只应出现在「已提权仍空」）此前无反向钉——
    // 若提示在非空列表也渲出，等于对权限已足够的用户误报缺 FDA。
    final ctx = await pumpHome(tester, platform: TargetPlatform.macOS); // 缺省 elevatedDevices 非空
    await tester.tap(find.text('以管理员身份重启引擎'));
    await flush(tester);

    expect(ctx.replaced.single, same(ctx.elevated));
    expect(find.text('U 盘 · SanDisk'), findsOneWidget, reason: '提权后设备须照常列出');
    expect(
      find.textContaining('完全磁盘访问'),
      findsNothing,
      reason: '列表非空 = 权限已足够，FDA 提示不得误显',
    );
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('父级换 client ⇒ 本页状态跟随并重列；同 client 重建不重复请求（didUpdateWidget）', (
    tester,
  ) async {
    // qual-m2-t6 候选 A：main.dart 在扫描页提权重启后 setState 换 app 级 client，
    // 首页须经 didUpdateWidget 跟随（identical 短路两条路径此前均无钉）。
    final a = FakeCoreClient(devices: const [_localDevice]);
    final b = FakeCoreClient(devices: const [_elevatedDevice]);
    await tester.pumpWidget(MaterialApp(home: HomePage(client: a)));
    await tester.pumpAndSettle();
    expect(find.text('local.img'), findsOneWidget);
    expect(a.calls.where((c) => c == 'listDevices'), hasLength(1));

    // 同一 client 的重建（主题/尺寸等引发的父级 rebuild）：identical 短路，不重复拉设备
    await tester.pumpWidget(MaterialApp(home: HomePage(client: a)));
    await tester.pumpAndSettle();
    expect(
      a.calls.where((c) => c == 'listDevices'),
      hasLength(1),
      reason: '同一 client 不得重复请求设备表',
    );

    // 父级换用新 client（扫描页提权重启经 main.dart 的 setState 路径）：
    // 本页必须跟随，并按新 client 重列设备（提权后枚举结果可能不同）
    await tester.pumpWidget(MaterialApp(home: HomePage(client: b)));
    await tester.pumpAndSettle();
    expect(b.calls, contains('listDevices'), reason: '换 client 必须重列设备');
    expect(find.text('U 盘 · SanDisk'), findsOneWidget);
  });

  testWidgets('提权在途页面卸载：成功不弃会话（新 client 显式关闭）、失败不崩（无 dispose 后 setState）', (
    tester,
  ) async {
    // qual-m2-t6 补钉（H7 存活）：用户在授权框久置时关窗 ⇒ 页面先亡、提权后落定。
    // 修前无人钉：成功分支不关 fresh = 无主 root daemon 只靠 --owner-pid 兜底；
    // 失败分支摘 mounted 守卫 = dispose 后 setState 崩溃。
    debugDefaultTargetPlatformOverride = TargetPlatform.linux;

    // ① 成功在途卸载
    final gateOk = Completer<void>();
    final localA = FakeCoreClient(daemonPathOverride: _daemon);
    final elevatedA = FakeCoreClient(devices: const [_elevatedDevice]);
    await tester.pumpWidget(
      MaterialApp(
        home: HomePage(
          client: localA,
          elevationLauncher: (plan) async {
            File(plan.portFile).writeAsStringSync('41234 token\n');
            return 0;
          },
          elevationConnect:
              ({
                required portFile,
                required launcherExit,
                required timeout,
                required interval,
              }) async {
                await gateOk.future;
                return elevatedA;
              },
        ),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('以管理员身份重启引擎'));
    await tester.pump();
    // 提权期间如实禁用 + 示意（认证框可久置）——首页侧此前无钉
    expect(find.text('等待授权…'), findsOneWidget);
    expect(
      tester.widget<FilledButton>(find.byType(FilledButton)).onPressed,
      isNull,
      reason: '提权建立中按钮必须禁用',
    );
    await tester.pumpWidget(const SizedBox()); // 页面卸载
    await tester.pump();
    gateOk.complete();
    for (var i = 0; i < 10; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }
    expect(
      elevatedA.closed,
      isTrue,
      reason: '页面已亡：提权会话必须显式关闭（owner-pid 监督之外的第一道）',
    );

    // ② 失败在途卸载：授权取消落到已销毁 State ⇒ 不得 setState（否则本测试以框架错误失败）
    final gateFail = Completer<void>();
    final localB = FakeCoreClient(daemonPathOverride: _daemon);
    await tester.pumpWidget(
      MaterialApp(
        home: HomePage(
          client: localB,
          elevationLauncher: (plan) => Completer<int>().future,
          elevationConnect:
              ({
                required portFile,
                required launcherExit,
                required timeout,
                required interval,
              }) async {
                await gateFail.future;
                throw ElevationDeniedException('提权进程退出码 1');
              },
        ),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('以管理员身份重启引擎'));
    await tester.pump();
    await tester.pumpWidget(const SizedBox());
    await tester.pump();
    gateFail.complete();
    for (var i = 0; i < 10; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }
    // 走到这里即通过：dispose 后 setState 会以 FlutterError 失败本测试
    expect(localB.closed, isFalse, reason: '失败路径不得关旧 client（页面已亡时同样不关）');
    debugDefaultTargetPlatformOverride = null;
  });
}
