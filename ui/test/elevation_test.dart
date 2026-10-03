// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// M1e-tail Task 4：提权流（Windows UAC / macOS osascript / Linux pkexec → TCP 会话）。
// ① 命令构造纯函数：结构 + 逐字 KAT + **注入用例**（路径含空格/引号/`'`/`$()`/反引号）；
//    macOS 的 shell 串在 /bin/sh 上真跑一遍 argv（引用正确性的真钉子）。
// ② 会话建立：假 daemon（真实 socket）验证两种半行形态重试 / 超时 / 取消快速失败。
// ③ 扫描页 widget：平台注入（debugDefaultTargetPlatformOverride）+ 假启动器写假 port-file
//    + 假连接器 → 断言走通、取消文案「未获得授权」、macOS「已提权但仍缺 FDA」栏。
// **未验证（需真机）**：真 UAC/osascript/pkexec 对话框与提权链、真 FDA 交互。
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/elevation.dart';
import 'package:xiaodun_ui/core_client/ipc_transport.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/features/scan/scan_page.dart';

import 'fake_core_client.dart';
import 'fake_daemon.dart';

const _daemon = '/opt/xiaodun/xd-daemon';
const _pid = 4242;

const _device = DeviceInfo(
  id: 'image:test.img',
  name: 'test.img',
  kind: 'image',
  sizeBytes: 4096,
  removable: false,
);

/// 注入用例：空格/引号/单引号/`$()`/反引号/换行——任一引用层出错即在此暴露。
const _hostilePaths = [
  '/opt/xiaodun/xd-daemon',
  '/Program Files/小盾 数据恢复/xd-daemon.exe',
  "/tmp/it's/daemon",
  r'C:\Users\Erik Doe\AppData\Local\Temp\xiaodun-elev-1\session.port',
  r'C:\a"b\x.port',
  r'/tmp/$(touch /tmp/pwned)',
  '/tmp/`touch /tmp/pwned`',
  '/tmp/a;b&c|d>e',
];

/// `-EncodedCommand` 参数 → 脚本正文（base64 → UTF-16LE）。
String decodeEncodedCommand(ElevationPlan plan) {
  final i = plan.arguments.indexOf('-EncodedCommand');
  expect(i, greaterThanOrEqualTo(0), reason: '必须用 -EncodedCommand（避开引号地狱）');
  final bytes = base64.decode(plan.arguments[i + 1]);
  return String.fromCharCodes([
    for (var j = 0; j < bytes.length; j += 2) bytes[j] | (bytes[j + 1] << 8),
  ]);
}

/// PowerShell 单引号字面量解析器（`''` = 一个 `'`）：从 [marker] 之后读到字面量结束。
/// **这就是注入断言**——引用若有破口，解析会在注入处提前收尾，返回值与原文不符。
String psLiteralAfter(String script, String marker) {
  var i = script.indexOf(marker);
  expect(i, greaterThanOrEqualTo(0), reason: '脚本里找不到 `$marker`：$script');
  i += marker.length;
  expect(script[i], "'", reason: '字面量须单引号起始：$script');
  final out = StringBuffer();
  for (i++; i < script.length; i++) {
    if (script[i] != "'") {
      out.write(script[i]);
      continue;
    }
    if (i + 1 < script.length && script[i + 1] == "'") {
      out.write("'");
      i++;
      continue;
    }
    return out.toString();
  }
  fail('字面量未闭合（逃逸）：$script');
}

/// AppleScript 双引号字面量解码（`\\`/`\"`）。
String appleScriptLiteral(String script) {
  const prefix = 'do shell script ';
  expect(script.startsWith(prefix), isTrue, reason: script);
  final body = script.substring(prefix.length);
  expect(
    body.endsWith(' with administrator privileges'),
    isTrue,
    reason: '必须带 with administrator privileges（弹管理员授权框）：$script',
  );
  final quoted = body.substring(
    0,
    body.length - ' with administrator privileges'.length,
  );
  expect(
    quoted.startsWith('"') && quoted.endsWith('"'),
    isTrue,
    reason: quoted,
  );
  final out = StringBuffer();
  for (var i = 1; i < quoted.length - 1; i++) {
    if (quoted[i] == '\\') {
      i++;
      out.write(quoted[i]);
    } else {
      out.write(quoted[i]);
    }
  }
  return out.toString();
}

/// 参数回显脚本（每个 argv 一行）的路径——当「daemon」用，验证 shell 引用把参数逐字送达。
/// 路径本身含空格：连 daemonPath 的引用一起验。**仅 unix**（CI：ubuntu/macOS）。
String makeArgvEcho() {
  final dir = Directory.systemTemp.createTempSync('xd argv echo ');
  final path = '${dir.path}/echo args.sh';
  File(path)
    ..writeAsStringSync(
      '#!/bin/sh\nfor a in "\$@"; do printf \'%s\\n\' "\$a"; done\n',
    )
    ..createSync(recursive: false);
  Process.runSync('chmod', ['755', path]);
  return path;
}

/// 校验 port-file 路径确实以「格式良好的单行」出现（启动器侧读它判就绪的形态）。
void expectPortFileWritten(String path, FakeDaemon daemon) {
  final content = File(path).readAsStringSync();
  expect(SocketCoreClient.parsePortFile(content).port, daemon.port);
  expect(SocketCoreClient.parsePortFile(content).token, daemon.token);
}

void main() {
  group('提权命令构造', () {
    test('windowsPlan：EncodedCommand 结构 + KAT + 注入面', () {
      final plan = windowsPlan(
        daemonPath: _daemon,
        portFile:
            r'C:\Users\Erik Doe\AppData\Local\Temp\xiaodun-elev-1\session.port',
        ownerPid: _pid,
      );
      expect(plan.executable, 'powershell.exe');
      expect(plan.arguments.take(3), [
        '-NoProfile',
        '-NonInteractive',
        '-EncodedCommand',
      ]);

      final script = decodeEncodedCommand(plan);
      // KAT（逐字）：单串 ArgumentList + 单引号字面量 + Win32 引用后的 port-file
      expect(
        script,
        'Start-Process -Verb RunAs -WindowStyle Hidden '
        "-FilePath '/opt/xiaodun/xd-daemon' "
        r"-ArgumentList '--listen 127.0.0.1:0 --port-file "
        r'"C:\Users\Erik Doe\AppData\Local\Temp\xiaodun-elev-1\session.port" '
        "--owner-pid $_pid'",
      );
      expect(
        script,
        contains('--listen 127.0.0.1:0'),
        reason: '字面 IP，不得 localhost',
      );
    });

    test('windowsPlan：注入用例逐个还原（启动器读到原值，脚本无法逃逸）', () {
      for (final silly in _hostilePaths) {
        final plan = windowsPlan(
          daemonPath: silly,
          portFile: silly,
          ownerPid: _pid,
        );
        final script = decodeEncodedCommand(plan);
        expect(psLiteralAfter(script, '-FilePath '), silly, reason: silly);
        // 第二个字面量 = daemon 参数串；其内 port-file 经 Win32 引用（含空格/引号才加引号）
        final args = psLiteralAfter(script, '-ArgumentList ');
        expect(
          args,
          '--listen 127.0.0.1:0 --port-file ${_win32Quoted(silly)} --owner-pid $_pid',
          reason: silly,
        );
      }
    });

    test('macosPlan：osascript 结构 + shell 串在 /bin/sh 上真跑（argv 逐字）', () async {
      final daemon = makeArgvEcho();
      final plan = macosPlan(
        daemonPath: daemon,
        portFile: '/tmp/xiaodun elev/session.port',
        ownerPid: _pid,
      );
      expect(plan.executable, 'osascript');
      expect(plan.arguments.first, '-e');

      final shell = appleScriptLiteral(plan.arguments[1]);
      final out = await Process.run('/bin/sh', ['-c', shell]);
      expect(out.exitCode, 0, reason: '${out.stderr}');
      expect((out.stdout as String).trim().split('\n'), [
        '--listen',
        '127.0.0.1:0',
        '--port-file',
        '/tmp/xiaodun elev/session.port',
        '--owner-pid',
        '$_pid',
      ]);
    });

    test('macosPlan：注入用例——argv 原样到达且不执行（无 /tmp/pwned）', () async {
      final pwned = File('/tmp/pwned');
      if (pwned.existsSync()) pwned.deleteSync();
      final daemon = makeArgvEcho();
      for (final silly in _hostilePaths) {
        final plan = macosPlan(
          daemonPath: daemon,
          portFile: silly,
          ownerPid: _pid,
        );
        final shell = appleScriptLiteral(plan.arguments[1]);
        expect(shell, contains('127.0.0.1:0'), reason: '字面 IP，不得 localhost');
        final out = await Process.run('/bin/sh', ['-c', shell]);
        expect(out.exitCode, 0, reason: '$silly → ${out.stderr}');
        // 六个 argv 逐字：port-file 必须作为**单个**参数原样到达（引用破口 ⇒ 拆成多个）
        expect((out.stdout as String).trim().split('\n'), [
          '--listen',
          '127.0.0.1:0',
          '--port-file',
          silly,
          '--owner-pid',
          '$_pid',
        ], reason: silly);
      }
      expect(pwned.existsSync(), isFalse, reason: '注入用例不得真的执行');
    });

    test('linuxPlan：argv 逐字（pkexec 不经 shell，无引用问题）', () {
      final plan = linuxPlan(
        daemonPath: '/usr/libexec/xiaodun/xd-daemon',
        portFile: '/tmp/xiaodun-elev-1/session.port',
        ownerPid: _pid,
      );
      expect(plan.executable, 'pkexec');
      expect(plan.arguments, [
        '/usr/libexec/xiaodun/xd-daemon',
        '--listen',
        '127.0.0.1:0',
        '--port-file',
        '/tmp/xiaodun-elev-1/session.port',
        '--owner-pid',
        '$_pid',
      ]);
      expect(plan.portFile, '/tmp/xiaodun-elev-1/session.port');
    });

    test('elevationPlanFor：按平台分派（三平台各一）', () {
      for (final (platform, exe) in [
        (TargetPlatform.windows, 'powershell.exe'),
        (TargetPlatform.macOS, 'osascript'),
        (TargetPlatform.linux, 'pkexec'),
      ]) {
        final plan = elevationPlanFor(
          platform,
          daemonPath: _daemon,
          portFile: '/tmp/f',
          ownerPid: _pid,
        );
        expect(plan.executable, exe, reason: '$platform');
      }
    });
  });

  group('提权会话建立', () {
    test('成功：port-file 就绪 → 握手 ping 通，返回新 client', () async {
      final daemon = await FakeDaemon.start(
        token: 'tok',
        onRequest: (req, send) => send(pongLine(req)),
      );
      addTearDown(daemon.close);
      final dir = Directory.systemTemp.createTempSync('xd_elev');
      addTearDown(() => dir.deleteSync(recursive: true));
      final pf = File('${dir.path}/session.port')
        ..writeAsStringSync(daemon.portFileContent);

      final client = await connectElevatedSession(
        portFile: pf.path,
        launcherExit: Future.value(0),
        timeout: const Duration(seconds: 5),
        interval: const Duration(milliseconds: 10),
      );
      addTearDown(client.close);
      expect((await client.ping()).pong, isTrue);
      expect(daemon.authLines.single, '{"auth":"tok"}');
    });

    test('半行形态①：只有端口（1 段，FormatException）→ 重试到完整行', () async {
      final daemon = await FakeDaemon.start(
        token: 'tok',
        onRequest: (req, send) => send(pongLine(req)),
      );
      addTearDown(daemon.close);
      final dir = Directory.systemTemp.createTempSync('xd_elev');
      addTearDown(() => dir.deleteSync(recursive: true));
      final pf = File('${dir.path}/session.port')
        ..writeAsStringSync('${daemon.port}\n'); // 半行：token 还没写出来

      Future<void> finishWrite() async {
        await Future<void>.delayed(const Duration(milliseconds: 60));
        pf.writeAsStringSync(daemon.portFileContent);
      }

      final client = await connectElevatedSession(
        portFile: pf.path,
        launcherExit: finishWrite().then((_) => 0),
        timeout: const Duration(seconds: 5),
        interval: const Duration(milliseconds: 10),
      );
      addTearDown(client.close);
      expect((await client.ping()).pong, isTrue);
    });

    test('半行形态②：token 被截断（2 段但值不全 → 握手 -32001）→ 重试到完整行', () async {
      final daemon = await FakeDaemon.start(
        token: 'fulltoken',
        onRequest: (req, send) => send(pongLine(req)),
      );
      addTearDown(daemon.close);
      final dir = Directory.systemTemp.createTempSync('xd_elev');
      addTearDown(() => dir.deleteSync(recursive: true));
      final pf = File('${dir.path}/session.port')
        ..writeAsStringSync('${daemon.port} full'); // 截断 token = 读到了但认证必败

      Future<void> finishWrite() async {
        await Future<void>.delayed(const Duration(milliseconds: 60));
        pf.writeAsStringSync(daemon.portFileContent);
      }

      final client = await connectElevatedSession(
        portFile: pf.path,
        launcherExit: finishWrite().then((_) => 0),
        timeout: const Duration(seconds: 5),
        interval: const Duration(milliseconds: 10),
      );
      addTearDown(client.close);
      expect((await client.ping()).pong, isTrue);
      expect(
        daemon.authLines,
        contains('{"auth":"full"}'),
        reason: '截断 token 确实发过握手（这正是必须重试的形态）',
      );
    });

    test('超时：port-file 始终不出现 → ElevationTimeoutException', () async {
      final dir = Directory.systemTemp.createTempSync('xd_elev');
      addTearDown(() => dir.deleteSync(recursive: true));
      final sw = Stopwatch()..start();
      await expectLater(
        connectElevatedSession(
          portFile: '${dir.path}/never.port',
          launcherExit: Future.value(0),
          timeout: const Duration(milliseconds: 120),
          interval: const Duration(milliseconds: 10),
        ),
        throwsA(isA<ElevationTimeoutException>()),
      );
      expect(sw.elapsedMilliseconds, lessThan(2000), reason: '按注入时限收口，不悬挂');
    });

    test('用户取消：提权器非零退出且无会话 → 快速 ElevationDeniedException', () async {
      final dir = Directory.systemTemp.createTempSync('xd_elev');
      addTearDown(() => dir.deleteSync(recursive: true));
      final sw = Stopwatch()..start();
      await expectLater(
        connectElevatedSession(
          portFile: '${dir.path}/never.port',
          launcherExit: Future.value(1), // UAC/polkit 被取消
          timeout: const Duration(seconds: 30),
          interval: const Duration(milliseconds: 10),
        ),
        throwsA(isA<ElevationDeniedException>()),
      );
      expect(sw.elapsedMilliseconds, lessThan(3000), reason: '取消不必白等 30s 超时');
    });

    test('spawnElevation 真起进程：无输出、退出码原样返回', () async {
      final plan = Platform.isWindows
          ? const ElevationPlan('cmd', ['/c', 'exit 7'], portFile: 'x')
          : const ElevationPlan('/bin/sh', ['-c', 'exit 7'], portFile: 'x');
      expect(await spawnElevation(plan), 7);
    });
  });

  group('扫描页提权流', () {
    // `debugDefaultTargetPlatformOverride` 必须在**测试体内**清掉：binding 在 testWidgets
    // 体结束、addTearDown 之前就校验 debug 变量（否则报「foundation debug variable changed」）。
    // 这条兜底只防早失败漏清导致的跨测试串台。
    tearDown(() => debugDefaultTargetPlatformOverride = null);

    /// -32001 引导 → [授权后重试] 的公共铺垫：Fake 客户端（带 daemonPath）+ 假启动器
    /// （写假 port-file）+ 假连接器（回提权会话 client）。
    Future<
      ({
        FakeCoreClient local,
        FakeCoreClient elevated,
        List<ElevationPlan> plans,
        List<String> portFilesSeen,
      })
    >
    pumpElevation(
      WidgetTester tester, {
      required TargetPlatform platform,
      Future<int> Function(int attempt)? launcherExit,
      Object? Function(String method)? elevatedFailWith,
    }) async {
      debugDefaultTargetPlatformOverride = platform;
      var attempts = 0;
      final local = FakeCoreClient(
        daemonPathOverride: _daemon,
        failWith: (method) => method == 'scanStart' && attempts++ == 0
            ? const RpcException(-32001, 'Device permission denied')
            : null,
      );
      final elevated = FakeCoreClient(failWith: elevatedFailWith);
      final plans = <ElevationPlan>[];
      final seen = <String>[];
      await tester.pumpWidget(
        MaterialApp(
          home: ScanPage(
            client: local,
            device: _device,
            elevationLauncher: (plan) async {
              plans.add(plan);
              final code = launcherExit == null
                  ? 0
                  : await launcherExit(plans.length);
              // 授权成功才有 daemon 写 port-file（真启动器不做这个：daemon 写。这里模拟 daemon 一侧）
              if (code == 0) {
                File(plan.portFile).writeAsStringSync('41234 token\n');
              }
              return code;
            },
            // 假连接器照真 connectElevatedSession 的判定顺序：先看提权器是否被取消，
            // 再看 port-file 是否已就绪（真链路由「提权会话建立」组用真 socket 全跑）。
            elevationConnect:
                ({
                  required portFile,
                  required launcherExit,
                  required timeout,
                  required interval,
                }) async {
                  seen.add(portFile);
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
      await tester.pump();
      return (
        local: local,
        elevated: elevated,
        plans: plans,
        portFilesSeen: seen,
      );
    }

    /// 提权流有一跳真实 port-file I/O 与假定时器：泵数轮即可（间隔在连接器里被替换）。
    /// 总时长须盖过对话框退场动画（150ms）——否则已关闭的对话框还在树里。
    Future<void> flush(WidgetTester tester) async {
      for (var i = 0; i < 20; i++) {
        await tester.pump(const Duration(milliseconds: 20));
      }
    }

    testWidgets('走通：-32001 → 授权对话框 → 平台提权 → 提权会话 client 重试 scanStart', (
      tester,
    ) async {
      final ctx = await pumpElevation(tester, platform: TargetPlatform.windows);
      await tester.tap(find.text('开始扫描'));
      await flush(tester);
      expect(find.text('需要管理员权限访问该设备'), findsOneWidget);

      await tester.tap(find.text('授权后重试'));
      await flush(tester);

      // 命令构造：Windows 平台 = UAC 命令，含字面 IP 与 --owner-pid
      expect(ctx.plans.single.executable, 'powershell.exe');
      final script = decodeEncodedCommand(ctx.plans.single);
      expect(script, contains('Start-Process -Verb RunAs'));
      expect(script, contains('--listen 127.0.0.1:0'));
      expect(script, contains('--owner-pid $pid'));
      // 走通：提权会话 client 承接了重试的 scanStart，页面进入扫描中
      expect(
        ctx.elevated.calls,
        contains('scanStart(image:test.img, mode:quick)'),
      );
      expect(find.text('暂停'), findsOneWidget);
      await tester.pumpWidget(const SizedBox());
      await tester.pump();
      debugDefaultTargetPlatformOverride = null;
    });

    testWidgets('取消授权：提权器非零退出 → 「未获得授权」+ 可重试，不换 client', (tester) async {
      final ctx = await pumpElevation(
        tester,
        platform: TargetPlatform.linux,
        launcherExit: (_) async => 1,
      );
      await tester.tap(find.text('开始扫描'));
      await flush(tester);
      await tester.tap(find.text('授权后重试'));
      await flush(tester);

      expect(ctx.plans.single.executable, 'pkexec');
      expect(find.textContaining('未获得授权'), findsOneWidget);
      expect(find.text('重新扫描'), findsOneWidget);
      expect(ctx.elevated.calls, isEmpty, reason: '没有提权会话就不该重试到新 client 上');
      await tester.pumpWidget(const SizedBox());
      await tester.pump();
      debugDefaultTargetPlatformOverride = null;
    });

    testWidgets('macOS：已提权仍 -32001 → 「已提权但仍缺完全磁盘访问」栏，不再弹提权框', (tester) async {
      final ctx = await pumpElevation(
        tester,
        platform: TargetPlatform.macOS,
        elevatedFailWith: (method) => method == 'scanStart'
            ? const RpcException(-32001, 'Device permission denied')
            : null,
      );
      await tester.tap(find.text('开始扫描'));
      await flush(tester);
      await tester.tap(find.text('授权后重试'));
      await flush(tester);

      expect(ctx.plans.single.executable, 'osascript');
      // 会话目录必须**真为** 0700：`createTempSync` 跟随 umask（实测 0002 ⇒ 0775），
      // 同组用户可替换 session.port ⇒ 控制器建目录后显式 chmod（见 scan_controller）。
      expect(
        File(ctx.portFilesSeen.single).parent.statSync().mode & 0x1FF,
        0x1C0,
        reason: '会话目录须显式 chmod 0700，不得听凭 umask',
      );
      expect(find.textContaining('已提权但仍缺完全磁盘访问'), findsOneWidget);
      expect(find.textContaining('隐私与安全性'), findsOneWidget);
      expect(
        find.text('需要管理员权限访问该设备'),
        findsNothing,
        reason: '提权已完成：再弹提权框没有意义',
      );
      await tester.pumpWidget(const SizedBox());
      await tester.pump();
      debugDefaultTargetPlatformOverride = null;
    });
  });
}

/// Win32 命令行引用（测试侧独立实现：与被测代码不共享，避免同错同过）。
String _win32Quoted(String s) {
  if (s.isNotEmpty && !s.contains(RegExp(r'[ \t"]'))) return s;
  final out = StringBuffer('"');
  var backslashes = 0;
  for (final ch in s.split('')) {
    if (ch == '\\') {
      backslashes++;
      continue;
    }
    if (ch == '"') {
      out.write('${'\\' * (backslashes * 2 + 1)}"');
    } else {
      out
        ..write('\\' * backslashes)
        ..write(ch);
    }
    backslashes = 0;
  }
  return (out..write('${'\\' * (backslashes * 2)}"')).toString();
}
