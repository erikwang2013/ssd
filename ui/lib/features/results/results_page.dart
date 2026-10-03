// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 结果浏览页：服务端分页（idx 序）+ 客户端质量过滤 + 多选；条目行见 entry_tile.dart。
//
// 文案铁律（widget 测试逐条断言；源码禁词整串零出现由 results_page_test.dart 的 grep 断言把关）：
// 1. 已删除 + 拓扑连续（exFAT NoFatChain，contiguous == true）且 quality == complete
//    →「已删除 · 簇未被占用（完整性高）」；true 但质量非 complete（位图证据不足/簇已被占）
//    一律落兜底保守文案，不得称完整性高（qual-m1d-t6 F1 门控）；
// 2. 已删除 + 非连续（contiguous == false）→「已删除 · 按删除链恢复，可能不完整」；
// 3. 任何文案不得对拓扑作出未经证实的断言（禁词为四字「连…假设」组合，测试运行时拼接构造）；
// 4. 雕刻件 → 仅「仅雕刻 · 可能不完整」，subtitle 不含路径（path 恒空）；
// 5. 存活 + 完整 → 不加任何警示。
// 兜底：contiguous == null（fat / 迁移前旧行 / 未知）→ 保守文案「已删除 · 恢复质量见分级」——
// fat 删除项读取走连续回退、且 fat 有分配表证据，但本页不替引擎宣称拓扑，也不借用 exFAT 的位图措辞。
//
// displayName 铁律（T4 移交）：仅供展示（本页所有下游导航只传 idx / entry，不传 displayName 作写参数）。
//
// 质量过滤语义（qual-m1d-t5 移交）：契约 scan.results 无 quality 参数（不扩 v1.2），
// 质量过滤只作用**已加载集合**，loadMore 增量扩大命中；计数行写明「已加载 / 共（服务端 total）」。
import 'dart:async';

import 'package:flutter/material.dart';

import '../../core_client/core_client.dart';
import '../../core_client/protocol.dart';
import '../preview/preview_page.dart';
import '../recover/recover_page.dart';
import 'entry_tile.dart';
import 'results_controller.dart';

class ResultsPage extends StatefulWidget {
  const ResultsPage({super.key, required this.client, required this.taskId});

  final CoreClient client;
  final int taskId;

  @override
  State<ResultsPage> createState() => _ResultsPageState();
}

class _ResultsPageState extends State<ResultsPage> {
  late final ResultsController _controller = ResultsController(
    widget.client,
    taskId: widget.taskId,
  );
  final ScrollController _scroll = ScrollController();

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_maybeLoadMore);
  }

  @override
  void dispose() {
    _scroll.dispose();
    _controller.dispose();
    super.dispose();
  }

  /// 滚动到 80% 触发增量（在途 / 无更多守卫在 loadMore 内部）。
  void _maybeLoadMore() {
    if (!_scroll.hasClients) return;
    final position = _scroll.position;
    if (position.maxScrollExtent <= 0) return;
    if (position.pixels >= position.maxScrollExtent * 0.8) {
      unawaited(_controller.loadMore());
    }
  }

  void _openPreview(ScanEntry entry) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => PreviewPage(
          client: widget.client,
          taskId: widget.taskId,
          entry: entry,
        ),
      ),
    );
  }

  void _openRecover() {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => RecoverPage(
          client: widget.client,
          taskId: widget.taskId,
          idxs: _controller.selectedIdxs,
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _controller,
      builder: (context, _) => Scaffold(
        appBar: AppBar(
          leading: _controller.selecting
              ? IconButton(
                  icon: const Icon(Icons.close),
                  tooltip: '退出多选',
                  onPressed: _controller.exitSelection,
                )
              : null,
          title: const Text('扫描结果'),
          actions: [
            Center(
              child: Padding(
                padding: const EdgeInsets.only(right: 16),
                child: Text('${_controller.total} 项'),
              ),
            ),
          ],
        ),
        body: Column(
          children: [
            _filterRow(),
            _countLine(),
            Expanded(child: _body()),
          ],
        ),
        bottomNavigationBar: _controller.selecting ? _selectionBar() : null,
      ),
    );
  }

  Widget _filterRow() {
    final controller = _controller;
    const qualities = [
      ('complete', '完整'),
      ('maybeDamaged', '可能损坏'),
      ('carved', '仅雕刻'),
    ];
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
      child: Wrap(
        spacing: 8,
        children: [
          FilterChip(
            label: const Text('全部'),
            selected: !controller.deletedOnly,
            onSelected: (_) => controller.setDeletedOnly(false),
          ),
          FilterChip(
            label: const Text('仅删除'),
            selected: controller.deletedOnly,
            onSelected: (_) => controller.setDeletedOnly(true),
          ),
          for (final (value, label) in qualities)
            FilterChip(
              label: Text(label),
              selected: controller.qualityFilter.contains(value),
              onSelected: (_) => controller.toggleQuality(value),
            ),
        ],
      ),
    );
  }

  /// 计数行写明语义：总量是服务端 total（含 deletedOnly），质量过滤命中只覆盖已加载集合。
  Widget _countLine() {
    final controller = _controller;
    final loaded = controller.loadedEntries.length;
    final text = controller.qualityFilter.isEmpty
        ? '已加载 $loaded / 共 ${controller.total} 项'
        : '过滤命中 ${controller.visibleEntries.length} · '
              '已加载 $loaded / 共 ${controller.total} 项';
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            text,
            style: const TextStyle(fontSize: 12, color: Color(0xFF8B97AC)),
          ),
          if (controller.qualityFilter.isNotEmpty)
            const Text(
              '质量过滤仅作用于已加载项，继续滚动将纳入更多',
              style: TextStyle(fontSize: 11, color: Color(0xFF8B97AC)),
            ),
        ],
      ),
    );
  }

  Widget _body() {
    final controller = _controller;
    if (controller.state == ResultsUiState.loading &&
        controller.loadedEntries.isEmpty) {
      return const Center(child: CircularProgressIndicator());
    }
    if (controller.state == ResultsUiState.failed &&
        controller.loadedEntries.isEmpty) {
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text('加载失败：${controller.error}'),
            const SizedBox(height: 8),
            FilledButton(onPressed: controller.reload, child: const Text('重试')),
          ],
        ),
      );
    }
    if (controller.loadedEntries.isEmpty) {
      return const Center(child: Text('没有找到结果'));
    }
    final visible = controller.visibleEntries;
    if (visible.isEmpty) {
      return Center(
        child: Text('已加载 ${controller.loadedEntries.length} 项中没有符合质量过滤的条目'),
      );
    }
    return ListView.builder(
      controller: _scroll,
      itemCount: visible.length + 1,
      itemBuilder: (context, i) {
        if (i == visible.length) return _footer();
        final entry = visible[i];
        return EntryTile(
          entry: entry,
          selecting: controller.selecting,
          selected: controller.selected.contains(entry.idx),
          onTap: () => controller.selecting
              ? controller.toggleSelected(entry.idx)
              : _openPreview(entry),
          onLongPress: () => controller.enterSelection(entry.idx),
        );
      },
    );
  }

  /// 末项：增量失败提示 / 加载指示 / 已到底 / 继续滚动。
  Widget _footer() {
    final controller = _controller;
    const hint = TextStyle(fontSize: 12, color: Color(0xFF8B97AC));
    if (controller.loadMoreError != null) {
      return Padding(
        padding: const EdgeInsets.all(12),
        child: Row(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            Text('加载失败：${controller.loadMoreError}'),
            TextButton(onPressed: controller.loadMore, child: const Text('重试')),
          ],
        ),
      );
    }
    if (controller.loadingMore) {
      return const Padding(
        padding: EdgeInsets.all(16),
        child: Center(child: CircularProgressIndicator()),
      );
    }
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Center(
        child: Text(controller.hasMore ? '继续滚动加载更多' : '已到底', style: hint),
      ),
    );
  }

  Widget _selectionBar() {
    final controller = _controller;
    return BottomAppBar(
      child: Row(
        children: [
          Text('已选 ${controller.selected.length} 项'),
          const Spacer(),
          FilledButton(
            onPressed: controller.selected.isEmpty ? null : _openRecover,
            child: Text('恢复所选 (${controller.selected.length})'),
          ),
        ],
      ),
    );
  }
}
