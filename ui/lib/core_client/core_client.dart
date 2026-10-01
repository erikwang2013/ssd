import 'protocol.dart';

/// UI 只依赖这个抽象；桌面实现 = IpcCoreClient，移动端 = FfiCoreClient（M4）。
abstract class CoreClient {
  Future<PingResult> ping();
  Future<List<DeviceInfo>> listDevices();

  /// 释放底层资源（桌面实现：关闭 daemon 进程；测试 fake：空实现）。
  Future<void> close();
}
