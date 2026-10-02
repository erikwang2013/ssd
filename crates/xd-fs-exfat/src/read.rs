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
#[path = "read_tests.rs"]
mod tests;
