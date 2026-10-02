// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 快扫：根（恒 FAT 链）→ 递归存活子目录；分级以**位图**为分配权威。
//! 对等继承 M1a 语义：根不可读 → Err（区别于空盘）；is_dir 忠实；保守降级；MAX 兜底。

use crate::ExfatError;
use crate::bitmap::Bitmap;
use crate::boot::{self, ExfatBoot};
use crate::dirent::{self, ParsedEntry};
use crate::fattab::Fat32;
use crate::read::{Resolved, load_bitmap_from_specials, resolve_clusters};
use xd_device::BlockDevice;

/// `read_file` 原样再导出：保 T7 路径 `xd_fs_exfat::scan::read_file` 不变（实现已迁 `read.rs`）。
pub use crate::read::read_file;

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

pub fn scan(dev: &dyn BlockDevice) -> Result<Vec<ExfatEntry>, ExfatError> {
    scan_with_observer(dev, &mut |_| {})
}

/// 扫描并逐条回调 `observer`。**后序语义**：目录条目在其子项枚举完毕后回调，`quality` 为终值
/// （子目录不可枚举的降级已写回）——流式落盘与整表结果分级逐字一致。观察者只读、不得中断
/// （中断由上层取消机制处理，见 xd-core scan_task）。
pub fn scan_with_observer(
    dev: &dyn BlockDevice,
    observer: &mut dyn FnMut(&ExfatEntry),
) -> Result<Vec<ExfatEntry>, ExfatError> {
    let boot = boot::parse(dev)?;
    let fat = Fat32::new(dev, &boot);
    let mut out = Vec::new();
    let root_data = read_root_dir(dev, &boot, &fat)?;
    let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
    let bitmap = load_bitmap_from_specials(dev, &boot, &fat, &root.specials); // 复用根快照（qual-t5 M1）
    scan_parsed(
        dev,
        &boot,
        &fat,
        bitmap.as_ref(),
        &root.entries,
        "/",
        0,
        &mut out,
        observer,
    )?;
    Ok(out)
}

/// 读根目录全部字节（根恒 FAT 链）。链失败或首读 0 字节 → Err（与空盘 Ok 区分，M1a I2 对等）。
/// `pub(crate)`：`read.rs::load_bitmap` 复用（read_file 无现成 specials）。
pub(crate) fn read_root_dir(
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
    observer: &mut dyn FnMut(&ExfatEntry),
) -> Result<(), ExfatError> {
    if depth > MAX_DEPTH || out.len() > MAX_ENTRIES {
        return Ok(()); // out.len() > 臂不可达（循环内 `>=` 已封顶；M1a 注 7 同款）
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
        // live 目录不看 checksum_ok：子项枚举成功已独立验证流扩展；时间戳/名字损坏
        // 不影响可恢复性（qual-t5 M3 裁定）
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
                        observer,
                    )?;
                }
                Err(_) => {
                    out[pushed].quality = RecoverQuality::MaybeDamaged; // 项本身可读、内容不可枚举
                }
            }
        }
        // 后序单回调：子目录降级已写回 out[pushed]，此处 quality 即终值
        observer(&out[pushed]);
    }
    Ok(())
}

/// 子目录字节：`resolve_clusters` 定位（contiguous 规范保证 / 否则先链、不足退连续）；
/// 读不满即降级 Err。物化有界（need ≤ MAX_DIR_BYTES/512），与 `read_file` 的流式路径不同。
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
    let clusters: Vec<u32> = match resolve_clusters(boot, fat, e.first_cluster, need, e.contiguous)
    {
        Some(Resolved::Chain(chain)) => chain, // 已切到 need 前缀（qual-t6 Minor 4）
        Some(Resolved::Contiguous { first, n }) => {
            (0..n).map(|i| (first as u64 + i) as u32).collect()
        }
        None => return Err(ExfatError::InvalidBoot("目录簇越界".into())),
    };
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

/// 删除项分级（(a) 裁定版）：**非连续 → 只信 stale 链**——链覆盖不了 need（含链断裂/被清）
/// 即证据不足（MaybeDamaged），绝不按连续假设评级；连续（NoFatChain 规范保证）→ 起点/可达
/// 界卫 + 逐簇位图全空才算 Complete。位图是分配权威（FAT 对删除项已 stale）。
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
    let need = e.data_length.div_ceil(boot.cluster_bytes());
    let all_free = |c: u32| matches!(bitmap.is_free(c), Ok(true));
    if !e.contiguous {
        let max_cluster = boot.cluster_count as u64 + 1;
        let fc = e.first_cluster as u64;
        if !(2..=max_cluster).contains(&fc) || need > max_cluster - fc + 1 {
            return RecoverQuality::MaybeDamaged; // 链越过可达簇数 = 链在说谎
        }
        // (a)：链只走不猜——链不足 need（含解析失败）→ 交付必短，证据不足
        let Ok(chain) = fat.chain(e.first_cluster) else {
            return RecoverQuality::MaybeDamaged;
        };
        if (chain.len() as u64) < need {
            return RecoverQuality::MaybeDamaged;
        }
        // 链前缀不得回访簇：回访=环/回折（如 FAT 自环 → [252,252,…]），交付会重复同一簇字节（伪造序）
        let mut sorted = chain[..need as usize].to_vec();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1]) {
            return RecoverQuality::MaybeDamaged; // 前缀回访簇 = 链在说谎
        }
        return if chain[..need as usize].iter().all(|c| all_free(*c)) {
            RecoverQuality::Complete
        } else {
            RecoverQuality::MaybeDamaged
        };
    }
    // 连续（NoFatChain 规范保证）：起点/可达界卫 + 逐簇空闲
    let max_cluster = boot.cluster_count as u64 + 1;
    if !(2..=max_cluster).contains(&(e.first_cluster as u64)) {
        return RecoverQuality::MaybeDamaged;
    }
    if need > max_cluster - e.first_cluster as u64 + 1 {
        return RecoverQuality::MaybeDamaged;
    }
    for i in 0..need {
        if !all_free((e.first_cluster as u64 + i) as u32) {
            return RecoverQuality::MaybeDamaged;
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

    fn dummy_parsed() -> ParsedEntry {
        ParsedEntry {
            name: "X.TXT".into(),
            attr_dir: false,
            first_cluster: 0,
            valid_data_length: 0,
            data_length: 0,
            contiguous: false,
            deleted: false,
            checksum_ok: true,
            name_verified: true,
        }
    }

    fn dummy_entry() -> ExfatEntry {
        ExfatEntry {
            name: String::new(),
            path: String::new(),
            size_bytes: 0,
            data_length: 0,
            first_cluster: 0,
            deleted: false,
            is_dir: false,
            contiguous: false,
            quality: RecoverQuality::Complete,
            ext: String::new(),
        }
    }

    /// 独立重算 SetChecksum（16 位循环右移累加、跳下标 2/3）：
    /// 构造"污染 DataLength 但还原校验仍自洽"的删除项（否则 T4 会直接丢弃该项）。
    fn set_checksum(set: &[u8]) -> u16 {
        let mut sum = 0u16;
        for (i, b) in set.iter().enumerate() {
            if i == 2 || i == 3 {
                continue;
            }
            sum = (if sum & 1 != 0 { 0x8000u16 } else { 0u16 })
                .wrapping_add(sum >> 1)
                .wrapping_add(*b as u16);
        }
        sum
    }

    /// 就地重算删除项集 SetChecksum：先按删除还原语义对每槽类型字节 `|=0x80` 再折叠
    /// （删除只清 bit7、不重算——这正是 T4 删除门槛的语义）。
    fn refix_deleted_checksum(img: &mut [u8], set_off: usize, slots: usize) {
        let mut restored = img[set_off..set_off + slots * 32].to_vec();
        for k in 0..slots {
            restored[k * 32] |= 0x80;
        }
        let cs = set_checksum(&restored);
        img[set_off + 2..set_off + 4].copy_from_slice(&cs.to_le_bytes());
    }

    /// 记账设备：记录每次 `read_at` 的起点偏移（qual-t5 M1 根单读证明）。
    /// `Mutex`（非 `RefCell`）——`BlockDevice: Send + Sync` 要求。
    struct CountingDev<'a> {
        inner: &'a dyn BlockDevice,
        reads: std::sync::Mutex<Vec<u64>>,
    }

    impl BlockDevice for CountingDev<'_> {
        fn info(&self) -> &xd_device::DeviceInfo {
            self.inner.info()
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, xd_device::DeviceError> {
            self.reads.lock().unwrap().push(offset);
            self.inner.read_at(offset, buf)
        }
    }

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

    #[test]
    fn live_dir_with_broken_checksum_stays_complete() {
        // qual-t5 M3 裁定：live 目录不看 checksum_ok——结构完好 + 子项枚举成功 → Complete
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_subdir("/", "DCIM")
            .add_file("/DCIM", "IMG.JPG", &[3u8; 100])
            .build();
        let mut patched = image.clone();
        patched[SET + 8] ^= 0xFF; // DCIM 主项时间戳字节：结构完好
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let d = entries.iter().find(|e| e.name == "DCIM").unwrap();
        assert_eq!(d.quality, RecoverQuality::Complete);
        assert!(entries.iter().any(|e| e.name == "IMG.JPG"), "子项照常枚举");
    }

    #[test]
    fn root_clusters_read_once() {
        // qual-t5 M1：根只读一遍。45 文件撑到 2 簇根，两簇各须恰好被读 1 次
        // （修复前 load_bitmap 会自读根一遍 → 每簇 2 次）。
        let mut b = xd_fixtures::ExfatImageBuilder::new();
        for i in 0..45u32 {
            b.add_file("/", &format!("F{i:04}.TXT"), b"x");
        }
        let image = b.build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let chain = fat.chain(boot.root_cluster).unwrap();
        assert!(chain.len() >= 2, "夹具须多簇根（45 文件）");
        let cd = CountingDev {
            inner: &dev,
            reads: std::sync::Mutex::new(Vec::new()),
        };
        let entries = scan(&cd).unwrap();
        assert_eq!(entries.len(), 45);
        let reads = cd.reads.lock().unwrap();
        for c in &chain {
            let start = boot.cluster_to_byte(*c);
            let n = reads.iter().filter(|o| **o == start).count();
            assert_eq!(n, 1, "根簇 {c} 应恰好读 1 次，实际 {n}");
        }
    }

    #[test]
    fn max_depth_semantics_direct() {
        // MAX_DEPTH 边界：depth == MAX_DEPTH 处理、depth == MAX_DEPTH + 1 截断
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let e = dummy_parsed();
        let mut at_limit = Vec::new();
        scan_parsed(
            &dev,
            &boot,
            &fat,
            None,
            std::slice::from_ref(&e),
            "/",
            MAX_DEPTH,
            &mut at_limit,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(at_limit.len(), 1, "depth == MAX_DEPTH 须处理");
        let mut over = Vec::new();
        scan_parsed(
            &dev,
            &boot,
            &fat,
            None,
            std::slice::from_ref(&e),
            "/",
            MAX_DEPTH + 1,
            &mut over,
            &mut |_| {},
        )
        .unwrap();
        assert!(over.is_empty(), "depth > MAX_DEPTH 须截断");
    }

    #[test]
    fn max_entries_semantics_direct() {
        // 满额语义：out 已达 MAX_ENTRIES → 立即 Ok（非 Err）且不增
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let e = dummy_parsed();
        let mut out: Vec<ExfatEntry> = (0..MAX_ENTRIES).map(|_| dummy_entry()).collect();
        scan_parsed(
            &dev,
            &boot,
            &fat,
            None,
            std::slice::from_ref(&e),
            "/",
            0,
            &mut out,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(out.len(), MAX_ENTRIES, "满额即 Ok 且不增");
    }

    #[test]
    fn deep_nesting_40_levels_e2e() {
        // 40 层子目录链：列出 depth 0..=32 的 33 个目录；D33 与 D32 内的文件不列出；无栈溢出
        let mut b = xd_fixtures::ExfatImageBuilder::new();
        let mut path = String::new();
        let mut d32 = String::new();
        for i in 0..40u32 {
            let name = format!("D{i:02}");
            let parent = if path.is_empty() {
                "/".to_string()
            } else {
                path.clone()
            };
            b.add_subdir(&parent, &name);
            path = format!("{path}/{name}");
            if i == 32 {
                d32 = path.clone();
            }
        }
        b.add_file(&d32, "DEEP.TXT", b"x");
        let image = b.build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let dirs: Vec<&str> = entries
            .iter()
            .filter(|e| e.is_dir)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(dirs.len(), 33, "depth 0..=32 共 33 个目录：{dirs:?}");
        assert!(dirs.contains(&"D00") && dirs.contains(&"D32"));
        assert!(!dirs.contains(&"D33"), "depth 33 起须截断");
        assert!(
            !entries.iter().any(|e| e.name == "DEEP.TXT"),
            "D32 内文件属 depth 33 枚举，不得列出"
        );
    }

    #[test]
    fn bitmap_datalength_bounds_degrade() {
        // 0x81 位图 DataLength 越界（上界 64MiB+1 / 下界 31 < ceil(252/8)）→ 位图不可读
        // → 删除项 MaybeDamaged，scan 仍 Ok
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        for dl in [MAX_DIR_BYTES + 1, 31u64] {
            let mut patched = image.clone();
            patched[ROOT_B + 32 + 24..ROOT_B + 32 + 32].copy_from_slice(&dl.to_le_bytes());
            let (_f, dev) = dev_for(&patched);
            let entries = scan(&dev).unwrap();
            let e = entries.iter().find(|e| e.deleted).unwrap();
            assert_eq!(e.quality, RecoverQuality::MaybeDamaged, "bitmap dl={dl}");
        }
    }

    #[test]
    fn read_subdir_bytes_gates_direct() {
        // dl==0 与 dl>64MiB 两道闸：不得进入任何读路径
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let mk = |dl: u64| ParsedEntry {
            attr_dir: true,
            first_cluster: 6,
            data_length: dl,
            contiguous: true,
            ..dummy_parsed()
        };
        assert!(matches!(
            read_subdir_bytes(&dev, &boot, &fat, &mk(0)),
            Err(ExfatError::InvalidBoot(_))
        ));
        assert!(matches!(
            read_subdir_bytes(&dev, &boot, &fat, &mk(MAX_DIR_BYTES + 1)),
            Err(ExfatError::InvalidBoot(_))
        ));
    }

    #[test]
    fn deleted_polluted_datalength_degrades() {
        // qual-t5 I1 构型：删除项 DataLength 污染成巨值（重算还原校验以过 T4 门槛）
        // → 界卫即刻降级 MaybeDamaged，且不物化簇表
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        let mut patched = image.clone();
        let stream = SET + 32;
        patched[stream + 24..stream + 32].copy_from_slice(&u64::MAX.to_le_bytes()); // DataLength
        refix_deleted_checksum(&mut patched, SET, 3);
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.name == "PHOTO.JPG").unwrap();
        assert!(e.deleted && e.size_bytes == 9000);
        assert_eq!(
            e.quality,
            RecoverQuality::MaybeDamaged,
            "need ≫ reachable：界卫即刻降级"
        );

        // 同构：起点污染（T4 不设 fc 上界）→ 起点界卫兜住，debug 档也不得下溢 panic
        let mut patched2 = image.clone();
        patched2[stream + 20..stream + 24].copy_from_slice(&9999u32.to_le_bytes()); // FirstCluster
        refix_deleted_checksum(&mut patched2, SET, 3);
        let (_f2, dev2) = dev_for(&patched2);
        let entries2 = scan(&dev2).unwrap();
        let e2 = entries2.iter().find(|e| e.name == "PHOTO.JPG").unwrap();
        assert_eq!(
            e2.quality,
            RecoverQuality::MaybeDamaged,
            "fc 越界：起点界卫兜住"
        );
    }

    #[test]
    fn deleted_chain_grades_on_need_prefix_only() {
        // I2 对齐的 T5 半壁：stale 链 [6,9,7,10]（7→10 已是 NEW.BIN 的链），dl 补成 8192（need=2）
        // → chain[..2]=[6,9] 全空闲 → Complete（链尾 7/10 被占不牵连）
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &[7u8; 9000], &[6, 9, 7], false)
            .delete("/", "OLD.BIN")
            .add_file_in_clusters("/", "NEW.BIN", &[5u8; 4600], &[7, 10], false)
            .build();
        let mut patched = image.clone();
        let stream = SET + 32;
        // VDL 与 DL 同补成 8192：vdl > dl 会被 T4 直接丢弃（夹具 vdl=9000）
        patched[stream + 8..stream + 16].copy_from_slice(&8192u64.to_le_bytes());
        patched[stream + 24..stream + 32].copy_from_slice(&8192u64.to_le_bytes());
        refix_deleted_checksum(&mut patched, SET, 3);
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.name == "OLD.BIN").unwrap();
        assert!(e.deleted);
        assert_eq!(
            e.quality,
            RecoverQuality::Complete,
            "只按 need 前缀判空闲，链尾被占不牵连"
        );
    }

    #[test]
    fn deleted_short_stale_chain_never_complete() {
        // (a)：碎片化删除项 stale 链被清（FAT[6]=0）→ 链只剩首簇 < need → MaybeDamaged；
        // 旧式会对连续区间 [6,7,8] 全空闲错误给出 Complete（探针 B 的分级半壁）。
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &[7u8; 9000], &[6, 9, 7], false)
            .delete("/", "OLD.BIN")
            .build();
        let mut patched = image.clone();
        patched[24 * 512 + 6 * 4..24 * 512 + 6 * 4 + 4].copy_from_slice(&0u32.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::MaybeDamaged,
            "链不足 need 不得按连续评级"
        );
    }

    #[test]
    fn deleted_loop_chain_beyond_reachable_degrades() {
        // qual-t4 发现 B：链在说谎（自环 + DL 污染）→ 可达界卫必须降级（旧 resolve_clusters 界卫对等保留）
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &[7u8; 4000], &[253], false)
            .delete("/", "OLD.BIN")
            .build();
        let mut patched = image.clone();
        let stream = SET + 32;
        patched[stream + 8..stream + 16].copy_from_slice(&12288u64.to_le_bytes()); // VDL
        patched[stream + 24..stream + 32].copy_from_slice(&12288u64.to_le_bytes()); // DL：need=3 > reachable=1
        patched[24 * 512 + 253 * 4..24 * 512 + 253 * 4 + 4].copy_from_slice(&253u32.to_le_bytes()); // FAT[253] 自环
        refix_deleted_checksum(&mut patched, SET, 3);
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "OLD.BIN")
            .unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::MaybeDamaged,
            "链越过可达簇数=链在说谎"
        );
    }

    #[test]
    fn deleted_loop_prefix_within_reachable_degrades() {
        // spec-t4 注记 2：fc=252、need=2=reachable、自环 FAT[252]=252 → 前缀 [252,252] 回访
        // → 交付必重复同一簇 = 伪造序 → MaybeDamaged（旧式与门槛前新式都错判 Complete）
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &[7u8; 4000], &[252], false)
            .delete("/", "OLD.BIN")
            .build();
        let mut patched = image.clone();
        let stream = SET + 32;
        patched[stream + 8..stream + 16].copy_from_slice(&8192u64.to_le_bytes()); // VDL
        patched[stream + 24..stream + 32].copy_from_slice(&8192u64.to_le_bytes()); // DL：need=2
        patched[24 * 512 + 252 * 4..24 * 512 + 252 * 4 + 4].copy_from_slice(&252u32.to_le_bytes()); // 自环
        refix_deleted_checksum(&mut patched, SET, 3);
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "OLD.BIN")
            .unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::MaybeDamaged,
            "前缀回访簇 = 链在说谎"
        );
    }

    #[test]
    fn observer_streams_post_order_with_final_quality() {
        // 健康 DCIM：IMG.JPG 先于 DCIM 回调（后序）；根文件 ROOT.TXT 在 DCIM 后
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_subdir("/", "DCIM")
            .add_file("/DCIM", "IMG.JPG", &[3u8; 100])
            .add_file("/", "ROOT.TXT", b"root")
            .build();
        let (_f, dev) = dev_for(&image);
        let mut seen: Vec<String> = Vec::new();
        let entries = scan_with_observer(&dev, &mut |e| seen.push(e.name.clone())).unwrap();
        assert_eq!(
            seen,
            vec!["IMG.JPG", "DCIM", "ROOT.TXT"],
            "后序：子项在目录前"
        );
        assert_eq!(entries.len(), 3, "返回值与整表同源");
    }

    #[test]
    fn observer_sees_downgraded_dir_quality() {
        // DCIM 不可枚举（首簇越界）→ observer 收到 DCIM 时 quality 已是终值 MaybeDamaged
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_subdir("/", "DCIM")
            .add_file("/DCIM", "IMG.JPG", &[3u8; 100])
            .build();
        let mut patched = image.clone();
        patched[SET + 32 + 20..SET + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let mut seen: Vec<(String, RecoverQuality)> = Vec::new();
        scan_with_observer(&dev, &mut |e| seen.push((e.name.clone(), e.quality))).unwrap();
        let dcim = seen.iter().find(|(n, _)| n == "DCIM").unwrap();
        assert_eq!(dcim.1, RecoverQuality::MaybeDamaged, "流式层拿到终值分级");
    }

    #[test]
    fn deleted_reachable_exact_fit_is_complete() {
        // 界卫 off-by-one：need == reachable（首簇 6 到末簇 count+1 全空闲）→ Complete，不得提前降级
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "G.BIN", &[4u8; 512])
            .delete("/", "G.BIN")
            .build();
        let (_f0, dev0) = dev_for(&image);
        let boot = boot::parse(&dev0).unwrap();
        let first = scan(&dev0)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "G.BIN")
            .unwrap()
            .first_cluster as u64;
        let reachable = boot.cluster_count as u64 + 1 - first + 1;
        let dl = reachable * boot.cluster_bytes();
        let mut patched = image.clone();
        let dl_off = SET + 32 + 24;
        patched[dl_off..dl_off + 8].copy_from_slice(&dl.to_le_bytes());
        refix_deleted_checksum(&mut patched, SET, 3);
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "G.BIN")
            .unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::Complete,
            "need == reachable 须 Complete（界卫严格 >）"
        );
    }
}
