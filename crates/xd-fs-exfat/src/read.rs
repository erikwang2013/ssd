// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 文件读取：按分配拓扑（NoFatChain 连续 / FAT 链 / 删除项 stale 链）重建字节流。
//! 交付长度 = min(VDL,DL)（T4 保证 VDL ≤ DL ⇒ 即 `size_bytes`）——`[VDL,DL)` 是未初始化区，
//! **绝不交付**；设备边界/坏读 → 诚实短前缀。
//! 删除项的分配权威是**位图**（FAT 已 stale）。**M1b (a) 裁定**：deleted+NoFatChain=0 → 只沿
//! stale 链走到首个被占用/断裂簇（诚实短前缀），绝不连续猜读；deleted+NoFatChain=1 → 连续为
//! 规范保证。碎裂删除场景的正解是 M1c 雕刻。
//! 链前缀回访（环/回折）**刻意不对称**：deleted 回访即空（stale 链整体是不可信证据，回访=自证伪）；
//! live 截断到首个回访点（FAT 是权威分配记录，逐跳可信至首次矛盾）。

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

/// 簇定位（`read_subdir_bytes` 与 `read_file` 两处共用；`grade_deleted` 自 (a) 起不再使用）。
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
/// 拓扑可由 `entry.contiguous` 表述（deleted+contiguous=规范保证连续；deleted+!contiguous=按删除链；
/// live+!contiguous=只信 FAT 链——M1d 文案据此，不得再称"可能连续猜读"）。
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
    // (a) 裁定（M1b）：删除项 + 非连续 → **只沿 stale 链**（删除留下的指纹；首个被占用/坏读簇
    // 即止 → 诚实短前缀），绝不回退连续猜读——旧式"链被证伪后退连续"会交付错位数据而无从发现。
    // contiguous=true 的删除项是 NoFatChain 规范保证，不走此分支。
    // 前缀回访 → 空交付（stale 链整体不可信、回访=自证伪→整链弃；与 live 的截断刻意不对称）。
    if entry.deleted && !entry.contiguous {
        let max_cluster = boot.cluster_count as u64 + 1;
        let fc = entry.first_cluster as u64;
        // (a) 可达界卫（保留旧 resolve_clusters 的对等界卫）：need 不得超过 fc→堆尾的物理簇数——
        // 否则链在说谎（自环/回折 + DL 污染可造出 len 充足但物理不可能的链）→ 空交付
        if !(2..=max_cluster).contains(&fc) || need > max_cluster - fc + 1 {
            return Ok(Vec::new());
        }
        let chain = fat.chain(entry.first_cluster).unwrap_or_default();
        let n = (need as usize).min(chain.len());
        // 链前缀不得回访簇：回访=环/回折（如 FAT 自环 → [252,252,…]），交付会重复同一簇字节（伪造序）
        let mut sorted = chain[..n].to_vec();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1]) {
            return Ok(Vec::new());
        }
        read_prefix(
            dev,
            &boot,
            chain[..n].iter().copied(),
            bitmap.as_ref(),
            size,
            &mut out,
            &mut buf,
        );
        out.truncate(size);
        return Ok(out);
    }
    if !entry.deleted && !entry.contiguous {
        // live 链式：只信链（链短/坏 → 诚实短前缀）——绝不连续猜读：坏 FAT 上猜读会交付
        // 他人数据且无从发现（M1a qual-t7 I1 对等）
        let chain = fat.chain(entry.first_cluster).unwrap_or_default();
        // 前缀回访（环/回折）→ 只交付首个回访点之前的簇（live 的"诚实短前缀"语义，与链短同规则；
        // 与 deleted 车道的"回访即空"刻意不对称：deleted 的 stale 链整体是不可信证据、回访=自证伪→
        // 整链弃；live 的 FAT 是权威分配记录，逐跳可信至首次矛盾（回访）即止）
        // ponytail: O(scan_end²) 比较；scan_end ≤ need（原全链扫描在 1M 簇链上 ≈5e11 次比较——
        // qual-t4 性能项）。need 若未来巨化，改 seen 位图 O(n)。
        let scan_end = (need as usize).min(chain.len());
        let end = (0..scan_end)
            .find(|&i| chain[..i].contains(&chain[i]))
            .unwrap_or(chain.len());
        let n = (need as usize).min(end);
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
    fn deleted_contiguous_middle_occupied_truncates_prefix() {
        // qual-t4 G2（minor 补强：占 3 簇的**中段**）：逐簇门控必须 stop 不能 skip——跳过被占簇
        // 续读会交付 8192 = 簇 6+8 错位数据；分级同步降级（连续车道=位图把关，链式车道=stale 链把关）
        let data: Vec<u8> = (0..12288u32).map(|i| (i % 239) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "G.BIN", &data)
            .delete("/", "G.BIN")
            .add_file_in_clusters("/", "NEW.BIN", &[5u8; 100], &[7], true)
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "G.BIN")
            .unwrap();
        assert!(e.deleted);
        assert_eq!(
            e.quality,
            RecoverQuality::MaybeDamaged,
            "中段被占 → 位图门控降级"
        );
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes.len(), 4096, "中段被占 → 逐簇门控截断");
        assert_eq!(bytes, data[..4096]);
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
    fn deleted_wiped_stale_chain_delivers_honest_prefix() {
        // (a) 探针 B：碎片化删除项（链序 [6,9,7] ≠ 物理序），删除后 stale 链被清（FAT[6]=0，
        // 如部分工具删除时清链）→ 只沿链走到链断：仅簇 6 可交付（4096B）。旧式"链证伪退连续"
        // 会读入连续 [6,7,8] 三簇并按 size 截断交付 9000B 错位数据（12288 仅为原始读取量）。
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 223) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &data, &[6, 9, 7], false)
            .delete("/", "OLD.BIN")
            .build();
        let mut patched = image.clone();
        patched[FAT_B + 6 * 4..FAT_B + 6 * 4 + 4].copy_from_slice(&0u32.to_le_bytes()); // 清链首跳
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::MaybeDamaged, "链证不足 → 封顶");
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(
            bytes,
            data[..4096],
            "只交付链上确证的第一个簇，绝不连续猜读"
        );
    }

    #[test]
    fn deleted_occupied_chain_cluster_stops_even_if_contiguous_free() {
        // (a) 探针 C：链 [6,9,7]，簇 9 被 NEW.BIN 复用（位图置位；FAT[9] 保留旧值 7——NEW.BIN
        // 连续不写 FAT）→ 链从 6 走到 9 即止、且簇 9 被占用 → 只交付簇 6（4096B）；
        // 连续区间 [6,7,8] 全空闲也不得猜读。
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 211) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &data, &[6, 9, 7], false)
            .delete("/", "OLD.BIN")
            .add_file_in_clusters("/", "NEW.BIN", &[5u8; 4000], &[9], true)
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "OLD.BIN")
            .unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::MaybeDamaged,
            "链 [6,9,7] 中被占簇 9 截断（NEW.BIN 连续不写 FAT，FAT[9] 保留旧值）"
        );
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes, data[..4096], "被占用簇即止；连续区间空闲≠可猜");
    }

    #[test]
    fn deleted_loop_chain_beyond_reachable_delivers_nothing() {
        // 同 scan 侧构造：无界卫时交付 3×同一簇的重复字节（伪造序）——界卫必须空交付
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &[7u8; 4000], &[253], false)
            .delete("/", "OLD.BIN")
            .build();
        let mut patched = image.clone();
        let stream = SET + 32;
        patched[stream + 8..stream + 16].copy_from_slice(&12288u64.to_le_bytes());
        patched[stream + 24..stream + 32].copy_from_slice(&12288u64.to_le_bytes());
        patched[24 * 512 + 253 * 4..24 * 512 + 253 * 4 + 4].copy_from_slice(&253u32.to_le_bytes());
        refix_deleted_checksum(&mut patched, SET, 3);
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "OLD.BIN")
            .unwrap();
        assert!(
            read_file(&dev, &e).unwrap().is_empty(),
            "物理不可能的链 → 确定性空（不得重复交付同一簇）"
        );
    }

    #[test]
    fn deleted_nonrevisit_chain_beyond_reachable_delivers_nothing() {
        // qual-t4 G1：链 253→6→7 无回访但越过可达（253 是末簇，reachable=1 < need=3）
        // ——只有可达界卫能拦（回访检测不触发）→ 空交付
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &[7u8; 4000], &[253], false)
            .delete("/", "OLD.BIN")
            .build();
        let mut patched = image.clone();
        let stream = SET + 32;
        patched[stream + 8..stream + 16].copy_from_slice(&12288u64.to_le_bytes()); // VDL
        patched[stream + 24..stream + 32].copy_from_slice(&12288u64.to_le_bytes()); // DL：need=3
        patched[24 * 512 + 253 * 4..24 * 512 + 253 * 4 + 4].copy_from_slice(&6u32.to_le_bytes()); // FAT[253]=6
        patched[24 * 512 + 6 * 4..24 * 512 + 6 * 4 + 4].copy_from_slice(&7u32.to_le_bytes()); // FAT[6]=7
        refix_deleted_checksum(&mut patched, SET, 3);
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "OLD.BIN")
            .unwrap();
        assert!(
            read_file(&dev, &e).unwrap().is_empty(),
            "非回访链越过可达 → 界卫空交付"
        );
    }

    #[test]
    fn deleted_loop_prefix_within_reachable_delivers_nothing() {
        // 同 scan 侧构造：无检测时交付 8192B（同一 4096B 簇读两次）——前缀回访必须空交付
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &[7u8; 4000], &[252], false)
            .delete("/", "OLD.BIN")
            .build();
        let mut patched = image.clone();
        let stream = SET + 32;
        patched[stream + 8..stream + 16].copy_from_slice(&8192u64.to_le_bytes());
        patched[stream + 24..stream + 32].copy_from_slice(&8192u64.to_le_bytes());
        patched[24 * 512 + 252 * 4..24 * 512 + 252 * 4 + 4].copy_from_slice(&252u32.to_le_bytes());
        refix_deleted_checksum(&mut patched, SET, 3);
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "OLD.BIN")
            .unwrap();
        assert!(
            read_file(&dev, &e).unwrap().is_empty(),
            "前缀回访 → 确定性空，不得重复交付同一簇"
        );
    }

    #[test]
    fn deleted_noncontiguous_exact_fit_is_complete_and_delivers_full() {
        // 边界相等：fc=252、链 252→253、need=2=reachable、无回访 → 界卫与回访检测都不得误拒
        let data: Vec<u8> = (0..8192u32).map(|i| (i % 233) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "G.BIN", &data, &[252, 253], false)
            .delete("/", "G.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "G.BIN")
            .unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::Complete,
            "链足 need 且前缀全空闲 → Complete（边界相等不误拒）"
        );
        assert_eq!(read_file(&dev, &e).unwrap(), data, "无回访 → 全量交付");
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
    fn live_loop_chain_delivers_prefix_up_to_first_revisit() {
        // qual-t4 追加：live 链自环 FAT[6]=6 → 只交付首个回访点之前的簇（4096），绝不让同一簇重复出现；
        // 质量仍是 entry 层 checksum 语义（本测钉住该刻意行为，M1c 若升级链感知分级再改）
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "L.BIN", &data, &[6, 7, 8], false)
            .build();
        image[FAT_B + 6 * 4..FAT_B + 6 * 4 + 4].copy_from_slice(&6u32.to_le_bytes()); // 自环
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "L.BIN")
            .unwrap();
        assert_eq!(
            e.quality,
            RecoverQuality::Complete,
            "live 质量=entry 层语义（刻意）"
        );
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes.len(), 4096, "首个回访点之前恰一簇");
        assert_eq!(bytes, data[..4096]);
    }

    #[test]
    fn live_polluted_lengths_stay_chain_bounded() {
        // qual-t4 G3：live 链式单簇 [100]、VDL/DL 均污染成 12288（need=3）→ 交付只来自链上实簇：
        // 恰 4096B（== 真数据），绝不按 DL 放大或连续猜读（链界住）。live 无 refix → 质量断言略。
        // （只污染 DL 时 VDL 先行截断，"恰 4096"丧失判别力——两条长度一起污染；簇位取堆中段 100，
        // 101/102 在设备内可读 ⇒ 判别落在簇源=链，不靠"簇在设备外早停"）
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "L.BIN", &data, &[100], false)
            .build();
        let mut patched = image.clone();
        let stream = SET + 32;
        patched[stream + 8..stream + 16].copy_from_slice(&12288u64.to_le_bytes()); // VDL 污染
        patched[stream + 24..stream + 32].copy_from_slice(&12288u64.to_le_bytes()); // DL 污染
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "L.BIN")
            .unwrap();
        assert_eq!(e.size_bytes, 12288);
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes.len(), 4096, "被链界住（单簇），非被 DL 放大");
        assert_eq!(bytes, data);
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
        // qual-t6 C1 / 探针 D 三档（每档各钉一种旧式崩法，重算还原校验以过 T4 门槛）：
        // (a) DL=u64::MAX → debug 乘法溢出 panic；(b) VDL=DL=(1<<52)+4096 → release 巨分配 abort；
        // (c) VDL=u64::MAX（直构）→ 越 DL 交付。三档均须 Ok 且长度受 min(VDL,DL)/可达簇数约束
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

        // release 半壁：VDL=DL=(1<<52)+4096 → 旧式容量提示 min(size, need*cb) ≈ 4.5PB 巨分配，
        // release 真 SIGABRT（乘法不溢出，故 debug 档不 panic）；新式三重钳位仍须 Ok(empty)
        let mut patched2 = image.clone();
        patched2[stream + 8..stream + 16].copy_from_slice(&((1u64 << 52) + 4096).to_le_bytes());
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
        // 位图不可读（0x81 首簇越界）→ 无从证伪 → 仍沿 stale 链交付全量（钉 `None => true` 分支）。
        // 构型刻意碎片化 [6,9,7]（物理序 ≠ 连续序）：add_file_chained 物理连续时退 contiguous 兜底
        // 字节相同，M4 变异（None => true → false）会逃逸（qual-t6 复审 Minor A）
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 227) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "G.BIN", &data, &[6, 9, 7], false)
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
