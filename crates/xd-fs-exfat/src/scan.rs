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
    /// 数据簇按拓扑（连续/链）全部确证空闲，交付长度可满
    Complete,
    /// 有簇已被重新分配（覆盖风险）、簇信息缺失或链证不足
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
        // live 文件不查链拓扑（质量=entry 层 checksum）；链自环由 read 层截断兜底（qual-t4）
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
#[path = "scan_tests.rs"]
mod tests;
