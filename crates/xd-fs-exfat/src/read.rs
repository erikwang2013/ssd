// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 文件读取：按分配拓扑（NoFatChain 连续 / FAT 链 / 删除项 stale 链回退）重建字节流。
//! 交付长度 = min(VDL,DL)（T4 保证 VDL ≤ DL ⇒ 即 `size_bytes`）——`[VDL,DL)` 是未初始化区，
//! **绝不交付**；设备边界/坏读 → 诚实短前缀。
//! 删除项的分配权威是**位图**（FAT 已 stale）；stale 链**仅在前缀全空闲时可信**；链被证伪后
//! 退连续**＝放弃链证据的猜读**，交付可能错位（M1b 决策项：deleted+!contiguous 是否改为只沿链
//! 走到首个非空闲/断链簇）。

use crate::ExfatError;
use crate::bitmap::Bitmap;
use crate::boot::{self, ExfatBoot};
use crate::dirent;
use crate::fattab::Fat32;
use crate::scan::ExfatEntry;
use xd_device::BlockDevice;

/// texFAT 双位图选择：优先活动位图（bitmaps[k].0 == active），缺失则回退任一。
pub(crate) fn pick_bitmap(bitmaps: &[(bool, u32, u64)], active: u8) -> Option<(u32, u64)> {
    let want_second = active == 1;
    bitmaps
        .iter()
        .find(|(second, _, _)| *second == want_second)
        .or_else(|| bitmaps.first())
        .map(|(_, f, l)| (*f, *l))
}

/// 由已解析的 specials 加载位图（scan 复用根快照；qual-t5 M1）。
/// 任何失败 → None（分级层即降级，绝不回退 FAT）。
pub(crate) fn load_bitmap_from_specials(
    dev: &dyn BlockDevice,
    boot: &ExfatBoot,
    fat: &Fat32,
    specials: &dirent::Specials,
) -> Option<Bitmap> {
    let (first, len) = pick_bitmap(&specials.bitmaps, boot.active_fat_index())?;
    Bitmap::load(dev, boot, fat, first, len).ok()
}

/// 自读根目录再转发（`read_file` 用——它没有现成 specials）。
fn load_bitmap(dev: &dyn BlockDevice, boot: &ExfatBoot, fat: &Fat32) -> Option<Bitmap> {
    let root_data = crate::scan::read_root_dir(dev, boot, fat).ok()?;
    let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
    load_bitmap_from_specials(dev, boot, fat, &root.specials)
}

pub(crate) enum Resolved {
    /// 链序簇号，**已切到 `need` 前缀**（各消费点只用前缀，无损；qual-t6 Minor 4）
    Chain(Vec<u32>),
    Contiguous {
        first: u32,
        n: u64,
    },
}

/// 簇定位（`read_subdir_bytes` / `grade_deleted` / `read_file` 三处共用）。
/// contiguous ⇒ 连续（NoFatChain 规范保证）；否则链覆盖 need 才用链（只取 need 前缀），短/坏链退连续。
/// None ⇔ 起点非法或 need 超出可达簇数 → 调用方各自降级。只定位、不物化连续段（qual-t5 I1）。
pub(crate) fn resolve_clusters(
    boot: &ExfatBoot,
    fat: &Fat32,
    first_cluster: u32,
    need: u64,
    contiguous: bool,
) -> Option<Resolved> {
    let max_cluster = boot.cluster_count as u64 + 1;
    if !(2..=max_cluster).contains(&(first_cluster as u64)) {
        return None;
    }
    let reachable = max_cluster - first_cluster as u64 + 1;
    if need == 0 || need > reachable {
        return None;
    }
    if !contiguous
        && let Ok(chain) = fat.chain(first_cluster)
        && chain.len() as u64 >= need
    {
        return Some(Resolved::Chain(chain[..need as usize].to_vec()));
    }
    Some(Resolved::Contiguous {
        first: first_cluster,
        n: need,
    })
}

/// 读取文件内容（交付 min(VDL,DL) = size_bytes 字节；设备边界/坏读早停 → 诚实短前缀）。
/// 注意：返回值只有字节，拓扑不可由返回推断——deleted ⇒ 拓扑不可知（可能 stale 链、可能连续
/// 猜读），M1d 文案不得称其为连续假设；live+contiguous 才是真连续。
pub fn read_file(dev: &dyn BlockDevice, entry: &ExfatEntry) -> Result<Vec<u8>, ExfatError> {
    if entry.size_bytes == 0 || entry.first_cluster < 2 {
        return Ok(Vec::new());
    }
    let boot = boot::parse(dev)?;
    let fat = Fat32::new(dev, &boot);
    let cb = boot.cluster_bytes();
    // 交付 = min(VDL,DL)：T4 已保证 VDL ≤ DL；直构条目绕开 T4 时也不得越 DL 交付（qual-t6 C1 旁）
    let size = entry.size_bytes.min(entry.data_length) as usize;
    let need = entry.data_length.div_ceil(cb);
    let bitmap = if entry.deleted {
        load_bitmap(dev, &boot, &fat) // 删除项：位图是分配权威（FAT 已 stale）
    } else {
        None
    };

    // 容量提示：三重钳位（size 与 need 均来自目录项、无上界；need*cb 的乘法在 isize 之类
    // 极端输入下会溢出/巨分配——qual-t6 C1）。提示只影响首配，不影响可交付长度。
    let cap = (size as u64).min(dev.size_bytes()).min(64 * 1024 * 1024) as usize;
    let mut out = Vec::with_capacity(cap);
    let mut buf = vec![0u8; cb as usize];
    if !entry.deleted && !entry.contiguous {
        // live 链式：只信链（链短/坏 → 诚实短前缀）——绝不连续猜读：坏 FAT 上猜读会交付
        // 他人数据且无从发现（M1a qual-t7 I1 对等）
        let chain = fat.chain(entry.first_cluster).unwrap_or_default();
        let n = (need as usize).min(chain.len());
        read_prefix(
            dev,
            &boot,
            chain[..n].iter().copied(),
            None,
            size,
            &mut out,
            &mut buf,
        );
        out.truncate(size);
        return Ok(out);
    }

    let Some(resolved) = resolve_clusters(&boot, &fat, entry.first_cluster, need, entry.contiguous)
    else {
        // 起点非法 / need 超可达簇数 → 确定性为空（野生 first_cluster 同界截断，M1a I4 对等）
        return Ok(Vec::new());
    };
    // 删除项：stale 链（已是 need 前缀）全空闲（位图为准）才可用，否则连续回退
    //（I2：只查前缀——查整链会在链尾被占时丢一半可恢复数据）；位图不可读 → 无从证伪，用链
    let resolved = match resolved {
        Resolved::Chain(chain) if entry.deleted => {
            let usable = match &bitmap {
                Some(b) => chain.iter().all(|c| matches!(b.is_free(*c), Ok(true))),
                None => true,
            };
            if usable {
                Resolved::Chain(chain)
            } else {
                Resolved::Contiguous {
                    first: entry.first_cluster,
                    n: need,
                }
            }
        }
        r => r,
    };
    match resolved {
        Resolved::Chain(chain) => read_prefix(
            dev,
            &boot,
            chain.iter().copied(),
            bitmap.as_ref(),
            size,
            &mut out,
            &mut buf,
        ),
        Resolved::Contiguous { first, n } => {
            // 逐簇 first+i，不物化连续段（qual-t5 I1：need 可被污染放大）
            read_prefix(
                dev,
                &boot,
                (0..n).map(|i| (first as u64 + i) as u32),
                bitmap.as_ref(),
                size,
                &mut out,
                &mut buf,
            );
        }
    }
    out.truncate(size);
    Ok(out)
}

/// 逐簇读取前缀：坏读/设备外/短读即停；删除项遇被占用簇即停（保守前缀，流式截断，
/// 替代先物化簇表再 `truncate`——qual-t5 修订注 I1）。`out` 只收已读字节，绝不伪造。
fn read_prefix(
    dev: &dyn BlockDevice,
    boot: &ExfatBoot,
    clusters: impl Iterator<Item = u32>,
    bitmap: Option<&Bitmap>,
    size: usize,
    out: &mut Vec<u8>,
    buf: &mut [u8],
) {
    for c in clusters {
        if let Some(b) = bitmap
            && !matches!(b.is_free(c), Ok(true))
        {
            break;
        }
        let n = match dev.read_at(boot.cluster_to_byte(c), buf) {
            Ok(n) => n,
            Err(_) => break, // 坏读早停：保留已读前缀（与 scan 同规则）
        };
        if n == 0 {
            break; // 该簇在设备外
        }
        out.extend_from_slice(&buf[..n]); // 短读只收已读部分（不得拿上一簇残字节当数据）
        if out.len() >= size || n < buf.len() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot::testutil::dev_for;
    use crate::scan::{RecoverQuality, scan};

    const ROOT_B: usize = 32 * 512 + 3 * 4096;
    const SET: usize = ROOT_B + 96;
    const FAT_B: usize = 24 * 512;

    /// 独立重算 SetChecksum（16 位循环右移累加、跳下标 2/3）：
    /// 构造"污染 VDL/DL 但还原校验仍自洽"的删除项（否则 T4 会直接丢弃该项）。
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

    #[test]
    fn reads_contiguous_file_exact() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "A.BIN", &data)
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "A.BIN")
            .unwrap();
        assert_eq!(read_file(&dev, &e).unwrap(), data);
    }

    #[test]
    fn reads_chained_fragmented_in_order() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "F.BIN", &data, &[7, 6, 8], false)
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "F.BIN")
            .unwrap();
        assert_eq!(read_file(&dev, &e).unwrap(), data, "必须按链序 7→6→8 拼接");
    }

    #[test]
    fn deleted_contiguous_reads_exact() {
        let data: Vec<u8> = (0..12000u32).map(|i| (i % 253) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "G.BIN", &data)
            .delete("/", "G.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(read_file(&dev, &e).unwrap(), data);
    }

    #[test]
    fn deleted_chained_uses_stale_chain_when_bitmap_free() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 241) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "G.BIN", &data)
            .delete("/", "G.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(
            read_file(&dev, &e).unwrap(),
            data,
            "位图全空 → stale 链可用（仍是原数据）"
        );
    }

    #[test]
    fn deleted_chained_occupied_middle_cluster_truncates_prefix() {
        // OLD.BIN 链式 3 簇（6,7,8）删除后，簇 7/8 被 NEW.BIN 复用 → 只交付簇 6 的前缀
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 239) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "OLD.BIN", &data)
            .delete("/", "OLD.BIN")
            .add_file_in_clusters("/", "NEW.BIN", &[5u8; 4500], &[7, 8], false)
            .build();
        let (_f, dev) = dev_for(&image);
        let old = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "OLD.BIN")
            .unwrap();
        assert_eq!(old.quality, RecoverQuality::MaybeDamaged);
        let bytes = read_file(&dev, &old).unwrap();
        assert_eq!(bytes.len(), 4096, "只交付首个空闲簇");
        assert_eq!(bytes, data[..4096]);
    }

    #[test]
    fn deleted_chain_tail_occupied_prefix_free_delivers_full() {
        // I2 的读半壁（对 T5 的 deleted_chain_grades_on_need_prefix_only）：stale 链 [6,9,7,10]，
        // 链尾 7/10 已是 NEW.BIN 的链；need=2 前缀 [6,9] 全空闲 → 必须按 stale 链交付 8192。
        // 查整链（旧稿）会回退连续 [6,7] 再被位图截断 → 只交付 4096（qual-t5 探针）。
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 229) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &data, &[6, 9, 7], false)
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
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "OLD.BIN")
            .unwrap();
        assert!(e.deleted);
        assert_eq!(
            e.quality,
            RecoverQuality::Complete,
            "T5 判据：前缀全空 → Complete"
        );
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes.len(), 8192, "链尾被占不得回退连续（会只交付 4096）");
        assert_eq!(bytes, data[..8192], "按链序 6→9 拼接");
    }

    #[test]
    fn vdl_lt_dl_delivers_vdl_only() {
        // 磁盘上有 9000 字节真实数据，但 VDL=5000 → 交付绝不越过 VDL
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 233) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_with_vdl("/", "V.BIN", &data, 5000)
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "V.BIN")
            .unwrap();
        assert_eq!(e.size_bytes, 5000);
        assert_eq!(e.data_length, 9000);
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes.len(), 5000, "严禁交付 [VDL,DL) 未初始化区");
        assert_eq!(bytes, data[..5000]);
    }

    #[test]
    fn live_broken_chain_returns_short_prefix_not_guess() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "A.BIN", &data)
            .build();
        image[FAT_B + 6 * 4..FAT_B + 6 * 4 + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // FAT[6]=EOC
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "A.BIN")
            .unwrap();
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes.len(), 4096, "live 只信链：链只剩首簇 → 诚实短前缀");
        assert_eq!(bytes, data[..4096]);
    }

    #[test]
    fn wild_first_cluster_bounded_not_panic() {
        // 野生 first_cluster + 大 DL → u64 累积 + 同界截断；不得 panic、不得伪造
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "X.TXT", b"x")
            .build();
        let (_f, dev) = dev_for(&image);
        let e = ExfatEntry {
            name: "WILD.BIN".into(),
            path: "/".into(),
            size_bytes: 1 << 20,
            data_length: 1 << 20,
            first_cluster: 0xFFFF_FE00,
            deleted: true,
            is_dir: false,
            contiguous: true,
            quality: RecoverQuality::MaybeDamaged,
            ext: "bin".into(),
        };
        let bytes = read_file(&dev, &e).unwrap();
        assert!(bytes.is_empty(), "界截断：确定性为空");
    }

    #[test]
    fn truncated_device_returns_read_prefix() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "A.BIN", &data)
            .build();
        // 截断到簇 6 全 + 簇 7 前 200 字节（文件簇 6,7,8 起于 heap+4*4096）
        let cut = 32 * 512 + 4 * 4096 + 4096 + 200;
        let truncated = image[..cut].to_vec();
        let (_f, dev) = dev_for(&truncated);
        // 根目录在簇 5（完整）→ 可扫出条目
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "A.BIN")
            .unwrap();
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes.len(), 4096 + 200);
        assert_eq!(bytes, data[..4096 + 200]);
    }

    #[test]
    fn polluted_lengths_read_file_stays_bounded_without_panic() {
        // qual-t6 C1 / 探针 D：DL 污染成 u64::MAX（重算还原校验以过 T4 门槛）→ read_file 不得因
        // need*cb 乘法溢出（debug panic）或巨分配（release abort），必须确定性 Ok(empty)
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        let mut patched = image.clone();
        let stream = SET + 32;
        patched[stream + 24..stream + 32].copy_from_slice(&u64::MAX.to_le_bytes()); // DataLength
        refix_deleted_checksum(&mut patched, SET, 3);
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::MaybeDamaged);
        assert!(
            read_file(&dev, &e).unwrap().is_empty(),
            "need ≫ 可达簇数 → 确定性空，且不得在容量提示上乘爆"
        );

        // release 半壁：DL=(1<<52)+4096 → need*cb ≈ 4.5PB（乘法不溢出但巨分配；旧式在 release
        // 直接 Abort）→ 仍须 Ok(empty)
        let mut patched2 = image.clone();
        patched2[stream + 24..stream + 32].copy_from_slice(&((1u64 << 52) + 4096).to_le_bytes());
        refix_deleted_checksum(&mut patched2, SET, 3);
        let (_f2, dev2) = dev_for(&patched2);
        let e2 = scan(&dev2)
            .unwrap()
            .into_iter()
            .find(|e| e.deleted)
            .unwrap();
        assert!(
            read_file(&dev2, &e2).unwrap().is_empty(),
            "巨 need 不得触发巨分配"
        );

        // VDL 半壁：直构条目绕开 T4 的 vdl ≤ dl 门槛（VDL=u64::MAX、DL=9000）→ 不崩，
        // 交付按 min(VDL,DL) 封顶在 DL（多读的簇在 truncate 前就被 size 截住）
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 197) as u8).collect();
        let image3 = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "V.BIN", &data)
            .build();
        let (_f3, dev3) = dev_for(&image3);
        let base = scan(&dev3)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "V.BIN")
            .unwrap();
        let poisoned = ExfatEntry {
            size_bytes: u64::MAX,
            ..base
        };
        let bytes = read_file(&dev3, &poisoned).unwrap();
        assert_eq!(bytes, data, "VDL 无上界 → 仍只交付 min(VDL,DL)=DL");
    }

    #[test]
    fn deleted_with_unreadable_bitmap_uses_stale_chain() {
        // 位图不可读（0x81 首簇越界）→ 无从证伪 → 仍沿 stale 链交付全量（钉 `None => true` 分支）
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 227) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "G.BIN", &data)
            .delete("/", "G.BIN")
            .build();
        let mut patched = image.clone();
        patched[ROOT_B + 32 + 20..ROOT_B + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::MaybeDamaged, "位图不可读 → 封顶");
        assert_eq!(
            read_file(&dev, &e).unwrap(),
            data,
            "位图不可读 → 无从证伪 → stale 链照用"
        );
    }

    #[test]
    fn vdl_zero_with_dl_positive_returns_empty() {
        // VDL=0、DL=9000（夹具 vdl=9000 需重算校验）→ 交付空，绝不下探 [VDL,DL)
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "Z.BIN", &[3u8; 9000])
            .delete("/", "Z.BIN")
            .build();
        let mut patched = image.clone();
        let stream = SET + 32;
        patched[stream + 8..stream + 16].copy_from_slice(&0u64.to_le_bytes()); // ValidDataLength=0
        refix_deleted_checksum(&mut patched, SET, 3);
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.size_bytes, 0);
        assert_eq!(e.data_length, 9000);
        assert!(read_file(&dev, &e).unwrap().is_empty(), "VDL=0 → 空交付");
    }

    #[test]
    fn pick_bitmap_prefers_active_and_falls_back() {
        // 纯函数级：texFAT 双位图选择（随 pick_bitmap 迁入本模块）
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
}
