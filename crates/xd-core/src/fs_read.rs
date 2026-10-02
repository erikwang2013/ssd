// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 条目分片读取（预览/导出共用）：live/删除 → 引擎 `read_file_range`；雕刻 → carving 回读。
//! 契约上限 `MAX_READ = 1MiB` 由 handlers 校验；内部调用方可放宽（导出 4MiB 片）。
//!
//! 路由裁定：`byte_offset.is_some()` 即雕刻件（其 run 界随进程消失，按**当前**分配表重定位）；
//! 其余按 `FsKind` 反构造引擎条目读取——`ScanEntry.contiguous` 是拓扑承重字段（exfat 真值、
//! fat/雕刻/迁移前旧行 None），None 反构造为 `false`（只信链：**宁可诚实短交付，不猜连续**）。

use std::ops::Range;

use base64::Engine as _;
use xd_device::BlockDevice;

use crate::api::ScanEntry;
use crate::scan_task::FsKind;

pub const MAX_READ: u64 = 1024 * 1024;

/// 读取失败：`TooLarge` 供上层（handlers）映射 -32009；其余一律内部错误。
#[derive(Debug)]
pub enum ReadError {
    TooLarge(u64),
    Internal(String),
}

/// 读取 `[offset, offset+length)`；返回 (bytes, eof)。
/// eof = 交付已到该条目**可得数据的末端**（损坏/短链件可能 < sizeBytes——UI 以此判"可能不完整"）。
pub fn read_entry_range(
    dev: &dyn BlockDevice,
    fs: FsKind,
    entry: &ScanEntry,
    offset: u64,
    length: u64,
) -> Result<(Vec<u8>, bool), ReadError> {
    // 契约层 offset 无上界（length 由 handlers 限 ≤ MAX_READ）：饱和加防溢出 panic，
    // 合法入参下与普通加法同值。
    let want = offset.saturating_add(length);
    let bytes = match entry.byte_offset {
        Some(bo) => {
            // 雕刻件：run 重定位（雕刻期的 run 界不落库；随后续分配变化重解，找不到即实错）
            let runs = unallocated_runs(dev, fs)?;
            let run = runs
                .iter()
                .find(|r: &&Range<u64>| r.contains(&bo))
                .ok_or_else(|| ReadError::Internal("carved offset outside free space".into()))?;
            let kind = xd_carving::Signature::from_ext(&entry.ext)
                .ok_or_else(|| ReadError::Internal("unknown carved ext".into()))?;
            // `want` 为交付上限：回读交付裁决区间的前缀，再切 [offset, want)（超 len 即空尾）
            xd_carving::read_back(dev, run.end, bo, kind, want)
                .map(|all| {
                    let s = offset.min(all.len() as u64) as usize;
                    let e = want.min(all.len() as u64) as usize;
                    all[s..e].to_vec()
                })
                .unwrap_or_default()
        }
        None => match fs {
            FsKind::Fat => {
                let e = to_fat_entry(entry);
                xd_fs_fat::read::read_file_range(dev, &e, offset, length)
                    .map_err(|e| ReadError::Internal(e.to_string()))?
            }
            FsKind::Exfat => {
                let e = to_exfat_entry(entry);
                xd_fs_exfat::read::read_file_range(dev, &e, offset, length)
                    .map_err(|e| ReadError::Internal(e.to_string()))?
            }
        }
        .into_iter()
        .take(length as usize) // 引擎内已 clamp 到条目尾；此为 length 侧防线
        .collect(),
    };
    let end = offset.saturating_add(bytes.len() as u64);
    let eof = (bytes.len() as u64) < length || end >= entry.size_bytes;
    Ok((bytes, eof))
}

/// 空闲区间（雕刻件 run 重定位用；两引擎错误一律收敛为 Internal = -32603）。
fn unallocated_runs(dev: &dyn BlockDevice, fs: FsKind) -> Result<Vec<Range<u64>>, ReadError> {
    match fs {
        FsKind::Fat => xd_fs_fat::freespace::unallocated_runs(dev)
            .map_err(|e| ReadError::Internal(e.to_string())),
        FsKind::Exfat => xd_fs_exfat::freespace::unallocated_runs(dev)
            .map_err(|e| ReadError::Internal(e.to_string())),
    }
}

/// 从契约条目反构造引擎条目（读路径专用；fat 无 NoFatChain 概念，拓扑由 `deleted` 决定）。
fn to_fat_entry(e: &ScanEntry) -> xd_fs_fat::scan::FatEntry {
    xd_fs_fat::scan::FatEntry {
        name: e.name.clone(),
        path: e.path.clone(),
        size_bytes: e.size_bytes,
        first_cluster: e.first_cluster,
        deleted: e.deleted,
        is_dir: e.is_dir,
        quality: match e.quality.as_str() {
            "maybeDamaged" => xd_fs_fat::scan::RecoverQuality::MaybeDamaged,
            _ => xd_fs_fat::scan::RecoverQuality::Complete,
        },
        ext: e.ext.clone(),
    }
}

/// 反构造：`data_length` 取 `size_bytes`（交付上界；读取簇数只需覆盖交付段——尾部 [VDL,DL)
/// 永不交付，少算无碍）；`contiguous` 缺省（fat/雕刻/迁移前旧行）取 **false**：只信链/
/// 走删除链裁定，**不得猜连续**（猜错 = 交付他人数据，违反"宁可漏报不可错报"）。
fn to_exfat_entry(e: &ScanEntry) -> xd_fs_exfat::scan::ExfatEntry {
    xd_fs_exfat::scan::ExfatEntry {
        name: e.name.clone(),
        path: e.path.clone(),
        size_bytes: e.size_bytes,
        data_length: e.size_bytes,
        first_cluster: e.first_cluster,
        deleted: e.deleted,
        is_dir: e.is_dir,
        contiguous: e.contiguous.unwrap_or(false),
        quality: match e.quality.as_str() {
            "maybeDamaged" => xd_fs_exfat::scan::RecoverQuality::MaybeDamaged,
            _ => xd_fs_exfat::scan::RecoverQuality::Complete,
        },
        ext: e.ext.clone(),
    }
}

/// 契约 `bytesBase64` 编码（单一入口：handlers 与测试共用，避免两处各写一份标准表）。
pub fn to_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ScanState;
    use crate::scan_task::ScanManager;
    use crate::store::Store;
    use std::sync::Arc;
    use std::time::Duration;

    fn mgr() -> ScanManager {
        ScanManager::new(Store::open_memory().unwrap(), Arc::new(|_| {}))
    }

    /// 真流程快扫至 completed，返回落库条目（含 contiguous 真值）。
    fn quick_entries(dev: &Arc<dyn BlockDevice>, fs: FsKind) -> Vec<ScanEntry> {
        let m = mgr();
        let s = m.start(dev.clone()).unwrap();
        assert_eq!(s.fs, fs);
        crate::testutil::wait_for_state(
            &m,
            s.task_id,
            ScanState::Completed,
            Duration::from_secs(20),
        );
        let (total, page) = m.results(s.task_id, 0, 100, false).unwrap();
        assert_eq!(total as usize, page.len());
        page
    }

    #[test]
    fn reads_live_and_deleted_via_engine() {
        // 快扫落库（exfat 夹具：LIVE_A.TXT 4B / DEL_ME.JPG 9000B 已删除）→ 契约条目经
        // 反构造回引擎读：切片 == 已知字节；contiguous 落库真值一并钉住。
        let (_f, dev) = crate::testutil::exfat_fixture();
        let page = quick_entries(&dev, FsKind::Exfat);
        let a = page.iter().find(|e| e.name == "LIVE_A.TXT").unwrap();
        assert_eq!(a.contiguous, Some(true), "exfat live 夹具为 NoFatChain");
        let (all, eof) = read_entry_range(&*dev, FsKind::Exfat, a, 0, 4096).unwrap();
        assert_eq!(all, b"aaaa", "小文件整读");
        assert!(eof, "整读至条目尾 → eof");
        let (mid, eof) = read_entry_range(&*dev, FsKind::Exfat, a, 1, 2).unwrap();
        assert_eq!(mid, b"aa", "中段切片");
        assert!(!eof, "中段不到尾 → 非 eof");
        let (beyond, eof) = read_entry_range(&*dev, FsKind::Exfat, a, 4, 16).unwrap();
        assert!(beyond.is_empty() && eof, "offset 越尾 → 空 + eof");

        let d = page.iter().find(|e| e.deleted).unwrap();
        assert_eq!(d.size_bytes, 9000);
        let (head, eof) = read_entry_range(&*dev, FsKind::Exfat, d, 0, 4096).unwrap();
        assert_eq!(head, [7u8; 4096], "删除件（连续）前缀");
        assert!(!eof);
        let (mid, eof) = read_entry_range(&*dev, FsKind::Exfat, d, 4000, 500).unwrap();
        assert_eq!(mid, [7u8; 500], "删除件中段切片");
        assert!(!eof);
        let (tail, eof) = read_entry_range(&*dev, FsKind::Exfat, d, 8999, 1).unwrap();
        assert_eq!(tail, [7u8; 1], "恰到尾字节");
        assert!(eof);
    }

    #[test]
    fn reads_live_and_deleted_via_engine_fat() {
        // fat 分支同形（反构造 `to_fat_entry` 的唯一出口测试）：live 按链、删除按连续回退。
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "FAT_A.TXT", b"hello, fat!")
            .add_file("/", "GONE.BIN", &[9u8; 3000])
            .delete("/", "GONE.BIN")
            .build();
        let (_f, dev) = crate::testutil::dev_from_bytes(&image);
        let page = quick_entries(&dev, FsKind::Fat);
        let a = page.iter().find(|e| e.name == "FAT_A.TXT").unwrap();
        assert_eq!(a.contiguous, None, "fat 无拓扑概念（恒 None）");
        let (all, eof) = read_entry_range(&*dev, FsKind::Fat, a, 0, 4096).unwrap();
        assert_eq!(all, b"hello, fat!");
        assert!(eof);
        let (mid, eof) = read_entry_range(&*dev, FsKind::Fat, a, 7, 5).unwrap();
        assert_eq!(mid, b"fat!", "中段切片（越条目尾即 clamp）");
        assert!(eof, "交付短于 length → eof");
        let d = page.iter().find(|e| e.deleted).unwrap();
        let (head, _) = read_entry_range(&*dev, FsKind::Fat, d, 1000, 500).unwrap();
        assert_eq!(head, [9u8; 500], "删除件（连续回退）中段");
    }

    /// 深扫夹具（与 scan_task 测试同构）：exfat + 最大空闲区间埋 mini_jpeg。
    fn deep_fixture() -> (Vec<u8>, Vec<u8>, u64) {
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "LIVE_A.TXT", b"aaaa")
            .add_file("/", "DEL_ME.JPG", &[7u8; 9000])
            .delete("/", "DEL_ME.JPG")
            .build();
        let (_f0, dev0) = crate::testutil::dev_from_bytes(&image);
        let runs = xd_fs_exfat::freespace::unallocated_runs(&*dev0).unwrap();
        let run = runs
            .iter()
            .max_by_key(|r| r.end - r.start)
            .expect("夹具必有空闲区间")
            .clone();
        let j = xd_fixtures::mini_jpeg(20000);
        xd_fixtures::plant_in_run(&mut image, run.start, &j);
        (image, j, run.start)
    }

    #[test]
    fn reads_carved_entry_back() {
        // 深扫 → carved 条目 → 回读（run 重定位 + 结构重走）全量/切片 == 原 mini_jpeg 字节。
        let (image, j, off) = deep_fixture();
        let (_f, dev) = crate::testutil::dev_from_bytes(&image);
        let m = mgr();
        let s = m.start_deep(dev.clone()).unwrap();
        crate::testutil::wait_for_state(
            &m,
            s.task_id,
            ScanState::Completed,
            Duration::from_secs(30),
        );
        let (_t, page) = m.results(s.task_id, 0, 10, false).unwrap();
        let e = &page[0];
        assert_eq!(e.quality, "carved");
        assert_eq!(e.byte_offset, Some(off));
        assert_eq!(e.contiguous, None, "雕刻件无拓扑");
        let (all, eof) = read_entry_range(&*dev, FsKind::Exfat, e, 0, 1 << 20).unwrap();
        assert_eq!(all, j, "全量回读 == 原字节");
        assert!(eof);
        let (mid, eof) = read_entry_range(&*dev, FsKind::Exfat, e, 100, 1000).unwrap();
        assert_eq!(mid, &j[100..1100], "切片 == 原字节切片（偏移相对雕刻起点）");
        assert!(!eof);
        let (beyond, eof) =
            read_entry_range(&*dev, FsKind::Exfat, e, e.size_bytes + 5, 16).unwrap();
        assert!(beyond.is_empty() && eof, "越雕刻件尾 → 空 + eof");
    }

    #[test]
    fn eof_false_mid_file_true_at_end() {
        // eof 两侧分支逐一钉死（LIVE_B.PNG = 100 字节）：只有「短交付或到尾」才是 eof。
        let (_f, dev) = crate::testutil::exfat_fixture();
        let page = quick_entries(&dev, FsKind::Exfat);
        let b = page.iter().find(|e| e.name == "LIVE_B.PNG").unwrap();
        assert_eq!(b.size_bytes, 100);
        for (off, len, want_eof) in [
            (0u64, 50u64, false), // 中段，未到尾
            (0, 100, true),       // 恰满 length 且恰到尾
            (50, 50, true),       // 恰到尾
            (50, 100, true),      // 越尾 clamp → 短交付
            (99, 1, true),        // 尾字节
            (100, 1, true),       // 空尾
        ] {
            let (bytes, eof) = read_entry_range(&*dev, FsKind::Exfat, b, off, len).unwrap();
            let want_len = len.min(b.size_bytes.saturating_sub(off)) as usize;
            assert_eq!(bytes.len(), want_len, "off={off} len={len} 交付长");
            assert_eq!(eof, want_eof, "off={off} len={len} eof");
        }
    }

    #[test]
    fn unknown_contiguous_follows_chain_not_guess() {
        // 迁移前旧行（contiguous = NULL，v4→v5 后仍为空）反构造必须按 false（只信链）：
        // 碎片链 [7,6,8] 若被猜成连续（`unwrap_or(true)`）会交付簇 7,8,9 → 字节错位。
        // 此测是「宁可诚实短交付也不猜连续」的牙齿（对称突变必红）。
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "F.BIN", &data, &[7, 6, 8], false)
            .build();
        let (_f, dev) = crate::testutil::dev_from_bytes(&image);
        let page = quick_entries(&dev, FsKind::Exfat);
        let mut e = page.iter().find(|e| e.name == "F.BIN").unwrap().clone();
        assert_eq!(e.contiguous, Some(false), "扫描落库为链式拓扑");
        e.contiguous = None; // 模拟迁移前旧行
        let (all, eof) = read_entry_range(&*dev, FsKind::Exfat, &e, 0, 1 << 20).unwrap();
        assert_eq!(all, data, "None ⇒ 只信链（猜连续会交付错位字节）");
        assert!(eof);
        let (mid, _) = read_entry_range(&*dev, FsKind::Exfat, &e, 4100, 100).unwrap();
        assert_eq!(mid, &data[4100..4200], "跨簇切片亦按链序");
    }

    #[test]
    fn unknown_ext_carved_errors() {
        // 反向构造：真雕刻条目改 ext → from_ext 无解 → Internal（不是空交付：条目本身是坏的）
        let (image, _j, _off) = deep_fixture();
        let (_f, dev) = crate::testutil::dev_from_bytes(&image);
        let m = mgr();
        let s = m.start_deep(dev.clone()).unwrap();
        crate::testutil::wait_for_state(
            &m,
            s.task_id,
            ScanState::Completed,
            Duration::from_secs(30),
        );
        let (_t, page) = m.results(s.task_id, 0, 10, false).unwrap();
        let mut e = page[0].clone();
        e.ext = "xyz".into();
        assert!(matches!(
            read_entry_range(&*dev, FsKind::Exfat, &e, 0, 64),
            Err(ReadError::Internal(_))
        ));
    }

    #[test]
    fn to_base64_matches_contract_example() {
        // 契约示例（proto/v1/examples/fs_read.response.json）：`hello, xiaodun!` → 16 字节 base64
        assert_eq!(to_base64(b"hello, xiaodun!"), "aGVsbG8sIHhpYW9kdW4h");
    }
}
