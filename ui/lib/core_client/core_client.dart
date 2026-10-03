// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'protocol.dart';

/// UI 只依赖这个抽象；桌面实现 = IpcCoreClient，移动端 = FfiCoreClient（M4）。
abstract class CoreClient {
  Future<PingResult> ping();
  Future<List<DeviceInfo>> listDevices();

  Future<ScanStartResult> scanStart(String device, {String mode = 'quick'});
  Future<ScanStatusResult> scanStatus(int taskId);
  Future<ScanResultsPage> scanResults(
    int taskId, {
    int offset = 0,
    int limit = 200,
    bool deletedOnly = false,
  });
  Future<void> scanPause(int taskId);
  Future<void> scanResume(int taskId);
  Future<void> scanCancel(int taskId);
  Future<FsReadResult> fsRead(
    int taskId,
    int idx, {
    int offset = 0,
    int length = 1048576,
  });
  Future<ExportStartResult> exportStart(
    int taskId,
    List<int> idxs,
    String targetDir,
  );
  Future<void> exportCancel(int exportId);

  /// 服务端通知（无 id 行）：scan.progress/scan.finished/export.progress/export.finished。
  Stream<Map<String, dynamic>> get notifications;

  /// daemon 可执行文件路径（提权命令构造用：UAC/osascript/pkexec 都要重新拉起同一个
  /// 二进制）。拿不到路径的实现返回 null ⇒ UI 退回 [restartPrivileged]（同参数 in-place
  /// 重启，pkexec stdio）或不做提权引导。
  String? get daemonPath;

  /// EACCES（-32001）引导：以同一 daemon 路径/参数经 pkexec 重启，返回新 client
  /// （旧进程由实现关闭）；不支持的实现返回 null（UI 不进提权重试路径）。
  Future<CoreClient?> restartPrivileged();

  /// 释放底层资源（桌面实现：关闭 daemon 进程；测试 fake：空实现）。
  Future<void> close();
}
