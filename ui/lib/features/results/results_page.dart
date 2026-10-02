// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// TODO(T6): 本文件由 T6 替换为完整结果页（虚拟化分页/过滤/多选/质量徽标）。
// 当前为最小桩：仅供扫描页「查看结果」导航先行合入（可编译可测）；
// 构造签名 `ResultsPage({required int taskId})` 保持兼容，T6 整体替换 body。
import 'package:flutter/material.dart';

class ResultsPage extends StatelessWidget {
  const ResultsPage({super.key, required this.taskId});

  final int taskId;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('扫描结果')),
      body: Center(child: Text('任务 #$taskId')),
    );
  }
}
