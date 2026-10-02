// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 快速扫描：目录遍历（根 + 子目录）+ 删除文件找回 + 质量分级。
//! 原则：保守降级——单个坏目录链/坏读不中止全盘（根目录无法开始枚举除外：返回 Err，
//! 与"空盘 Ok([])"区分）；只有确证全空闲才评 Complete。

use crate::FatError;
use crate::bpb::{self, Bpb, FatType};
use crate::dirent::{self, ParsedEntry};
use crate::fat::Fat;
use xd_device::BlockDevice;

/// `read_file` 原样再导出：保 `xd_fs_fat::scan::read_file` 路径不变（实现已迁 `read.rs`，
/// 与 exfat 同款）。
pub use crate::read::read_file;

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

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
