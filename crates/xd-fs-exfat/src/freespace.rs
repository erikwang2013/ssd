// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 未分配空间枚举（雕刻输入，§M1c）：**位图是分配权威**（删除后 FAT 已 stale），
//! 空闲簇合并为字节区间。返回已排序、互不相交、簇边界对齐的 `[start,end)`，
//! 全部落于数据簇堆内。位图不可读 → Err（雕刻拒绝在未知分配上猜）；
//! `is_free` Err 视为已分配（保守：宁可漏扫不可误扫）；上限 MAX_RUNS 截断并如实计数。

use std::ops::Range;

use crate::ExfatError;
use crate::bitmap::Bitmap;
use crate::boot::{self, ExfatBoot};
use crate::dirent;
use crate::fattab::Fat32;
use crate::read::load_bitmap_from_specials;
use xd_device::BlockDevice;

pub const MAX_RUNS: usize = 100_000;

pub fn unallocated_runs(dev: &dyn BlockDevice) -> Result<Vec<Range<u64>>, ExfatError> {
    let boot = boot::parse(dev)?;
    let fat = Fat32::new(dev, &boot);
    let root_data = crate::scan::read_root_dir(dev, &boot, &fat)?;
    let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
    let bitmap = load_bitmap_from_specials(dev, &boot, &fat, &root.specials)
        .ok_or_else(|| ExfatError::InvalidBoot("位图不可读——雕刻拒绝在未知分配上猜".into()))?;
    Ok(runs_from_free(&boot, &bitmap))
}

/// 空闲簇 2..=count+1 线性合并。`is_free` Err 视为已分配（保守：宁可漏扫不可误扫——
/// `Bitmap::load` 已保证长度，该臂实际不可达，纯防御）；MAX_RUNS 到顶截断
/// （调用方以 Σ区间长 为进度目标，不虚报未枚举区间）。
fn runs_from_free(boot: &ExfatBoot, bitmap: &Bitmap) -> Vec<Range<u64>> {
    let max_cluster = boot.cluster_count as u64 + 1;
    let cb = boot.cluster_bytes();
    let mut out: Vec<Range<u64>> = Vec::new();
    let mut run_start: Option<u64> = None;
    for c in 2..=max_cluster {
        let free = matches!(bitmap.is_free(c as u32), Ok(true));
        match (free, run_start) {
            (true, None) => run_start = Some(c),
            (false, Some(s)) => {
                out.push(boot.cluster_to_byte(s as u32)..boot.cluster_to_byte(c as u32));
                run_start = None;
            }
            _ => {}
        }
        if out.len() >= MAX_RUNS {
            break; // 截断：不虚报未扫区间
        }
    }
    if let Some(s) = run_start
        && out.len() < MAX_RUNS
    {
        // 末段右界 = 末簇起点 + 簇宽（cluster_to_byte 的契约上界是 count+1，不外推）
        out.push(boot.cluster_to_byte(s as u32)..boot.cluster_to_byte(max_cluster as u32) + cb);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot::testutil::dev_for;

    #[test]
    fn fragmented_free_space_yields_exact_runs() {
        // 位图 2 / upcase 3,4 / 根 5 已占用；文件各占 1 簇（7 与 9）→ 空闲 = 6、8、10..=253
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "A.BIN", &[1u8; 100], &[7], true)
            .add_file_in_clusters("/", "B.BIN", &[2u8; 100], &[9], true)
            .build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let runs = unallocated_runs(&dev).unwrap();
        let cb = boot.cluster_bytes();
        assert_eq!(runs.len(), 3, "{runs:?}");
        assert_eq!(runs[0], boot.cluster_to_byte(6)..boot.cluster_to_byte(7));
        assert_eq!(runs[1], boot.cluster_to_byte(8)..boot.cluster_to_byte(9));
        assert_eq!(
            runs[2],
            boot.cluster_to_byte(10)..boot.cluster_to_byte(253) + cb
        );
        let total: u64 = runs.iter().map(|r| r.end - r.start).sum();
        assert_eq!(total, (1 + 1 + 244) * cb);
    }

    #[test]
    fn empty_volume_is_one_run_and_unreadable_bitmap_is_err() {
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let runs = unallocated_runs(&dev).unwrap();
        assert_eq!(runs.len(), 1, "空盘：6..=253 一整段");
        assert_eq!(runs[0].start, boot.cluster_to_byte(6));
        assert_eq!(
            runs[0].end,
            boot.cluster_to_byte(253) + boot.cluster_bytes(),
            "右界 = 卷末簇堆末端"
        );
        // 位图项（根目录槽 1，0x81）首簇改越界 → 位图不可读 → Err（绝不当空闲）
        let mut patched = image.clone();
        const ROOT_B: usize = 32 * 512 + 3 * 4096;
        patched[ROOT_B + 32 + 20..ROOT_B + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
        let (_f2, dev2) = dev_for(&patched);
        assert!(unallocated_runs(&dev2).is_err(), "位图不可读必须拒绝");
    }

    #[test]
    fn runs_are_sorted_disjoint_and_never_allocated() {
        // 不变量：升序、不相交、Σ 与位图空闲簇数一致、已分配簇（7/9/11）不被覆盖。
        // 12000B → ceil(12000/4096) = 3 簇（与 clusters 列表自洽）
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "A.BIN", &[1u8; 12000], &[7, 9, 11], false)
            .build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let root_data = crate::scan::read_root_dir(&dev, &boot, &fat).unwrap();
        let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
        let bm = load_bitmap_from_specials(&dev, &boot, &fat, &root.specials).unwrap();
        let runs = unallocated_runs(&dev).unwrap();
        for w in runs.windows(2) {
            assert!(w[0].start < w[1].start, "升序");
            assert!(w[0].end <= w[1].start, "不得相交");
        }
        let cb = boot.cluster_bytes();
        for c in [7u32, 9, 11] {
            let (s, e) = (boot.cluster_to_byte(c), boot.cluster_to_byte(c) + cb);
            assert!(
                !runs.iter().any(|r| r.start < e && s < r.end),
                "已分配簇 {c} 不得出现在空闲区间: {runs:?}"
            );
        }
        let free_clusters = (2..=boot.cluster_count as u64 + 1)
            .filter(|c| matches!(bm.is_free(*c as u32), Ok(true)))
            .count() as u64;
        let run_bytes: u64 = runs.iter().map(|r| r.end - r.start).sum();
        assert_eq!(run_bytes, free_clusters * cb);
    }
}
