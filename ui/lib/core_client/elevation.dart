// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import 'core_client.dart';
import 'ipc_transport.dart';

/// 提权命令构造（纯函数，可单测）；**真提权路径未验证（需真机 UAC/polkit/授权对话框）**。
///
/// 三平台同一形态：`<提权器> <daemonPath> --listen 127.0.0.1:0 --port-file <F> --owner-pid <P>`
/// ——UAC/osascript 提权后父进程拿不到子进程 stdio 句柄（也无法继承），TCP 回环会话是唯一
/// 可移植通道（协议与威胁模型见 crates/xd-daemon/src/transport.rs 头注与 docs/security §7/§10）。
///
/// - `--listen` 用**字面 IP** `127.0.0.1:0`（勿 localhost：解析可能得 ::1 或失败）；
/// - `--owner-pid` = UI 进程 pid：daemon 属主监督（UI 亡 ⇒ daemon 自退 + 清 port-file，
///   防 root daemon 无主滞留/多轮提权累积），见 §10；
/// - **不提 `--device`/`--image`**：提权 daemon 靠启动枚举列设备（§10），命令面因此不含任何
///   文件路径参数——pkexec 不校验参数（§3），少一个参数就少一个任意路径面。
class ElevationPlan {
  const ElevationPlan(
    this.executable,
    this.arguments, {
    required this.portFile,
  });

  /// 提权器可执行文件（powershell.exe / osascript / pkexec）。
  final String executable;

  /// 提权器参数（逐字传给 Process.start，不再经 shell）。
  final List<String> arguments;

  /// 会话交接件路径：启动器/测试据此判断会话就绪（其内容已逐字嵌入 [arguments]）。
  final String portFile;
}

/// Windows：UAC 弹窗启动（`Start-Process -Verb RunAs`）。daemon 以 `--listen/--port-file`
/// 会话模式启动（stdio 句柄无法跨提权继承——这就是 TCP 回环传输存在的理由）。
///
/// 用 `-EncodedCommand`（base64 UTF-16LE）避开引号地狱：脚本正文经 PowerShell 单引号字面量
/// 转义（[`_psLiteral`]），daemon 参数合成**一条**命令行字符串再经 [`_win32Argument`] 引用
/// ——`Start-Process -ArgumentList` 传数组会按空格拼串、含空格路径必被拆断（PowerShell 已知
/// 行为），单串形式才是逐字透传。
ElevationPlan windowsPlan({
  required String daemonPath,
  required String portFile,
  required int ownerPid,
}) {
  final daemonArgs =
      '--listen 127.0.0.1:0 --port-file ${_win32Argument(portFile)} '
      '--owner-pid $ownerPid';
  final script =
      'Start-Process -Verb RunAs -WindowStyle Hidden '
      '-FilePath ${_psLiteral(daemonPath)} '
      '-ArgumentList ${_psLiteral(daemonArgs)}';
  return ElevationPlan('powershell.exe', [
    '-NoProfile',
    '-NonInteractive',
    '-EncodedCommand',
    _utf16leBase64(script),
  ], portFile: portFile);
}

/// macOS：管理员授权启动（`osascript do shell script … with administrator privileges`）。
/// 未验证（需真机授权对话框）；**osascript 提权 ≠ 完全磁盘访问（FDA）**——root 后仍可能
/// EPERM，失败 UX 须给「已提权但仍缺完全磁盘访问」一栏（§9/§10）。
ElevationPlan macosPlan({
  required String daemonPath,
  required String portFile,
  required int ownerPid,
}) {
  final shell =
      '${_posixQuoted(daemonPath)} --listen 127.0.0.1:0 '
      '--port-file ${_posixQuoted(portFile)} --owner-pid $ownerPid';
  return ElevationPlan('osascript', [
    '-e',
    'do shell script ${_applescriptQuoted(shell)} with administrator privileges',
  ], portFile: portFile);
}

/// Linux：pkexec（polkit action `com.erik.xiaodun.daemon.run`，文案与白名单见
/// packaging/polkit 与 docs/security §1/§3）。argv 直接给出（不经 shell），无引用问题。
/// 未验证（需真机 polkit + 已安装 policy）。
ElevationPlan linuxPlan({
  required String daemonPath,
  required String portFile,
  required int ownerPid,
}) => ElevationPlan('pkexec', [
  daemonPath,
  '--listen',
  '127.0.0.1:0',
  '--port-file',
  portFile,
  '--owner-pid',
  '$ownerPid',
], portFile: portFile);

/// 平台分派（测试可用 `debugDefaultTargetPlatformOverride` 注入平台）。
ElevationPlan elevationPlanFor(
  TargetPlatform platform, {
  required String daemonPath,
  required String portFile,
  required int ownerPid,
}) => switch (platform) {
  TargetPlatform.windows => windowsPlan(
    daemonPath: daemonPath,
    portFile: portFile,
    ownerPid: ownerPid,
  ),
  TargetPlatform.macOS => macosPlan(
    daemonPath: daemonPath,
    portFile: portFile,
    ownerPid: ownerPid,
  ),
  _ => linuxPlan(
    daemonPath: daemonPath,
    portFile: portFile,
    ownerPid: ownerPid,
  ),
};

/// 提权会话建立超时与轮询间隔（计划裁定：≤30s、500ms——用户可能在认证框上耗时）。
const Duration kElevationTimeout = Duration(seconds: 30);
const Duration kElevationPollInterval = Duration(milliseconds: 500);

/// macOS：osascript 提权 **≠ 完全磁盘访问（FDA）**——root 后仍可能 EPERM（见文件头与 §9/§10）。
/// 扫描页（-32001 且已提权）与首页（已提权仍列不到设备）共用同一文案。
const String kMacosFdaHint = '已提权但仍缺完全磁盘访问（系统设置 > 隐私与安全性）';

/// 提权被拒：提权器启动失败或非零退出（UAC/polkit 被取消/拒绝）。
class ElevationDeniedException implements Exception {
  ElevationDeniedException(this.message);
  final String message;
  @override
  String toString() => message;
}

/// 期限内没有可用会话（授权框久置/daemon 未起来/daemon 起后即退）。
class ElevationTimeoutException implements Exception {
  ElevationTimeoutException(this.message);
  final String message;
  @override
  String toString() => message;
}

/// 提权启动器签名（测试注入假启动器；缺省 [`spawnElevation`]）。
typedef ElevationLauncher = Future<int> Function(ElevationPlan plan);

/// 提权会话连接签名（测试注入假连接器 = widget 测试不碰真实 socket；缺省
/// [`connectElevatedSession`]，真实链路由 elevation_test 的连接器单测用假 daemon 全跑）。
typedef ElevationSessionConnector = Future<CoreClient> Function({
  required String portFile,
  required Future<int> launcherExit,
  required Duration timeout,
  required Duration interval,
});

/// 缺省启动器：起提权进程，排空 stdout/stderr（防管道写满卡死），返回退出码。
///
/// 退出码语义（三平台不一致，故只作**快速失败提示**）：Windows `Start-Process` 在 UAC 结果
/// 之外立即返回 0，授权与否由「port-file 按时出现 + 握手成功」判定；pkexec/osascript 被取消
/// 时非零。启动器抛错（ProcessException：提权器不存在）同样按「被拒」处理，由上层转换。
Future<int> spawnElevation(ElevationPlan plan) async {
  final process = await Process.start(plan.executable, plan.arguments);
  unawaited(process.stdout.drain<void>());
  unawaited(process.stderr.drain<void>());
  return process.exitCode;
}

/// 建提权会话：轮询 port-file → 连接 → 握手探针（ping）→ 返回新 client；失败重试到
/// [timeout] 为止。**两种半行形态都要重试**（T1 移交②）：port-file 只写了一半（1 段 →
/// [SocketCoreClient.parsePortFile] 抛 FormatException）或 token 被截断（2 段但值不全 →
/// 握手 -32001），两种都必须当作「未就绪」等下一拍——Windows 的 port-file 是直写（非原子）。
///
/// [launcherExit] 由调用方先起好（不 await）：非零退出/启动器抛错且本拍连接也没成功 ⇒
/// 立即 [ElevationDeniedException]（用户取消授权不必白等 30s）。
Future<CoreClient> connectElevatedSession({
  required String portFile,
  required Future<int> launcherExit,
  Duration timeout = kElevationTimeout,
  Duration interval = kElevationPollInterval,
}) async {
  final deadline = DateTime.now().add(timeout);
  var denied = false;
  var deniedReason = '';
  unawaited(
    launcherExit.then(
      (code) {
        if (code != 0) {
          denied = true;
          deniedReason = '提权进程退出码 $code';
        }
      },
      onError: (Object e) {
        denied = true;
        deniedReason = '无法启动提权进程：$e';
      },
    ),
  );
  Object? lastError;
  while (DateTime.now().isBefore(deadline)) {
    SocketCoreClient? client;
    try {
      client = await SocketCoreClient.start(portFile: portFile);
      await client.ping(); // 握手探针：截断 token 的 -32001 在此暴露
      return client;
    } catch (e) {
      lastError = e;
      if (client != null) unawaited(client.close()); // 半成品连接不泄漏
    }
    // 让 launcherExit 的回调落定，再判快速失败（用户取消授权不与超时同文案）
    await Future<void>.delayed(Duration.zero);
    if (denied) throw ElevationDeniedException(deniedReason);
    await Future<void>.delayed(interval);
  }
  throw ElevationTimeoutException(
    '未在 ${timeout.inSeconds} 秒内建立提权会话${lastError == null ? '' : '（$lastError）'}',
  );
}

/// 提权会话全流程（扫描页 -32001 引导与首页空列表入口的**唯一**实现）：
/// 0700 会话目录 → [elevationPlanFor] → 起提权进程（与轮询并行）→ 轮询 port-file
/// ≤[timeout]（半行双形态重试）→ TCP 握手（ping 探针）→ 返回提权会话 client。
///
/// 失败（用户取消/超时/传输异常）按原样抛出且**会话目录已清理**；成功时把会话目录交给
/// [onSessionDir]（调用方在会话结束时删整个目录；daemon 自退时删自己的 port-file——清理
/// 归属见 docs/security §10）。旧 client 处置与换用归调用方（各页面的 client 归属不同）。
Future<CoreClient> elevateSession({
  required String daemonPath,
  required int ownerPid,
  ElevationLauncher? launcher,
  ElevationSessionConnector? connect,
  Duration timeout = kElevationTimeout,
  Duration interval = kElevationPollInterval,
  void Function(Directory sessionDir)? onSessionDir,
}) async {
  Directory? dir;
  try {
    // UI 自建 0700 会话目录（建后显式 chmod，防 umask 放宽）：root daemon 写 port-file 时把
    // 属主交还目录属主（本用户），否则 0600 属主=root，UI 读不到令牌（见 portfile.rs）。
    dir = Directory.systemTemp.createTempSync('xiaodun-elev-');
    // `createTempSync` 无 mode 参数且**跟随 umask**（实测 0002 ⇒ 0775）：同组用户可 unlink/
    // 替换 session.port（UI 读前 race）⇒ 显式收紧到 0700。Windows 无 POSIX 位（ACL 见 §10.5）。
    if (!Platform.isWindows) {
      Process.runSync('chmod', ['700', dir.path]);
    }
    final portFile = '${dir.path}/session.port';
    final plan = elevationPlanFor(
      defaultTargetPlatform,
      daemonPath: daemonPath,
      portFile: portFile,
      ownerPid: ownerPid,
    );
    final fresh = await (connect ?? connectElevatedSession)(
      portFile: portFile,
      launcherExit: (launcher ?? spawnElevation)(plan), // 不 await：与轮询并行，取消即刻唤醒
      timeout: timeout,
      interval: interval,
    );
    onSessionDir?.call(dir);
    return fresh;
  } catch (_) {
    if (dir != null) cleanupElevationDir(dir);
    rethrow;
  }
}

/// 会话目录收尾（尽力而为；port-file 清理归属见 docs/security §10：daemon 自退时删自己的
/// port-file，UI 在会话结束后删整个目录）。失败只留一个空目录（系统临时目录有清理策略），
/// 不打断流程。
void cleanupElevationDir(Directory dir) {
  try {
    dir.deleteSync(recursive: true);
  } on FileSystemException {
    // 忽略：目录/文件可能已被并发清理
  }
}

/// Win32 命令行参数引用（CreateProcess 语法）：含空格/制表/引号才加引号；反斜杠仅在引号前
/// 与结尾处加倍（否则会与引号转义粘连）。
String _win32Argument(String s) {
  if (s.isNotEmpty && !s.contains(RegExp(r'[ \t"]'))) return s;
  final out = StringBuffer('"');
  var backslashes = 0;
  for (final ch in s.split('')) {
    if (ch == '\\') {
      backslashes++;
      continue;
    }
    if (ch == '"') {
      out
        ..write('\\' * (backslashes * 2 + 1))
        ..write('"');
    } else {
      out
        ..write('\\' * backslashes)
        ..write(ch);
    }
    backslashes = 0;
  }
  out
    ..write('\\' * (backslashes * 2))
    ..write('"');
  return out.toString();
}

/// PowerShell 单引号字面量（唯一转义 = 引号翻倍；无反斜杠转义 ⇒ 无注入面）。
String _psLiteral(String s) => "'${s.replaceAll("'", "''")}'";

/// POSIX sh 单引号引用（唯一转义 = `'\''`；`$`/反引号/引号在单引号内全部字面化）。
String _posixQuoted(String s) => "'${s.replaceAll("'", "'\\''")}'";

/// AppleScript 双引号字面量：仅 `\` 与 `"` 需转义。
String _applescriptQuoted(String s) =>
    '"${s.replaceAll('\\', '\\\\').replaceAll('"', '\\"')}"';

/// base64(UTF-16LE)——`-EncodedCommand` 的输入编码。
String _utf16leBase64(String s) {
  final bytes = <int>[];
  for (final unit in s.codeUnits) {
    bytes
      ..add(unit & 0xff)
      ..add(unit >> 8);
  }
  return base64.encode(bytes);
}
