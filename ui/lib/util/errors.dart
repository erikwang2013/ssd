// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:async';

import '../core_client/protocol.dart';

/// 错误展示映射（T5 移交，T6/T8 共用）：传输层文案保持诊断原样，展示层在此翻译。
/// - `StateError`：daemon 退出（-15 是用户在途退出的正常路径，直接 `'$e'`
///   会渲染 `Bad state: daemon exited with code -15`）；
/// - `TimeoutException`：close 后的新调用是挂 10s 超时，非 StateError；
/// - `RpcException`：只显示契约 `message`（-32002/-32005/-32003… 文案自带上下文，
///   不加 `RpcException(code):` 前缀）；
/// - 其余：`'$error'`。
String describeCoreError(Object error) {
  if (error is StateError) return '核心服务已退出，请重启应用';
  if (error is TimeoutException) return '核心服务无响应';
  if (error is RpcException) return error.message;
  return '$error';
}
