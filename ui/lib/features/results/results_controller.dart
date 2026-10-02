// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../core_client/core_client.dart';
import '../../core_client/protocol.dart';
import '../../util/errors.dart';

enum ResultsUiState { loading, ready, failed }

/// 结果页状态机：服务端分页 + 客户端质量过滤 + 多选。
///
/// 分页：`pageSize=200`，服务端排序即 `idx` 序（无本地排序）；`loadMore` 的 offset
/// 恒取**已加载数**（非过滤命中数）。
///
/// 过滤语义（qual-m1d-t5 移交，契约 `scan.results` 无 quality 参数——不扩 v1.2）：
/// - `deletedOnly` 走服务端（带参数重拉，`total` = 应用该过滤后的总数）；
/// - `quality` 只能对**已加载集合**做客户端过滤：过滤命中数随 `loadMore` 增量扩大，
///   UI 的 `total` 文案必须写明是「已加载 / 共（服务端 total）」而非全量命中。
/// - 过滤切换（含 deletedOnly 与 quality）**重置分页**：清列表、offset 回 0、清选择。
class ResultsController extends ChangeNotifier {
  ResultsController(this._client, {required this.taskId}) {
    unawaited(reload());
  }

  /// 与契约 limit 上限 1000 无关；计划定值。
  static const int pageSize = 200;

  final CoreClient _client;
  final int taskId;

  ResultsUiState _state = ResultsUiState.loading;
  ResultsUiState get state => _state;

  String? _error;

  /// 首屏失败文案（[ResultsUiState.failed] 时非空）。
  String? get error => _error;

  String? _loadMoreError;

  /// 增量页失败文案：已加载页保留，页脚提示并可重试。
  String? get loadMoreError => _loadMoreError;

  final List<ScanEntry> _loaded = [];

  /// 已加载集合（服务端 idx 序，跨页拼接）。
  List<ScanEntry> get loadedEntries => List.unmodifiable(_loaded);

  int _total = 0;

  /// 服务端 total（已应用 deletedOnly 过滤）——不是质量过滤命中数。
  int get total => _total;

  bool _deletedOnly = false;
  bool get deletedOnly => _deletedOnly;

  final Set<String> _quality = {};

  /// 客户端质量过滤（空集 = 全部）；只作用 [loadedEntries]。
  Set<String> get qualityFilter => Set.unmodifiable(_quality);

  final Set<int> _selected = {};
  Set<int> get selected => Set.unmodifiable(_selected);

  bool _selecting = false;
  bool get selecting => _selecting;

  bool _loadingMore = false;
  bool get loadingMore => _loadingMore;

  int _generation = 0;
  bool _disposed = false;

  /// 质量过滤后的可见集合（过滤切换只改视图，不改已加载集合）。
  List<ScanEntry> get visibleEntries => _quality.isEmpty
      ? List.unmodifiable(_loaded)
      : _loaded.where((e) => _quality.contains(e.quality)).toList();

  bool get hasMore => _loaded.length < _total;

  /// 多选导航参数：只传 idx（写路径铁律——displayName 仅供展示）。
  List<int> get selectedIdxs => _selected.toList()..sort();

  /// 首屏 / 重置后重拉。过滤切换期间的在途响应靠 [_generation] 丢弃（防过期页覆盖）。
  Future<void> reload() async {
    final gen = ++_generation;
    _state = ResultsUiState.loading;
    _error = null;
    _loadMoreError = null;
    // 陈旧 loadMore 提前 return 时靠此行避免卡 spinner——隐式耦合：
    // 增量在途期间发生重置，加载指示须随重置一起归零（loadMore 的 gen 守卫不再回写）。
    _loadingMore = false;
    _loaded.clear();
    _total = 0;
    _notify();
    try {
      final page = await _client.scanResults(
        taskId,
        offset: 0,
        limit: pageSize,
        deletedOnly: _deletedOnly,
      );
      if (_disposed || gen != _generation) return;
      _loaded.addAll(page.entries);
      _total = page.total;
      _state = ResultsUiState.ready;
    } catch (e) {
      if (_disposed || gen != _generation) return;
      _error = describeCoreError(e);
      _state = ResultsUiState.failed;
    }
    _notify();
  }

  /// 触底增量：失败不炸页（已加载保留），页脚可重试。
  Future<void> loadMore() async {
    if (_state != ResultsUiState.ready || _loadingMore || !hasMore) return;
    final gen = _generation;
    final offset = _loaded.length;
    _loadingMore = true;
    _loadMoreError = null;
    _notify();
    try {
      final page = await _client.scanResults(
        taskId,
        offset: offset,
        limit: pageSize,
        deletedOnly: _deletedOnly,
      );
      if (_disposed || gen != _generation) return;
      _loaded.addAll(page.entries);
      _total = page.total;
      if (page.entries.isEmpty && _loaded.length < _total) {
        // 契约保证 offset < total 必有条目：空页 + total 虚高 = 对端自相矛盾。
        // 就地封口，防「继续滚动」对同一 offset 无限重拉（页脚停在可重试/到底态）。
        _total = _loaded.length;
      }
    } catch (e) {
      if (_disposed || gen != _generation) return;
      _loadMoreError = describeCoreError(e);
    }
    _loadingMore = false;
    _notify();
  }

  /// 过滤切换：清选择 + 重置分页（语义显式，qual-m1d-t5 ③）。
  void setDeletedOnly(bool value) {
    if (value == _deletedOnly) return;
    _deletedOnly = value;
    _resetForFilterChange();
  }

  void toggleQuality(String quality) {
    if (!_quality.remove(quality)) _quality.add(quality);
    _resetForFilterChange();
  }

  void _resetForFilterChange() {
    _selecting = false;
    _selected.clear();
    unawaited(reload());
  }

  void enterSelection(int idx) {
    _selecting = true;
    _selected.add(idx);
    _notify();
  }

  void toggleSelected(int idx) {
    if (!_selected.remove(idx)) _selected.add(idx);
    _notify();
  }

  void exitSelection() {
    _selecting = false;
    _selected.clear();
    _notify();
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
