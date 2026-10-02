// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// TODO(T7): 预览页由 T7 整体实现（图片/文本/信息卡 + 雕刻回读与损坏兜底）。
// 当前为最小桩：仅供 T6 结果页「点击条目」导航先行合入（可编译可测）；
// T7 将在本签名上补 client 参数（同 T6 对 ResultsPage 的做法）。
// 铁律：displayName 仅供展示；本页任何写路径（预览另存）不得消费它——落盘名以
// ExportReportItem.name 为准（T4 移交）。
import 'package:flutter/material.dart';

import '../../core_client/protocol.dart';

class PreviewPage extends StatelessWidget {
  const PreviewPage({super.key, required this.taskId, required this.entry});

  final int taskId;
  final ScanEntry entry;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text(entry.displayName)),
      body: Center(child: Text('预览 #${entry.idx}')),
    );
  }
}
