// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 预览页：AppBar=displayName；body=加载态 → 图片/文本/「不支持」→ 短交付提示 → 信息卡（恒显）；
// 底部 [恢复此文件] → RecoverPage(taskId, [idx])。
//
// 信息卡字段：名称/路径（雕刻件 path 恒空，不显示行）/大小/状态/质量徽标；雕刻件的
// byteOffset 展示为「偏移」。质量徽标与警示文案复用结果页的纯函数（同一铁律来源）。
//
// displayName 铁律（T4 移交）：仅供展示；本页写路径（恢复导航）只传 idx。
import 'package:flutter/material.dart';

import '../../core_client/core_client.dart';
import '../../core_client/protocol.dart';
import '../../util/format.dart';
import '../recover/recover_page.dart';
import '../results/entry_tile.dart';
import 'preview_controller.dart';

class PreviewPage extends StatefulWidget {
  const PreviewPage({
    super.key,
    required this.client,
    required this.taskId,
    required this.entry,
  });

  final CoreClient client;
  final int taskId;
  final ScanEntry entry;

  @override
  State<PreviewPage> createState() => _PreviewPageState();
}

class _PreviewPageState extends State<PreviewPage> {
  late final PreviewController _controller = PreviewController(
    widget.client,
    taskId: widget.taskId,
    entry: widget.entry,
  );

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _openRecover() {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) =>
            RecoverPage(taskId: widget.taskId, idxs: [widget.entry.idx]),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _controller,
      builder: (context, _) => Scaffold(
        appBar: AppBar(title: Text(widget.entry.displayName)),
        body: Column(
          children: [
            Expanded(child: _previewArea()),
            if (_controller.shortDelivery)
              const Padding(
                padding: EdgeInsets.fromLTRB(16, 4, 16, 4),
                child: Row(
                  children: [
                    Icon(
                      Icons.warning_amber_rounded,
                      size: 16,
                      color: kQualityDamagedColor,
                    ),
                    SizedBox(width: 6),
                    Expanded(
                      child: Text(
                        '实际数据短于声明大小',
                        style: TextStyle(
                          fontSize: 12,
                          color: kQualityDamagedColor,
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            _infoCard(),
          ],
        ),
        bottomNavigationBar: SafeArea(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 12),
            child: FilledButton(
              onPressed: _openRecover,
              child: const Text('恢复此文件'),
            ),
          ),
        ),
      ),
    );
  }

  Widget _previewArea() {
    final controller = _controller;
    if (controller.state == PreviewUiState.loading) {
      return const Center(child: CircularProgressIndicator());
    }
    if (controller.state == PreviewUiState.failed) {
      return Center(child: Text('加载失败：${controller.error}'));
    }
    if (controller.tooLarge) {
      return const Center(child: Text('文件过大，暂不支持预览'));
    }
    final bytes = controller.imageBytes;
    if (bytes != null) {
      return Center(
        child: Image.memory(
          bytes,
          errorBuilder: (context, error, stackTrace) =>
              const Center(child: Text('数据损坏，无法预览')),
        ),
      );
    }
    final text = controller.text;
    if (text != null) {
      return SingleChildScrollView(
        padding: const EdgeInsets.all(12),
        child: SelectableText(text),
      );
    }
    return const Center(child: Text('此类型不支持预览'));
  }

  Widget _infoCard() {
    final entry = widget.entry;
    final note = entryQualityNote(entry);
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.fromLTRB(16, 10, 16, 10),
      decoration: const BoxDecoration(
        border: Border(top: BorderSide(color: Color(0xFFE0E4EC))),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _infoRow('名称', entry.displayName),
          if (entry.path.isNotEmpty) _infoRow('路径', entry.path),
          _infoRow('大小', formatBytes(entry.sizeBytes)),
          _infoRow('状态', entry.deleted ? '已删除' : '存活'),
          if (entry.byteOffset != null) _infoRow('偏移', '${entry.byteOffset}'),
          Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Row(
              children: [
                QualityBadge(quality: entry.quality),
                if (note != null) ...[
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      note.text,
                      style: TextStyle(fontSize: 12, color: note.color),
                    ),
                  ),
                ],
              ],
            ),
          ),
        ],
      ),
    );
  }

  Widget _infoRow(String label, String value) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 2),
    child: Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        SizedBox(
          width: 48,
          child: Text(
            label,
            style: const TextStyle(fontSize: 12, color: Color(0xFF8B97AC)),
          ),
        ),
        Expanded(
          child: Text(
            value,
            style: const TextStyle(fontSize: 12),
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
          ),
        ),
      ],
    ),
  );
}
