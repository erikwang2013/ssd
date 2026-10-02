// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 快扫：根（恒 FAT 链）→ 递归存活子目录；分级以**位图**为分配权威。
//! 对等继承 M1a 语义：根不可读 → Err（区别于空盘）；is_dir 忠实；保守降级；MAX 兜底。

use crate::ExfatError;
use crate::bitmap::Bitmap;
use crate::boot::{self, ExfatBoot};
use crate::dirent::{self, ParsedEntry};
use crate::fattab::Fat32;
use xd_device::BlockDevice;

const MAX_ENTRIES: usize = 200_000;
const MAX_DEPTH: u32 = 32;
const MAX_DIR_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverQuality {
    Complete,
    MaybeDamaged,
}

#[derive(Debug, Clone)]
pub struct ExfatEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64, // = ValidDataLength（交付长度）
    pub data_length: u64,
    pub first_cluster: u32,
    pub deleted: bool,
    pub is_dir: bool,
    pub contiguous: bool,
    pub quality: RecoverQuality,
    pub ext: String,
}

/// texFAT 双位图选择：优先活动位图（bitmaps[k].0 == active），缺失则回退任一。
pub(crate) fn pick_bitmap(bitmaps: &[(bool, u32, u64)], active: u8) -> Option<(u32, u64)> {
    let want_second = active == 1;
    bitmaps
        .iter()
        .find(|(second, _, _)| *second == want_second)
        .or_else(|| bitmaps.first())
        .map(|(_, f, l)| (*f, *l))
}

pub fn scan(dev: &dyn BlockDevice) -> Result<Vec<ExfatEntry>, ExfatError> {
    let boot = boot::parse(dev)?;
    let fat = Fat32::new(dev, &boot);
    let mut out = Vec::new();
    let root_data = read_root_dir(dev, &boot, &fat)?;
    let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
    let bitmap = load_bitmap(dev, &boot, &fat);
    scan_parsed(
        dev,
        &boot,
        &fat,
        bitmap.as_ref(),
        &root.entries,
        "/",
        0,
        &mut out,
    )?;
    Ok(out)
}

/// 定位并加载分配位图（scan 与 read_file 共用）。任何失败 → None（分级层即降级，绝不回退 FAT）。
fn load_bitmap(dev: &dyn BlockDevice, boot: &ExfatBoot, fat: &Fat32) -> Option<Bitmap> {
    let root_data = read_root_dir(dev, boot, fat).ok()?;
    let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
    let (first, len) = pick_bitmap(&root.specials.bitmaps, boot.active_fat_index())?;
    Bitmap::load(dev, boot, fat, first, len).ok()
}

/// 读根目录全部字节（根恒 FAT 链）。链失败或首读 0 字节 → Err（与空盘 Ok 区分，M1a I2 对等）。
fn read_root_dir(
    dev: &dyn BlockDevice,
    boot: &ExfatBoot,
    fat: &Fat32,
) -> Result<Vec<u8>, ExfatError> {
    let chain = fat
        .chain(boot.root_cluster)
        .map_err(|_| ExfatError::InvalidBoot("根目录不可读".into()))?;
    let cb = boot.cluster_bytes();
    let mut data = Vec::new();
    let mut buf = vec![0u8; cb as usize];
    let mut got_any = false;
    for c in chain {
        if data.len() as u64 > MAX_DIR_BYTES {
            break;
        }
        let n = match dev.read_at(boot.cluster_to_byte(c), &mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        got_any = true;
        data.extend_from_slice(&buf[..n]);
        if n < buf.len() {
            break;
        }
    }
    if !got_any {
        return Err(ExfatError::InvalidBoot("根目录不可读".into()));
    }
    Ok(data)
}

#[allow(clippy::too_many_arguments)]
fn scan_parsed(
    dev: &dyn BlockDevice,
    boot: &ExfatBoot,
    fat: &Fat32,
    bitmap: Option<&Bitmap>,
    entries: &[ParsedEntry],
    path: &str,
    depth: u32,
    out: &mut Vec<ExfatEntry>,
) -> Result<(), ExfatError> {
    if depth > MAX_DEPTH || out.len() > MAX_ENTRIES {
        return Ok(());
    }
    for e in entries {
        if out.len() >= MAX_ENTRIES {
            return Ok(());
        }
        let ext = e
            .name
            .rsplit_once('.')
            .map(|(_, x)| x.to_ascii_lowercase())
            .unwrap_or_default();
        let quality = if e.attr_dir {
            RecoverQuality::Complete
        } else if e.deleted {
            grade_deleted(boot, fat, bitmap, e)
        } else if e.checksum_ok {
            RecoverQuality::Complete
        } else {
            // live 但项集校验和不符：保守降级封顶（qual-t4 M3 裁定 a；值域已由下游界兜住）
            RecoverQuality::MaybeDamaged
        };
        out.push(ExfatEntry {
            name: e.name.clone(),
            path: path.to_string(),
            size_bytes: e.valid_data_length,
            data_length: e.data_length,
            first_cluster: e.first_cluster,
            deleted: e.deleted,
            is_dir: e.attr_dir, // ParsedEntry 字段名是 attr_dir（T4）；映射在本层，勿反向改 T4
            contiguous: e.contiguous,
            quality,
            ext,
        });
        let pushed = out.len() - 1;
        if e.attr_dir && !e.deleted && e.first_cluster >= 2 {
            let child_path = if path == "/" {
                format!("/{}", e.name)
            } else {
                format!("{path}/{}", e.name)
            };
            let child = read_subdir_bytes(dev, boot, fat, e);
            match child {
                Ok(data) => {
                    let d = dirent::parse_directory_bytes(&data, boot.cluster_bytes() as usize);
                    scan_parsed(
                        dev,
                        boot,
                        fat,
                        bitmap,
                        &d.entries,
                        &child_path,
                        depth + 1,
                        out,
                    )?;
                }
                Err(_) => {
                    out[pushed].quality = RecoverQuality::MaybeDamaged; // 项本身可读、内容不可枚举
                }
            }
        }
    }
    Ok(())
}

/// 子目录字节：contiguous → 连续读（规范保证）；否则先链、不足退连续；读不满即降级 Err。
fn read_subdir_bytes(
    dev: &dyn BlockDevice,
    boot: &ExfatBoot,
    fat: &Fat32,
    e: &ParsedEntry,
) -> Result<Vec<u8>, ExfatError> {
    if e.data_length == 0 || e.data_length > MAX_DIR_BYTES {
        return Err(ExfatError::InvalidBoot("目录 DataLength 非法".into()));
    }
    let cb = boot.cluster_bytes();
    let need = e.data_length.div_ceil(cb);
    let mut clusters: Vec<u32> = Vec::new();
    if !e.contiguous
        && let Ok(chain) = fat.chain(e.first_cluster)
        && chain.len() as u64 >= need
    {
        clusters = chain[..need as usize].to_vec();
    }
    if clusters.is_empty() {
        let max_cluster = boot.cluster_count as u64 + 1;
        let mut c = e.first_cluster as u64;
        while (clusters.len() as u64) < need && c <= max_cluster {
            clusters.push(c as u32);
            c += 1;
        }
        if (clusters.len() as u64) < need {
            return Err(ExfatError::InvalidBoot("目录簇越界".into()));
        }
    }
    let mut data = Vec::with_capacity(e.data_length as usize);
    let mut buf = vec![0u8; cb as usize];
    for c in clusters {
        let n = match dev.read_at(boot.cluster_to_byte(c), &mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if n < buf.len() {
            break;
        }
    }
    if (data.len() as u64) < e.data_length {
        return Err(ExfatError::InvalidBoot("目录读不满".into()));
    }
    data.truncate(e.data_length as usize);
    Ok(data)
}

/// 删除项分级：簇定位（contiguous 规范保证 / 链优先）→ 界内 → 位图全空 → Complete。
fn grade_deleted(
    boot: &ExfatBoot,
    fat: &Fat32,
    bitmap: Option<&Bitmap>,
    e: &ParsedEntry,
) -> RecoverQuality {
    if boot.backup_used {
        return RecoverQuality::MaybeDamaged; // 卷级几何未验证 → 封顶
    }
    let Some(bitmap) = bitmap else {
        return RecoverQuality::MaybeDamaged; // 位图不可读 → 永不给 Complete
    };
    if e.data_length == 0 {
        return RecoverQuality::Complete;
    }
    if e.first_cluster < 2 {
        return RecoverQuality::MaybeDamaged;
    }
    let cb = boot.cluster_bytes();
    let need = e.data_length.div_ceil(cb);
    let max_cluster = boot.cluster_count as u64 + 1;
    let mut clusters: Vec<u32> = Vec::new();
    if !e.contiguous
        && let Ok(chain) = fat.chain(e.first_cluster)
        && chain.len() as u64 >= need
    {
        clusters = chain[..need as usize].to_vec();
    }
    if clusters.is_empty() {
        let mut c = e.first_cluster as u64;
        while (clusters.len() as u64) < need && c <= max_cluster {
            clusters.push(c as u32);
            c += 1;
        }
    }
    if (clusters.len() as u64) < need {
        return RecoverQuality::MaybeDamaged; // 越界截断：表项不可信
    }
    for c in clusters {
        match bitmap.is_free(c) {
            Ok(true) => {}
            _ => return RecoverQuality::MaybeDamaged, // 已占用/不可判：宁可漏报
        }
    }
    RecoverQuality::Complete
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot::testutil::{dev_for, patch_boot_both};

    const ROOT_B: usize = 32 * 512 + 3 * 4096;
    const SET: usize = ROOT_B + 96;

    #[test]
    fn scans_live_and_deleted_with_full_names() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"hello")
            .add_file_chained("/", "G.BIN", &[9u8; 9000])
            .delete("/", "G.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let a = entries.iter().find(|e| e.name == "A.TXT").unwrap();
        assert!(!a.deleted && a.quality == RecoverQuality::Complete);
        let g = entries.iter().find(|e| e.name == "G.BIN").unwrap();
        assert!(g.deleted && g.size_bytes == 9000, "exFAT 删除名一字不差");
    }

    #[test]
    fn deleted_contiguous_is_complete() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::Complete,
            "NoFatChain 删除后仍连续（规范保证）+ 位图全空"
        );
        assert!(e.contiguous);
    }

    #[test]
    fn deleted_chained_stale_fat_bitmap_clear_is_complete() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "G.BIN", &[9u8; 9000])
            .delete("/", "G.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::Complete,
            "位图全空（权威判据），stale FAT 不影响"
        );
    }

    #[test]
    fn deleted_with_reused_cluster_is_maybe_damaged() {
        // 删除后被新文件复用簇（位图置位）→ 只能 MaybeDamaged
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "OLD.BIN", &[7u8; 9000])
            .delete("/", "OLD.BIN")
            .add_file_in_clusters("/", "NEW.BIN", &[5u8; 4500], &[6, 7], false)
            .build();
        let (_f, dev) = dev_for(&image);
        let old = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "OLD.BIN")
            .unwrap();
        assert_eq!(old.quality, RecoverQuality::MaybeDamaged);
    }

    #[test]
    fn unreadable_bitmap_never_complete() {
        // 把 0x81 的 FirstCluster 改成越界 → 位图不可读 → 删除项封顶 MaybeDamaged，scan 仍 Ok
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        let mut patched = image.clone();
        patched[ROOT_B + 32 + 20..ROOT_B + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::MaybeDamaged);
    }

    #[test]
    fn pick_bitmap_prefers_active_and_falls_back() {
        // 纯函数级：texFAT 双位图选择
        let both = vec![(false, 2, 32u64), (true, 40, 32u64)];
        assert_eq!(pick_bitmap(&both, 1), Some((40, 32)));
        assert_eq!(pick_bitmap(&both, 0), Some((2, 32)));
        let only_first = vec![(false, 2, 32u64)];
        assert_eq!(
            pick_bitmap(&only_first, 1),
            Some((2, 32)),
            "缺失活动位图时回退任一可用"
        );
        assert_eq!(pick_bitmap(&[], 0), None);
    }

    #[test]
    fn texfat_fallback_still_scans() {
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        patch_boot_both(
            &mut image,
            &[(110, &2u8.to_le_bytes()), (106, &1u16.to_le_bytes())],
        );
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::Complete,
            "仅 id-0 位图存在 → 回退仍可用"
        );
    }

    #[test]
    fn backup_geometry_caps_deleted_quality() {
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        image[200] ^= 0xFF; // 主区校验失败（备区仍有效）
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::MaybeDamaged,
            "卷级降级：几何未验证"
        );
    }

    #[test]
    fn root_unreadable_is_err() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"x")
            .build();
        let truncated = image[..20_000].to_vec(); // 根区（28672 起）在设备外
        let (_f, dev) = dev_for(&truncated);
        assert!(matches!(scan(&dev), Err(ExfatError::InvalidBoot(m)) if m.contains("根目录")));
    }

    #[test]
    fn unreadable_subdir_marks_entry_damaged_and_continues() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_subdir("/", "DCIM")
            .add_file("/DCIM", "IMG.JPG", &[3u8; 100])
            .add_file("/", "ROOT.TXT", b"root")
            .build();
        let mut patched = image.clone();
        // DCIM 项集首簇（根槽 3 的 0xC0 偏移 20）改越界
        patched[SET + 32 + 20..SET + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let dir = entries.iter().find(|e| e.name == "DCIM").unwrap();
        assert_eq!(dir.quality, RecoverQuality::MaybeDamaged);
        assert!(entries.iter().any(|e| e.name == "ROOT.TXT"), "不得中止全盘");
        assert!(!entries.iter().any(|e| e.name == "IMG.JPG"));
    }

    #[test]
    fn deleted_dir_listed_not_recursed() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_subdir("/", "DCIM")
            .add_file("/DCIM", "IMG.JPG", &[3u8; 100])
            .build();
        let mut patched = image.clone();
        patched[SET] &= 0x7F;
        patched[SET + 32] &= 0x7F;
        patched[SET + 64] &= 0x7F;
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let dcim = entries.iter().find(|e| e.name == "DCIM").unwrap();
        assert!(dcim.deleted && dcim.is_dir, "is_dir 忠实属性");
        assert!(
            !entries.iter().any(|e| e.name == "IMG.JPG"),
            "不递归已删目录"
        );
    }

    #[test]
    fn live_broken_checksum_caps_quality() {
        // qual-t4 M3 裁定 a：live 项集校验和不符（结构完好）→ 保守降级 MaybeDamaged
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"x")
            .build();
        let mut patched = image.clone();
        patched[SET + 8] ^= 0xFF; // A.TXT 主项时间戳字节：结构完好、校验和坏
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "A.TXT")
            .unwrap();
        assert_eq!(e.quality, RecoverQuality::MaybeDamaged);
    }

    #[test]
    fn scans_multi_cluster_root_completely() {
        // 45 文件把根撑到 2 簇（含 0x01 补齐）：扫描必须读满整链、45 条全出——
        // 防"补齐回归成 0x00 导致解析器半途终止、静默丢后半根目录"（qual-t1 I3）
        let mut b = xd_fixtures::ExfatImageBuilder::new();
        for i in 0..45u32 {
            b.add_file("/", &format!("F{i:04}.TXT"), b"x");
        }
        let image = b.build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let files: Vec<&str> = entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(files.len(), 45, "多簇根不得丢条目：{files:?}");
        assert!(files.contains(&"F0041.TXT"), "第二根簇的条目必须在列");
        assert!(files.contains(&"F0044.TXT"));
    }
}
