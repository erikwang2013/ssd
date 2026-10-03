// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 报告视图（done/canceled）：三计数块 + 降级/失败清单（status 图标/名称/reason）
// + [打开目标文件夹] [完成]。
//
// 落盘名铁律（T4 移交）：清单一切名字用 ExportReportItem.name（实际落盘名），
// 绝不消费 displayName；成功件不在 items 里（契约如此）——「打开目标文件夹」只按
// 目录打开，不按名定位成功件。
import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';

import '../../core_client/protocol.dart';
import '../results/entry_tile.dart'
    show kQualityCompleteColor, kQualityDamagedColor;

const Color kReportFailedColor = Color(0xFFC62828);

class ReportView extends StatelessWidget {
  const ReportView({super.key, required this.report, required this.onDone});

  final ExportFinished report;
  final VoidCallback onDone;

  @override
  Widget build(BuildContext context) {
    final canceled = report.canceled;
    return ListView(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 16),
      children: [
        Text(
          canceled ? '已取消' : '恢复完成',
          style: const TextStyle(fontSize: 16, fontWeight: FontWeight.w600),
        ),
        if (canceled)
          const Text(
            '已完成的文件保留在目标文件夹',
            style: TextStyle(fontSize: 12, color: Color(0xFF8B97AC)),
          ),
        const SizedBox(height: 12),
        Row(
          children: [
            _CountBlock(
              keyName: 'count-succeeded',
              count: report.succeeded,
              label: '成功',
              color: kQualityCompleteColor,
            ),
            _CountBlock(
              keyName: 'count-degraded',
              count: report.degraded,
              label: '降级',
              color: kQualityDamagedColor,
            ),
            _CountBlock(
              keyName: 'count-failed',
              count: report.failed,
              label: '失败',
              color: kReportFailedColor,
            ),
          ],
        ),
        const SizedBox(height: 8),
        if (report.itemsTruncated)
          const Text(
            '清单过长，仅显示部分降级/失败件',
            style: TextStyle(fontSize: 12, color: Color(0xFF8B97AC)),
          ),
        for (final item in report.items) _itemRow(item),
        const SizedBox(height: 16),
        Row(
          children: [
            OutlinedButton.icon(
              onPressed: () => unawaited(_openTargetFolder(context)),
              icon: const Icon(Icons.folder_open, size: 18),
              label: const Text('打开目标文件夹'),
            ),
            const Spacer(),
            FilledButton(onPressed: onDone, child: const Text('完成')),
          ],
        ),
      ],
    );
  }

  /// 桌面 Linux：xdg-open 目录；其他平台/失败 → 提示手动打开（不静默）。
  Future<void> _openTargetFolder(BuildContext context) async {
    final dir = report.targetDir;
    if (Platform.isLinux) {
      try {
        await Process.start('xdg-open', [dir], mode: ProcessStartMode.detached);
        return;
      } catch (_) {
        // 落到手动提示
      }
    }
    if (!context.mounted) return;
    ScaffoldMessenger.of(context)
        .showSnackBar(SnackBar(content: Text('请手动打开文件夹：$dir')));
  }

  Widget _itemRow(ExportReportItem item) {
    final degraded = item.status != 'failed';
    return ListTile(
      dense: true,
      contentPadding: EdgeInsets.zero,
      leading: Icon(
        degraded ? Icons.warning_amber_rounded : Icons.error_outline,
        color: degraded ? kQualityDamagedColor : kReportFailedColor,
      ),
      title: Text(item.name, style: const TextStyle(fontSize: 14)),
      subtitle: item.reason == null
          ? null
          : Text(item.reason!, style: const TextStyle(fontSize: 12)),
    );
  }
}

class _CountBlock extends StatelessWidget {
  const _CountBlock({
    required this.keyName,
    required this.count,
    required this.label,
    required this.color,
  });

  final String keyName;
  final int count;
  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) {
    return Expanded(
      child: Column(
        children: [
          Text(
            '$count',
            key: ValueKey(keyName),
            style: TextStyle(
              fontSize: 22,
              fontWeight: FontWeight.w600,
              color: color,
            ),
          ),
          Text(
            label,
            style: const TextStyle(fontSize: 12, color: Color(0xFF8B97AC)),
          ),
        ],
      ),
    );
  }
}
