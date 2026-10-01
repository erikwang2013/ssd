import 'protocol.dart';

/// UI 只依赖这个抽象；桌面实现 = IpcCoreClient，移动端 = FfiCoreClient（M4）。
abstract class CoreClient {
  Future<PingResult> ping();
  Future<List<DeviceInfo>> listDevices();
}
