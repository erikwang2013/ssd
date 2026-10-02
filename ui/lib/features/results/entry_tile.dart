// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'package:flutter/material.dart';

import '../../core_client/protocol.dart';
import '../../util/format.dart';

/// 质量徽标配色：完整=绿 / 可能损坏=橙 / 仅雕刻=蓝灰（计划 Step 1 规范）。
const Color kQualityCompleteColor = Color(0xFF2E7D32);
const Color kQualityDamagedColor = Color(0xFFE65100);
const Color kQualityCarvedColor = Color(0xFF546E7A);

/// 未知 quality（前向兼容）归入「可能损坏」保守档：宁可多提醒，不可误报完好。
({String label, Color color}) qualityBadgeFor(String quality) =>
    switch (quality) {
      'complete' => (label: '完整', color: kQualityCompleteColor),
      'carved' => (label: '仅雕刻', color: kQualityCarvedColor),
      _ => (label: '可能损坏', color: kQualityDamagedColor),
    };

/// 条目质量文案（铁律 1-5 + 拓扑未知兜底，见 results_page.dart 头注）。
/// 返回 null = 不加任何警示（live + complete）。
({String text, Color color})? entryQualityNote(ScanEntry e) {
  if (e.quality == 'carved') {
    return (text: '仅雕刻 · 可能不完整', color: kQualityCarvedColor);
  }
  if (!e.deleted) return null; // live：质量由徽标呈现
  return switch (e.contiguous) {
    // exFAT NoFatChain：连续是规范保证，quality 完整 = 位图逐簇空闲
    true => (text: '已删除 · 簇未被占用（完整性高）', color: kQualityCompleteColor),
    // exFAT 走删除链（stale）：首个被占用/断裂簇即诚实短交付
    false => (text: '已删除 · 按删除链恢复，可能不完整', color: kQualityDamagedColor),
    // fat / 迁移前旧行 / 未知：拓扑无契约证据，不替引擎宣称
    null => (text: '已删除 · 恢复质量见分级', color: Color(0xFF8B97AC)),
  };
}

IconData _iconFor(ScanEntry e) {
  if (e.isDir) return Icons.folder;
  return switch (e.ext.toLowerCase()) {
    'jpg' || 'jpeg' || 'png' => Icons.image,
    _ => Icons.insert_drive_file,
  };
}

/// 结果条目行：类型图标 / displayName / 路径 · 大小 / 质量徽标与警示文案。
/// 多选态（[selecting]）下 leading 换 Checkbox，点行 = 切换选择（由页面决定 onTap 语义）。
class EntryTile extends StatelessWidget {
  const EntryTile({
    super.key,
    required this.entry,
    this.selecting = false,
    this.selected = false,
    this.onTap,
    this.onLongPress,
  });

  final ScanEntry entry;
  final bool selecting;
  final bool selected;
  final VoidCallback? onTap;
  final VoidCallback? onLongPress;

  @override
  Widget build(BuildContext context) {
    final badge = qualityBadgeFor(entry.quality);
    final note = entryQualityNote(entry);
    // 雕刻件 path 恒空：subtitle 只显示大小（计划 Step 1）
    final subtitle = [
      if (entry.path.isNotEmpty) entry.path,
      formatBytes(entry.sizeBytes),
    ].join(' · ');

    return ListTile(
      leading: selecting
          ? Checkbox(value: selected, onChanged: (_) => onTap?.call())
          : Icon(_iconFor(entry), color: const Color(0xFF8B97AC)),
      title: Text(
        entry.displayName,
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
      ),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(subtitle, style: const TextStyle(fontSize: 12)),
          if (note != null)
            Text(note.text, style: TextStyle(fontSize: 12, color: note.color)),
        ],
      ),
      trailing: Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
        decoration: BoxDecoration(
          color: badge.color.withValues(alpha: 0.12),
          borderRadius: BorderRadius.circular(6),
          border: Border.all(color: badge.color),
        ),
        child: Text(
          badge.label,
          style: TextStyle(fontSize: 12, color: badge.color),
        ),
      ),
      selected: selected,
      onTap: onTap,
      onLongPress: onLongPress,
    );
  }
}
