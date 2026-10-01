import 'dart:convert';

class DeviceInfo {
  const DeviceInfo({
    required this.id,
    required this.name,
    required this.kind,
    required this.sizeBytes,
    required this.removable,
    this.fsGuess,
  });

  final String id;
  final String name;
  final String kind; // physical | volume | image
  final int sizeBytes;
  final bool removable;
  final String? fsGuess;

  factory DeviceInfo.fromJson(Map<String, dynamic> json) => DeviceInfo(
        id: json['id'] as String,
        name: json['name'] as String,
        kind: json['kind'] as String,
        sizeBytes: json['sizeBytes'] as int,
        removable: json['removable'] as bool,
        fsGuess: json['fsGuess'] as String?,
      );

  Map<String, dynamic> toJson() => {
        'id': id,
        'name': name,
        'kind': kind,
        'sizeBytes': sizeBytes,
        'removable': removable,
        'fsGuess': fsGuess,
      };
}

class PingResult {
  const PingResult({required this.pong, required this.version, required this.protocol});

  final bool pong;
  final String version;
  final int protocol;

  factory PingResult.fromJson(Map<String, dynamic> json) => PingResult(
        pong: json['pong'] as bool,
        version: json['version'] as String,
        protocol: json['protocol'] as int,
      );
}

class RpcException implements Exception {
  const RpcException(this.code, this.message);
  final int code;
  final String message;
  @override
  String toString() => 'RpcException($code): $message';
}

String encodeRequest({required Object id, required String method, Object? params}) =>
    jsonEncode({'jsonrpc': '2.0', 'id': id, 'method': method, 'params': params});

/// 从一条完整响应消息中取出 result；错误响应抛出 [RpcException]。
Map<String, dynamic> decodeResult(Map<String, dynamic> message) {
  final error = message['error'];
  if (error != null) {
    final e = error as Map<String, dynamic>;
    throw RpcException(e['code'] as int, e['message'] as String);
  }
  return message['result'] as Map<String, dynamic>;
}
