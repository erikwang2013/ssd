// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 恢复页：目标选择 → 开始恢复 → 进度（done/total + writtenBytes）→ 报告
// （三计数 + 降级/失败清单）或错误引导。状态机/通知订阅见 recover_controller.dart，
// 报告视图见 report_view.dart。
//
// displayName 铁律（T4 移交）：写路径只传 idx；报告落盘名一律 ExportReportItem.name。
import 'dart:async';

import 'package:flutter/material.dart';

import '../../core_client/core_client.dart';
import '../../util/format.dart';
import 'recover_controller.dart';
import 'report_view.dart';

class RecoverPage extends StatefulWidget {
  const RecoverPage({
    super.key,
    required this.client,
    required this.taskId,
    required this.idxs,
  });

  final CoreClient client;
  final int taskId;
  final List<int> idxs;

  @override
  State<RecoverPage> createState() => _RecoverPageState();
}

class _RecoverPageState extends State<RecoverPage> {
  late final RecoverController _controller = RecoverController(
    widget.client,
    taskId: widget.taskId,
    idxs: widget.idxs,
  );

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _controller,
      builder: (context, _) => Scaffold(
        appBar: AppBar(title: const Text('恢复文件')),
        body: Column(
          children: [
            _summaryCard(),
            _targetRow(),
            Expanded(child: _section()),
          ],
        ),
      ),
    );
  }

  /// 卡片：源任务 · 已选 K 项 ·（启动后）预计字节（estimatedBytes）。
  Widget _summaryCard() {
    final bytes = _controller.estimatedBytes;
    final estimate = bytes == null ? '' : ' · 预计 ${formatBytes(bytes)}';
    return Card(
      margin: const EdgeInsets.fromLTRB(16, 12, 16, 4),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Text(
          '源任务 #${widget.taskId} · 已选 ${widget.idxs.length} 项$estimate',
        ),
      ),
    );
  }

  Widget _targetRow() {
    final dir = _controller.targetDir;
    final exporting = _controller.state == RecoverUiState.exporting;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 4, 16, 4),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  dir ?? '选择目标文件夹…',
                  style: TextStyle(
                    fontSize: 14,
                    color: dir == null
                        ? const Color(0xFF8B97AC)
                        : const Color(0xFF1F2937),
                  ),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
              ),
              TextButton(
                onPressed: exporting
                    ? null
                    : () => unawaited(_controller.selectTarget()),
                child: const Text('选择'),
              ),
            ],
          ),
          if (dir != null)
            const Text(
              '请勿选源设备所在的盘（导出前建议先卸载源盘）',
              style: TextStyle(fontSize: 12, color: Color(0xFF8B97AC)),
            ),
        ],
      ),
    );
  }

  Widget _section() {
    final controller = _controller;
    switch (controller.state) {
      case RecoverUiState.picking:
      case RecoverUiState.failed:
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (controller.error != null)
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
                child: Text(
                  controller.error!,
                  style: const TextStyle(
                    fontSize: 13,
                    color: kReportFailedColor,
                  ),
                ),
              ),
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
              child: FilledButton(
                onPressed: controller.targetDir == null
                    ? null
                    : () => unawaited(controller.start()),
                child: const Text('开始恢复'),
              ),
            ),
          ],
        );
      case RecoverUiState.exporting:
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
              child: LinearProgressIndicator(
                value: controller.total > 0
                    ? controller.done / controller.total
                    : null,
              ),
            ),
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
              child: Text(
                controller.total > 0
                    ? '已完成 ${controller.done}/${controller.total} · '
                          '${formatBytes(controller.writtenBytes)}'
                    : '正在启动…',
                style: const TextStyle(fontSize: 13, color: Color(0xFF8B97AC)),
              ),
            ),
            if (controller.error != null)
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
                child: Text(
                  controller.error!,
                  style: const TextStyle(
                    fontSize: 13,
                    color: kReportFailedColor,
                  ),
                ),
              ),
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
              child: OutlinedButton(
                onPressed: controller.canCancel
                    ? () => unawaited(controller.cancel())
                    : null,
                child: const Text('取消恢复'),
              ),
            ),
          ],
        );
      case RecoverUiState.done:
      case RecoverUiState.canceled:
        return ReportView(
          report: controller.report!,
          onDone: () => Navigator.of(context).pop(),
        );
    }
  }
}
