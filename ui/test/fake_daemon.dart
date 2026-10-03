// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// Dart 假 daemon（真 daemon transport.rs 行为的镜像）：认证首行 → 逐行处理 / -32001 即断。
// 供 SocketCoreClient 协议测试（socket_transport_test）与提权会话测试（elevation_test）共用。
// 真 daemon 全链路见 crates/xd-daemon/tests/tcp_session.rs（CI 三平台真跑）。
import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// 假 daemon：`ServerSocket.bind(127.0.0.1, 0)`，首行必须 `{"auth":<token>}`，
/// 否则回 -32001 即断；通过后逐行交给 [onRequest] 应答（可推通知）。
class FakeDaemon {
  FakeDaemon._(this._server, this.token);

  final ServerSocket _server;
  final String token;

  /// 收到的首行（逐字断言握手）。
  final List<String> authLines = [];

  static Future<FakeDaemon> start({
    required String token,
    void Function(Map<String, dynamic> request, void Function(String) send)?
    onRequest,
  }) async {
    final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    final daemon = FakeDaemon._(server, token);
    server.listen((socket) {
      unawaited(daemon._serve(socket, onRequest));
    });
    return daemon;
  }

  int get port => _server.port;

  /// port-file 内容（`<port> <token>`；提权测试的假启动器/半行注入都写它）。
  String get portFileContent => '$port $token';

  Future<void> _serve(
    Socket socket,
    void Function(Map<String, dynamic>, void Function(String))? onRequest,
  ) async {
    void send(String line) => socket.writeln(line);
    var authed = false;
    final lines = socket
        .cast<List<int>>()
        .transform(const Utf8Decoder(allowMalformed: true))
        .transform(const LineSplitter());
    await for (final line in lines) {
      if (!authed) {
        authLines.add(line);
        final ok = (jsonDecode(line) as Map<String, dynamic>)['auth'] == token;
        if (!ok) {
          send(authFailureLine);
          await socket.flush();
          await socket.close();
          return;
        }
        authed = true;
        continue;
      }
      onRequest?.call(jsonDecode(line) as Map<String, dynamic>, send);
    }
  }

  Future<void> close() => _server.close();
}

/// 认证失败行（真 daemon 逐字：id=null + -32001 + 立即断开）。
const String authFailureLine =
    '{"jsonrpc":"2.0","id":null,"error":{"code":-32001,"message":"Authentication failed"}}';

/// ping 应答（逐字信封；通知与响应抢序由客户端容忍）。
String pongLine(Map<String, dynamic> req) =>
    '{"jsonrpc":"2.0","id":${req['id']},"result":{"pong":true,"version":"fake","protocol":1}}';
