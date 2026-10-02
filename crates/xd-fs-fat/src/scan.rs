// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 快速扫描：目录遍历（根 + 子目录）+ 删除文件找回 + 质量分级。
//! 原则：保守降级——单个坏目录链/坏读不中止全盘（根目录无法开始枚举除外：返回 Err，
//! 与"空盘 Ok([])"区分）；只有确证全空闲才评 Complete。

use crate::FatError;
use crate::bpb::{self, Bpb, FatType};
use crate::dirent::{self, ParsedEntry};
use crate::fat::Fat;
use xd_device::BlockDevice;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverQuality {
    /// 数据簇全部空闲，且长度按连续假设可得
    Complete,
    /// 有簇已被重新分配（覆盖风险），或簇信息缺失
    MaybeDamaged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FatEntry {
    pub name: String,
    pub path: String, // "/" 或 "/DIR"
    pub size_bytes: u64,
    pub first_cluster: u32,
    pub deleted: bool,
    pub is_dir: bool,
    pub quality: RecoverQuality,
    pub ext: String, // 小写，无扩展名 = ""
}

const MAX_ENTRIES: usize = 200_000;
const MAX_DEPTH: u32 = 32;

/// 扫描整卷：根目录 + 递归子目录，返回全部条目（含删除文件）。
/// 根目录无法开始枚举（根链解析失败或根区读不到内容）→ Err，与空盘 `Ok([])` 区分；
/// 其余局部损坏按保守降级处理（跳过或标记 MaybeDamaged），不中止全盘。
pub fn scan(dev: &dyn BlockDevice) -> Result<Vec<FatEntry>, FatError> {
    scan_with_observer(dev, &mut |_| {})
}

/// 扫描并逐条回调 `observer`。**后序语义**：目录条目在其子项枚举完毕后回调，`quality` 为终值
/// （子目录不可枚举的降级已写回）——流式落盘与整表结果分级逐字一致。观察者只读、不得中断
/// （中断由上层取消机制处理，见 xd-core scan_task）。
pub fn scan_with_observer(
    dev: &dyn BlockDevice,
    observer: &mut dyn FnMut(&FatEntry),
) -> Result<Vec<FatEntry>, FatError> {
    let bpb = bpb::parse(dev)?;
    let fat = Fat::new(dev, &bpb);
    let mut out = Vec::new();
    let readable = if bpb.fat_type == FatType::Fat32 {
        scan_cluster_dir(
            dev,
            &bpb,
            &fat,
            bpb.root_cluster,
            "/",
            0,
            &mut out,
            observer,
        )?
    } else {
        scan_fixed_root(dev, &bpb, &fat, &mut out, observer)?
    };
    if !readable {
        return Err(FatError::InvalidBpb("根目录不可读".into()));
    }
    Ok(out)
}

/// FAT12/16 固定根目录。返回值：根区是否可枚举。
fn scan_fixed_root(
    dev: &dyn BlockDevice,
    bpb: &Bpb,
    fat: &Fat,
    out: &mut Vec<FatEntry>,
    observer: &mut dyn FnMut(&FatEntry),
) -> Result<bool, FatError> {
    let root_bytes = ((bpb.root_entry_count as u32) * 32) as usize;
    let mut buf = vec![0u8; root_bytes];
    let start = bpb.root_start_sector as u64 * bpb.bytes_per_sector as u64;
    let n = dev.read_at(start, &mut buf)?;
    if n == 0 {
        return Ok(false); // 根区在设备外：无法开始枚举
    }
    let parsed = dirent::parse_directory_bytes(&buf[..n]); // 短读 → 只解析已读部分（不得零填充当 End）
    append_parsed(dev, bpb, fat, parsed, "/", 0, out, observer)?;
    Ok(true)
}

/// 簇链目录（FAT32 根与所有子目录）。返回值：该目录是否可枚举。
#[allow(clippy::too_many_arguments)]
fn scan_cluster_dir(
    dev: &dyn BlockDevice,
    bpb: &Bpb,
    fat: &Fat,
    start_cluster: u32,
    path: &str,
    depth: u32,
    out: &mut Vec<FatEntry>,
    observer: &mut dyn FnMut(&FatEntry),
) -> Result<bool, FatError> {
    if depth > MAX_DEPTH || out.len() > MAX_ENTRIES {
        return Ok(true); // 命中深度/容量上限：内容已尽量取到，不算不可读
    }
    let mut data = Vec::new();
    let mut buf = vec![0u8; bpb.cluster_bytes() as usize];
    let chain = match fat.chain(start_cluster) {
        Ok(c) => c,
        Err(_) => return Ok(false), // 坏目录链：该目录无法开始枚举（调用方决定降级方式）
    };
    let mut got_any = false;
    for c in chain {
        let n = match dev.read_at(bpb.cluster_to_byte(c), &mut buf) {
            Ok(n) => n,
            Err(_) => break, // 读失败：解析已收集部分
        };
        if n == 0 {
            break; // 该簇在设备外
        }
        got_any = true;
        data.extend_from_slice(&buf[..n]); // 短读 → 只收已读部分（不得拿上一簇残字节当数据）
        if n < buf.len() {
            break;
        }
        if data.len() > 64 * 1024 * 1024 {
            break; // 防御：目录不可能这么大
        }
    }
    if !got_any {
        return Ok(false); // 首簇即读不到：不可枚举
    }
    let parsed = dirent::parse_directory_bytes(&data);
    append_parsed(dev, bpb, fat, parsed, path, depth, out, observer)?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn append_parsed(
    dev: &dyn BlockDevice,
    bpb: &Bpb,
    fat: &Fat,
    parsed: Vec<ParsedEntry>,
    path: &str,
    depth: u32,
    out: &mut Vec<FatEntry>,
    observer: &mut dyn FnMut(&FatEntry),
) -> Result<(), FatError> {
    for e in parsed {
        if out.len() >= MAX_ENTRIES {
            return Ok(()); // 上限跨目录共享，入口检查之外的兜底（单巨型目录也不得越顶）
        }
        if e.attr & 0x08 != 0 {
            continue; // 卷标（真实盘根目录必有一条）不是文件，与 dot 同理过滤
        }
        let ext = e
            .name
            .rsplit_once('.')
            .map(|(_, x)| x.to_ascii_lowercase())
            .unwrap_or_default();
        let quality = if e.is_dir {
            RecoverQuality::Complete
        } else if e.deleted {
            grade_deleted(fat, bpb, e.first_cluster, e.size)?
        } else {
            RecoverQuality::Complete
        };
        out.push(FatEntry {
            name: e.name.clone(),
            path: path.to_string(),
            size_bytes: e.size as u64,
            first_cluster: e.first_cluster,
            deleted: e.deleted,
            is_dir: e.is_dir,
            quality,
            ext,
        });
        let pushed = out.len() - 1;
        // 只递归存活目录（已删除目录的簇可能被再分配，M1a 不深入）
        if e.is_dir && !e.deleted && e.first_cluster >= 2 {
            let child_path = if path == "/" {
                format!("/{}", e.name)
            } else {
                format!("{path}/{}", e.name)
            };
            let readable = scan_cluster_dir(
                dev,
                bpb,
                fat,
                e.first_cluster,
                &child_path,
                depth + 1,
                out,
                observer,
            )?;
            if !readable {
                // 目录项本身可读但内容不可枚举 → 该条目标记不完整（Rec3）
                out[pushed].quality = RecoverQuality::MaybeDamaged;
            }
        }
        // 后序单回调：子目录降级已写回 out[pushed]，此处 quality 即终值
        observer(&out[pushed]);
    }
    Ok(())
}

/// 删除文件质量分级：按连续簇假设检查每个簇是否空闲。
fn grade_deleted(
    fat: &Fat,
    bpb: &Bpb,
    first_cluster: u32,
    size: u32,
) -> Result<RecoverQuality, FatError> {
    if size == 0 {
        return Ok(RecoverQuality::Complete);
    }
    if first_cluster < 2 {
        return Ok(RecoverQuality::MaybeDamaged); // 无簇信息（如删除后 first_cluster 被清零）
    }
    let need = size.div_ceil(bpb.cluster_bytes());
    let max_cluster = bpb.data_cluster_count() + 1;
    for i in 0..need {
        let c = first_cluster + i;
        if c > max_cluster {
            return Ok(RecoverQuality::MaybeDamaged); // 越界：表项不可信
        }
        match fat.is_free(c) {
            Ok(true) => {}                                // 确证空闲
            _ => return Ok(RecoverQuality::MaybeDamaged), // Err 与 Ok(false) 同路降级：宁可漏报不可错报
        }
    }
    Ok(RecoverQuality::Complete)
}

/// 读取文件内容（恰好 size 字节；设备边界/坏读早停 → 返回短于 size 的前缀，不伪造）。
/// 策略：存活文件只信 FAT 链（链短/坏 → 诚实短前缀；环 → 按链读到 need，内容可能自重复）；删除项
/// （M1a 语义：删除即清 FAT，链属他人）按连续簇回退。
/// 注意：返回值只有字节——"是否走了连续假设"由 `entry.deleted` 推断（M1d UI 文案据此）。
pub fn read_file(dev: &dyn BlockDevice, entry: &FatEntry) -> Result<Vec<u8>, FatError> {
    if entry.size_bytes == 0 || entry.first_cluster < 2 {
        return Ok(Vec::new());
    }
    let bpb = bpb::parse(dev)?;
    let size = entry.size_bytes as usize; // 目录项来源恒 ≤ u32::MAX（FatEntry 由 scan 产出）
    let need = (size as u32).div_ceil(bpb.cluster_bytes()) as usize;
    let clusters: Vec<u32> = if entry.deleted {
        // 连续回退。界与 grade_deleted 同源（count+1）；u64 累积防野生 first_cluster 的
        // u32 加法溢出（qual-t6 I4）。
        let max_cluster = bpb.data_cluster_count() as u64 + 1;
        let mut v = Vec::new();
        let mut c = entry.first_cluster as u64;
        while v.len() < need && c <= max_cluster {
            v.push(c as u32);
            c += 1;
        }
        v
    } else {
        // 只信链：坏 FAT 上连续猜读会交付他人数据且质量恒 Complete，调用方无从发现——宁可漏报不可错报；
        // 环链只按链读不猜测、不挂起（qual-t7 复审）。
        let fat = Fat::new(dev, &bpb);
        let chain = fat.chain(entry.first_cluster).unwrap_or_default();
        chain[..need.min(chain.len())].to_vec()
    };
    let mut out = Vec::with_capacity(size.min(clusters.len() * bpb.cluster_bytes() as usize));
    let mut buf = vec![0u8; bpb.cluster_bytes() as usize];
    for c in clusters {
        let n = match dev.read_at(bpb.cluster_to_byte(c), &mut buf) {
            Ok(n) => n,
            Err(_) => break, // 坏道/越界：保留已读前缀（与 scan 同规则，坏道跳过不中断）
        };
        if n == 0 {
            break; // 该簇在设备外
        }
        out.extend_from_slice(&buf[..n]); // 短读 → 只收已读部分（不得拿上一簇残字节当数据）
        if out.len() >= size || n < buf.len() {
            break;
        }
    }
    out.truncate(size);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xd_device::image::ImageFileDevice;

    fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(image).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        (f, dev)
    }

    #[test]
    fn finds_live_and_deleted_files() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "LIVE.TXT", b"alive")
            .add_file("/", "GONE.JPG", &[9u8; 700])
            .delete("/", "GONE.JPG")
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let live = entries.iter().find(|e| e.name == "LIVE.TXT").unwrap();
        assert!(!live.deleted);
        assert_eq!(live.quality, RecoverQuality::Complete);
        let gone = entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(gone.size_bytes, 700);
        assert_eq!(gone.quality, RecoverQuality::Complete); // 全簇空闲
    }

    #[test]
    fn deleted_name_loses_first_char_or_uses_lfn() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.JPG", &[1u8; 300])
            .delete("/", "GONE.JPG")
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let d = entries.iter().find(|e| e.deleted).unwrap();
        assert!(
            d.name.starts_with('?'),
            "expected '?ONE.JPG', got {}",
            d.name
        );
    }

    #[test]
    fn overwritten_clusters_grade_maybe_damaged() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "OLD.BIN", &[7u8; 1024])
            .delete("/", "OLD.BIN")
            .add_file("/", "NEW.BIN", &[9u8; 1024]) // 复用簇
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let old = entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(old.quality, RecoverQuality::MaybeDamaged);
    }

    #[test]
    fn recurses_subdirectories() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_subdir("/", "PHOTOS")
            .add_file("/PHOTOS", "IMG.JPG", &[3u8; 100])
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let img = entries.iter().find(|e| e.name == "IMG.JPG").unwrap();
        assert_eq!(img.path, "/PHOTOS");
        assert!(entries.iter().any(|e| e.name == "PHOTOS" && e.is_dir));
    }

    #[test]
    fn scans_fat32_and_fat12_images() {
        for mut builder in [
            xd_fixtures::FatImageBuilder::fat32(),
            xd_fixtures::FatImageBuilder::fat12(),
        ] {
            let image = builder.add_file("/", "K.TXT", b"ok").build();
            let (_f, dev) = dev_for(&image);
            let entries = scan(&dev).unwrap();
            assert!(
                entries.iter().any(|e| e.name == "K.TXT"),
                "scan failed for {:?}",
                entries
            );
        }
    }

    #[test]
    fn skips_volume_label_entries() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "REAL.TXT", b"x")
            .build();
        let mut patched = image.clone();
        let slot = 18 * 512; // FAT16 根目录第 1 槽
        patched[slot + 11] = 0x08; // 把该条目改成卷标属性
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        assert!(entries.is_empty(), "卷标不应出现在结果中: {entries:?}");
    }

    #[test]
    fn bad_subdir_chain_does_not_abort_scan() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_subdir("/", "PHOTOS")
            .add_file("/PHOTOS", "IMG.JPG", &[3u8; 100])
            .add_file("/", "ROOT.TXT", b"root")
            .build();
        let mut patched = image.clone();
        let base = 18 * 512; // FAT16 根目录区起点
        let pos = patched[base..base + 32 * 8]
            .chunks(32)
            .position(|c| &c[..5] == b"PHOTO")
            .unwrap();
        patched[base + pos * 32 + 26..base + pos * 32 + 28]
            .copy_from_slice(&0xFFFFu16.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        assert!(
            entries.iter().any(|e| e.name == "ROOT.TXT"),
            "坏子目录链不得中止全盘"
        );
        assert!(
            entries.iter().any(|e| e.name == "PHOTOS"),
            "坏目录项本身仍应列出"
        );
        assert!(
            !entries.iter().any(|e| e.name == "IMG.JPG"),
            "不可读目录不得产出条目"
        );
    }

    #[test]
    fn scan_errors_when_fat32_root_cluster_out_of_range() {
        // I2：根不可枚举必须 Err（与空盘 Ok([]) 区分）
        let image = xd_fixtures::FatImageBuilder::fat32()
            .add_file("/", "A.TXT", b"x")
            .build();
        let mut patched = image.clone();
        patched[44..48].copy_from_slice(&1_000_000u32.to_le_bytes()); // bpb 只验 ≥2
        let (_f, dev) = dev_for(&patched);
        assert!(matches!(scan(&dev), Err(FatError::InvalidBpb(m)) if m.contains("根目录不可读")));
    }

    #[test]
    fn scan_errors_when_root_region_unreadable() {
        // I2：截断到 boot+FAT 区内（root_start=9216 在设备外）→ 首读 0 字节 → Err
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "A.TXT", b"x")
            .build();
        let truncated = image[..5120].to_vec();
        let (_f, dev) = dev_for(&truncated);
        assert!(matches!(scan(&dev), Err(FatError::InvalidBpb(m)) if m.contains("根目录不可读")));
    }

    #[test]
    fn unreadable_subdir_marks_entry_maybe_damaged() {
        // Rec3：子目录不可枚举 → 已 push 的目录条目标记 MaybeDamaged，其余照常
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_subdir("/", "PHOTOS")
            .add_file("/PHOTOS", "IMG.JPG", &[3u8; 100])
            .add_file("/", "ROOT.TXT", b"root")
            .build();
        let mut patched = image.clone();
        let base = 18 * 512;
        let pos = patched[base..base + 32 * 8]
            .chunks(32)
            .position(|c| &c[..5] == b"PHOTO")
            .unwrap();
        patched[base + pos * 32 + 26..base + pos * 32 + 28].copy_from_slice(&5000u16.to_le_bytes()); // 5000 > data_cluster_count()+1
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let dir = entries.iter().find(|e| e.name == "PHOTOS").unwrap();
        assert!(dir.is_dir);
        assert_eq!(dir.quality, RecoverQuality::MaybeDamaged);
        assert!(
            entries
                .iter()
                .any(|e| e.name == "ROOT.TXT" && e.quality == RecoverQuality::Complete)
        );
        assert!(!entries.iter().any(|e| e.name == "IMG.JPG"));
    }

    #[test]
    fn deleted_entry_without_cluster_info_grades_maybe_damaged() {
        // M3：first_cluster < 2（簇信息缺失）→ MaybeDamaged
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &[4u8; 512])
            .delete("/", "GONE.BIN")
            .build();
        let mut patched = image.clone();
        let de = patched
            .windows(32)
            .position(|w| w[0] == 0xE5 && &w[8..11] == b"BIN")
            .unwrap();
        patched[de + 26..de + 28].copy_from_slice(&0u16.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let gone = entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(gone.quality, RecoverQuality::MaybeDamaged);
    }

    #[test]
    fn deleted_entry_with_out_of_range_cluster_grades_maybe_damaged() {
        // M3：first_cluster 越界（> count+1）且 size > 0 → MaybeDamaged
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &[4u8; 512])
            .delete("/", "GONE.BIN")
            .build();
        let mut patched = image.clone();
        let de = patched
            .windows(32)
            .position(|w| w[0] == 0xE5 && &w[8..11] == b"BIN")
            .unwrap();
        patched[de + 26..de + 28].copy_from_slice(&5000u16.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let gone = entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(gone.quality, RecoverQuality::MaybeDamaged);
    }

    #[test]
    fn ext_is_lowercase_suffix_or_empty() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "A.JPG", b"x")
            .add_file("/", "NOEXT", b"y")
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        assert_eq!(
            entries.iter().find(|e| e.name == "A.JPG").unwrap().ext,
            "jpg"
        );
        assert_eq!(entries.iter().find(|e| e.name == "NOEXT").unwrap().ext, "");
    }

    #[test]
    fn reads_live_file_exactly() {
        let data: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "DATA.BIN", &data)
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.name == "DATA.BIN").unwrap();
        let bytes = read_file(&dev, e).unwrap();
        assert_eq!(bytes, data);
    }

    #[test]
    fn reads_deleted_file_via_contiguous_fallback() {
        let data: Vec<u8> = (0..1200u32).map(|i| (i % 253) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &data)
            .delete("/", "GONE.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.deleted).unwrap();
        let bytes = read_file(&dev, e).unwrap();
        assert_eq!(bytes, data); // 删除后 FAT 链已清 → 连续回退精确还原
    }

    #[test]
    fn deleted_entry_ignores_reused_chain_reads_original() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &data)
            .delete("/", "GONE.BIN")
            .build();
        let mut patched = image.clone();
        // 模拟非连续复用：entry(2)=7、entry(7)=8、entry(8)=EOC（旧文件数据仍在簇 2,3）
        patched[516..518].copy_from_slice(&7u16.to_le_bytes());
        patched[526..528].copy_from_slice(&8u16.to_le_bytes());
        patched[528..530].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.deleted).unwrap();
        let bytes = read_file(&dev, e).unwrap();
        assert_eq!(bytes, data); // 判据修正：删除项走连续回退而非他人链
    }

    #[test]
    fn reads_exact_size_not_full_cluster() {
        let data = b"short".to_vec(); // 5 字节 < 1 簇
        let image = xd_fixtures::FatImageBuilder::fat32()
            .add_file("/", "S.TXT", &data)
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.name == "S.TXT").unwrap();
        assert_eq!(read_file(&dev, e).unwrap(), data);
    }

    #[test]
    fn wild_first_cluster_is_bounded_not_panic() {
        // I4：u32 高位被污染的删除项（first_cluster≈0xFFFFFE00）→ 连续回退必须 u64 累积 +
        // 同界截断，不得 u32 加法溢出 panic（旧 `(0..need).map(|i| first_cluster + i)` 在此 debug 溢出）
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "X.TXT", b"x")
            .build();
        let mut patched = image.clone();
        // 扩大卷几何使 count+1 逼近 u32 上界（否则界本身先挡住溢出路径）
        patched[19..21].copy_from_slice(&0u16.to_le_bytes()); // 16 位总数清零 → 走 32 位字段
        patched[32..36].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let e = FatEntry {
            name: "WILD.BIN".into(),
            path: "/".into(),
            size_bytes: 266_240, // need=520 ≥ u32::MAX - 0xFFFFFE00 + 1 = 513 → 旧实现必溢出
            first_cluster: 0xFFFF_FE00,
            deleted: true,
            is_dir: false,
            quality: RecoverQuality::MaybeDamaged,
            ext: "bin".into(),
        };
        let bytes = read_file(&dev, &e).unwrap(); // 不得 panic
        assert!(bytes.is_empty(), "界截断：不得越界读，也不得伪造");
    }

    #[test]
    fn truncated_device_returns_read_prefix() {
        // 坏道/设备截断：只交付已读前缀，不伪造、不拿残字节
        let data: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "DATA.BIN", &data)
            .build();
        let truncated = image[..26_312].to_vec(); // 数据区簇2全 + 簇3前200B（data_start=50扇区）
        let (_f, dev) = dev_for(&truncated);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.name == "DATA.BIN").unwrap();
        let bytes = read_file(&dev, e).unwrap();
        assert_eq!(bytes.len(), 712); // 512 + 200
        assert_eq!(bytes, data[..712]);
    }

    #[test]
    fn live_file_with_broken_chain_returns_short_prefix_not_guess() {
        // qual-t7 I1：坏 FAT 上的活文件不得连续猜读（会全尺寸交付他人数据且质量恒 Complete）
        let data: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "DATA.BIN", &data)
            .build();
        let mut patched = image.clone();
        patched[516..518].copy_from_slice(&0xFFFFu16.to_le_bytes()); // FAT[2]=EOC：链只剩首簇
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.name == "DATA.BIN").unwrap();
        let bytes = read_file(&dev, e).unwrap();
        assert_eq!(bytes.len(), 512); // 诚实短前缀：不预测簇 3/4
        assert_eq!(bytes, data[..512]);
    }

    #[test]
    fn read_file_terminates_on_chain_loop() {
        // 活文件自环链：不挂起、不越读；按链读到 need（内容为簇 2 自重复——FAT 所指如此，非他人数据）
        let data: Vec<u8> = (0..1200u32).map(|i| (i % 241) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "LOOP.BIN", &data)
            .build();
        let mut patched = image.clone();
        patched[516..518].copy_from_slice(&2u16.to_le_bytes()); // FAT[2]=2 自环
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.name == "LOOP.BIN").unwrap();
        let bytes = read_file(&dev, e).unwrap();
        assert_eq!(bytes.len(), 1200); // 环链长 count+2 ≥ need → 全尺寸（非短前缀）
        assert_eq!(&bytes[..512], &data[..512]);
        assert_eq!(&bytes[512..1024], &data[..512]); // 簇 2 自重复：锁定"按链读"语义
    }

    #[test]
    fn zero_size_or_missing_cluster_info_returns_empty() {
        // size=0（空文件）与 first_cluster<2（簇信息缺失）→ Ok([])（字节层诚实为空）
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "EMPTY.BIN", b"")
            .add_file("/", "GONE.BIN", &[4u8; 512])
            .delete("/", "GONE.BIN")
            .build();
        let mut patched = image.clone();
        let de = patched
            .windows(32)
            .position(|w| w[0] == 0xE5 && &w[8..11] == b"BIN")
            .unwrap();
        patched[de + 26..de + 28].copy_from_slice(&0u16.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let empty = entries.iter().find(|e| e.name == "EMPTY.BIN").unwrap();
        assert_eq!(read_file(&dev, empty).unwrap(), Vec::<u8>::new());
        let gone = entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(read_file(&dev, gone).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn observer_streams_post_order_with_final_quality() {
        // 后序：子项 IN.TXT 先于目录 DIR 回调，根文件 ROOT.TXT 在 DIR 后
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_subdir("/", "DIR")
            .add_file("/DIR", "IN.TXT", b"x")
            .add_file("/", "ROOT.TXT", b"y")
            .build();
        let (_f, dev) = dev_for(&image);
        let mut seen: Vec<String> = Vec::new();
        let entries = scan_with_observer(&dev, &mut |e| seen.push(e.name.clone())).unwrap();
        assert_eq!(
            seen,
            vec!["IN.TXT", "DIR", "ROOT.TXT"],
            "后序：子项在目录前"
        );
        assert_eq!(entries.len(), 3, "返回值与整表同源");
    }

    #[test]
    fn observer_sees_downgraded_dir_quality() {
        // 坏目录链（首簇越界，既有手法）→ observer 收到 DIR 时 quality 已是终值 MaybeDamaged
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_subdir("/", "DIR")
            .add_file("/DIR", "IN.TXT", b"x")
            .build();
        let mut patched = image.clone();
        let base = 18 * 512; // FAT16 根目录区起点
        let pos = patched[base..base + 32 * 8]
            .chunks(32)
            .position(|c| &c[..3] == b"DIR")
            .unwrap();
        patched[base + pos * 32 + 26..base + pos * 32 + 28].copy_from_slice(&5000u16.to_le_bytes()); // > data_cluster_count()+1
        let (_f, dev) = dev_for(&patched);
        let mut seen: Vec<(String, RecoverQuality)> = Vec::new();
        scan_with_observer(&dev, &mut |e| seen.push((e.name.clone(), e.quality))).unwrap();
        let dir = seen.iter().find(|(n, _)| n == "DIR").unwrap();
        assert_eq!(dir.1, RecoverQuality::MaybeDamaged, "流式层拿到终值分级");
    }
}
