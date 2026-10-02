// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// TODO(T8): 恢复页由 T8 整体实现（目标选择/导出进度/报告视图）。
// 当前为最小桩：仅供 T6 结果页「恢复所选」导航先行合入（可编译可测）；
// T8 将在本签名上补 client 参数（同 T6 对 ResultsPage 的做法）。
// 写路径铁律（T4/T6 移交）：只按 idx 定位条目；报告中「打开目标文件夹」等动作
// 一律用 ExportReportItem.name（实际落盘名），不得消费 displayName。
import 'package:flutter/material.dart';

class RecoverPage extends StatelessWidget {
  const RecoverPage({super.key, required this.taskId, required this.idxs});

  final int taskId;
  final List<int> idxs;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('恢复文件')),
      body: Center(child: Text('恢复任务 #$taskId · ${idxs.length} 项 · idx $idxs')),
    );
  }
}
