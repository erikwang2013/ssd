// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 预览页状态机（无 UI），按 ext 分派读取策略：
// - 图片（jpg/jpeg/png）：1MiB/片循环 fsRead 到 eof；客户端上限 32MiB——声明超限即不拉，
//   实收越限（声明撒谎）即停，两者同示「文件过大，暂不支持预览」；
// - 文本（txt/log/md/json）：单次前缀 256KiB，utf8 allowMalformed 解码；
// - 其它：不读取，仅信息卡（信息卡恒显，由页面负责）。
//
// 短交付：eof 到而实收 < 声明大小 → 页面提示「实际数据短于声明大小」。exFAT VDL 语义在
// 本层不可知（契约不含 DataLength）——**不得**替引擎猜测未初始化区，只陈述实收事实。
// -32009（条目 > 64MiB，契约拒绝线）与本地 32MiB 上限同一文案。
//
// displayName 铁律（T4 移交）：仅供展示；写路径只传 idx。
import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/foundation.dart';

import '../../core_client/core_client.dart';
import '../../core_client/protocol.dart';
import '../../util/errors.dart';

enum PreviewUiState { loading, ready, failed }

class PreviewController extends ChangeNotifier {
  PreviewController(this._client, {required this.taskId, required this.entry}) {
    unawaited(_load());
  }

  /// 契约单片上限（fs.read 的 length ∈ 1..=1048576）。
  static const int chunkBytes = 1048576;

  /// 图片预览客户端上限：超过不拉（契约服务端拒绝线是 64MiB，本地更保守）。
  static const int imageCapBytes = 32 * 1024 * 1024;

  /// 文本预览前缀字节数。
  static const int textPrefixBytes = 256 * 1024;

  static const Set<String> imageExts = {'jpg', 'jpeg', 'png'};
  static const Set<String> textExts = {'txt', 'log', 'md', 'json'};

  final CoreClient _client;
  final int taskId;
  final ScanEntry entry;

  PreviewUiState _state = PreviewUiState.loading;
  String? _error;
  Uint8List? _imageBytes;
  String? _text;
  bool _tooLarge = false;
  bool _shortDelivery = false;
  bool _disposed = false;

  PreviewUiState get state => _state;

  /// 首屏失败文案（[PreviewUiState.failed] 时非空）。
  String? get error => _error;

  /// 图片分片组装结果（eof 到达或截图上限前的已收字节）。
  Uint8List? get imageBytes => _imageBytes;

  /// 文本前缀（utf8 allowMalformed）。
  String? get text => _text;

  /// 声明 / 实收超上限：不拉或截停。
  bool get tooLarge => _tooLarge;

  /// eof 到而实收 < 声明大小。
  bool get shortDelivery => _shortDelivery;

  bool get _isImage => imageExts.contains(entry.ext.toLowerCase());
  bool get _isText => textExts.contains(entry.ext.toLowerCase());

  Future<void> _load() async {
    try {
      if (_isImage) {
        await _loadImage();
      } else if (_isText) {
        await _loadText();
      }
      _state = PreviewUiState.ready;
    } catch (e) {
      // -32009：条目超过契约 fs.read 上限（64MiB）——与本地上限同一用户体验
      if (e is RpcException && e.code == -32009) {
        _tooLarge = true;
        _state = PreviewUiState.ready;
      } else {
        _error = describeCoreError(e);
        _state = PreviewUiState.failed;
      }
    }
    _notify();
  }

  Future<void> _loadImage() async {
    if (entry.sizeBytes > imageCapBytes) {
      _tooLarge = true;
      return;
    }
    final builder = BytesBuilder(copy: false);
    var offset = 0;
    var eof = false;
    while (true) {
      final chunk = await _client.fsRead(
        taskId,
        entry.idx,
        offset: offset,
        length: chunkBytes,
      );
      builder.add(chunk.bytes);
      offset += chunk.bytes.length;
      eof = chunk.eof;
      if (eof) break;
      // 契约外防御：非 eof 零交付（防死循环）/ 实收越上限（声明撒谎）即停
      if (chunk.bytes.isEmpty || offset >= imageCapBytes) break;
    }
    if (!eof && offset >= imageCapBytes) {
      _tooLarge = true;
      return;
    }
    _imageBytes = builder.takeBytes();
    _shortDelivery = eof && offset < entry.sizeBytes;
  }

  Future<void> _loadText() async {
    final chunk = await _client.fsRead(
      taskId,
      entry.idx,
      offset: 0,
      length: textPrefixBytes,
    );
    _text = utf8.decode(chunk.bytes, allowMalformed: true);
    _shortDelivery = chunk.eof && chunk.bytes.length < entry.sizeBytes;
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
