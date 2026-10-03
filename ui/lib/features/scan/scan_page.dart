// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'package:flutter/material.dart';

import '../../core_client/core_client.dart';
import '../../core_client/elevation.dart';
import '../../core_client/protocol.dart';
import '../../util/format.dart';
import '../results/results_page.dart';
import 'scan_controller.dart';

/// 扫描页：模式选择 / 真实进度（通知 + 1s 轮询对账）/ 暂停恢复取消 / EACCES 引导。
class ScanPage extends StatefulWidget {
  const ScanPage({
    super.key,
    required this.client,
    required this.device,
    this.onClientReplaced,
    this.elevationLauncher,
    this.elevationConnect,
  });

  final CoreClient client;
  final DeviceInfo device;

  /// 特权重启后应用层换用新 client（EACCES 引导路径）。
  final void Function(CoreClient client)? onClientReplaced;

  /// 测试注入缝（提权器启动/会话连接）：null = 用真实现（UAC/osascript/pkexec + TCP）。
  final ElevationLauncher? elevationLauncher;
  final ElevationSessionConnector? elevationConnect;

  @override
  State<ScanPage> createState() => _ScanPageState();
}

class _ScanPageState extends State<ScanPage> {
  late final ScanController _controller = ScanController(
    widget.client,
    deviceId: widget.device.id,
    onClientReplaced: widget.onClientReplaced,
    elevationLauncher: widget.elevationLauncher,
    elevationConnect: widget.elevationConnect,
  );

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  /// 开始扫描；-32001 → EACCES 对话框，[授权后重试] = 平台提权引导（UAC/osascript/pkexec
  /// → TCP 提权会话）→ 重试 scanStart。**真对话框路径未验证（需真机）。**
  Future<void> _start() async {
    await _controller.start();
    if (!mounted) return;
    while (_controller.needsElevation) {
      final retry = await showDialog<bool>(
        context: context,
        builder: (ctx) => AlertDialog(
          title: const Text('需要管理员权限访问该设备'),
          content: const Text('将弹出系统授权对话框；授权后以管理员身份建立提权会话，然后继续扫描。'),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx, false),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(ctx, true),
              child: const Text('授权后重试'),
            ),
          ],
        ),
      );
      if (!mounted) return;
      if (retry != true) {
        _controller.dismissElevation();
        return;
      }
      await _controller.retryWithPrivileges();
      if (!mounted) return;
    }
  }

  Future<void> _confirmCancel() async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('取消扫描？'),
        content: const Text('扫描将停止。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('继续扫描'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('取消扫描'),
          ),
        ],
      ),
    );
    if (confirmed == true) await _controller.cancel();
  }

  void _openResults() {
    final taskId = _controller.taskId;
    if (taskId == null) return;
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        // 提权会话替换过客户端：必须用控制器**当前**的 client（widget.client 是旧引用）
        builder: (_) => ResultsPage(client: _controller.client, taskId: taskId),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _controller,
      builder: (context, _) => Scaffold(
        appBar: AppBar(title: Text(widget.device.name)),
        body: ListView(
          padding: const EdgeInsets.all(16),
          children: [
            SegmentedButton<String>(
              segments: const [
                ButtonSegment(value: 'quick', label: Text('快速扫描')),
                ButtonSegment(value: 'deep', label: Text('深度扫描')),
              ],
              selected: {_controller.mode},
              onSelectionChanged: _controller.busy
                  ? null
                  : (selection) => _controller.setMode(selection.first),
            ),
            if (_controller.mode == 'deep')
              const Padding(
                padding: EdgeInsets.only(top: 8),
                child: Text(
                  '在未分配空间按文件签名找回（照片/图片）；无文件名，结果标注『仅雕刻』',
                  style: TextStyle(fontSize: 12, color: Color(0xFF8B97AC)),
                ),
              ),
            const SizedBox(height: 16),
            ..._actionArea(),
            ..._progressArea(),
            ..._statusArea(),
          ],
        ),
      ),
    );
  }

  List<Widget> _actionArea() {
    final state = _controller.state;
    switch (state) {
      case ScanUiState.completed:
        return [
          FilledButton(
            onPressed: _openResults,
            child: Text('查看结果 (${_controller.foundCount})'),
          ),
        ];
      case ScanUiState.canceled || ScanUiState.failed:
        return [FilledButton(onPressed: _start, child: const Text('重新扫描'))];
      case ScanUiState.idle ||
          ScanUiState.starting ||
          ScanUiState.scanning ||
          ScanUiState.paused:
        return [
          FilledButton(
            onPressed: _controller.busy ? null : _start,
            child: Text(_controller.busy ? '扫描中…' : '开始扫描'),
          ),
        ];
    }
  }

  List<Widget> _progressArea() {
    final state = _controller.state;
    if (state != ScanUiState.scanning && state != ScanUiState.paused) {
      return const [];
    }
    final total = _controller.totalBytes;
    final read = formatBytes(_controller.readBytes);
    return [
      const SizedBox(height: 16),
      LinearProgressIndicator(value: _controller.percent),
      const SizedBox(height: 8),
      Text(
        total == null || total == 0
            ? '已扫 $read'
            : '已扫 $read / ${formatBytes(total)}',
      ),
      Text('已找到 ${_controller.foundCount} 项'),
      Text('用时 ${formatElapsed(_controller.elapsedMs)}'),
      const SizedBox(height: 8),
      Row(
        children: [
          if (state == ScanUiState.scanning)
            OutlinedButton(
              onPressed: _controller.pause,
              child: const Text('暂停'),
            )
          else
            FilledButton(
              onPressed: _controller.resume,
              child: const Text('恢复'),
            ),
          const SizedBox(width: 8),
          OutlinedButton(onPressed: _confirmCancel, child: const Text('取消')),
        ],
      ),
    ];
  }

  List<Widget> _statusArea() {
    final state = _controller.state;
    final error = _controller.error;
    // 提权会话建立中（认证框可能久置 30s）：如实告知，不显示「扫描中」
    if (_controller.elevationPending) {
      return const [Text('等待授权…')];
    }
    return switch (state) {
      ScanUiState.canceled => const [Text('扫描已取消')],
      ScanUiState.failed => [Text(error == null ? '扫描失败' : '扫描失败：$error')],
      _ => const [],
    };
  }
}
