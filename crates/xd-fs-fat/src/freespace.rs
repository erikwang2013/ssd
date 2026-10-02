// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 未分配空间枚举（雕刻输入，§M1c）：FAT 表空闲项（0x000/0x0000/0x00000000）合并为
//! 字节区间。**FAT 删除即清链 → 删除文件的数据簇恰好落在此处（雕刻主战场）**。
//! 返回已排序、互不相交、簇边界对齐的 `[start,end)`，全部落于数据簇堆内
//! （FAT12/16 的固定根目录区永不入选）。BPB/FAT 解析失败 → Err；
//! `is_free` Err（表项读失败）视为已分配（保守：宁可漏扫不可误扫）；空闲区间数上限
//! MAX_RUNS **如实截断**（只丢尾不虚报，见常量 doc）。
//! 与 exfat 侧的不对称（**T6 显式决策点**）：本引擎在 FAT 全表不可读时退化为 `Ok(空)`
//! （与"全盘已分配"不可区分——保守方向已由专测钉死）；exfat 位图不可读 → `Err`。
//! 深扫若要区分"无空闲"与"空闲不可知"，由 T6 在 ScanError 层显式决策（-32005），
//! 本层不引入新错误类型。
//! ponytail: 逐簇一次 read_at（沿用 fat.rs 的取舍）；1M 簇盘实测慢再引入扇区缓存/预读。

use std::ops::Range;

use crate::FatError;
use crate::bpb::{self, Bpb};
use crate::fat::Fat;
use xd_device::BlockDevice;

/// 区间数上限：超出上限**返回前缀（只丢尾）**，不虚报未枚举区间；
/// `len() == MAX_RUNS` 是**可能截断**的唯一信号。
pub const MAX_RUNS: usize = 100_000;

pub fn unallocated_runs(dev: &dyn BlockDevice) -> Result<Vec<Range<u64>>, FatError> {
    let bpb = bpb::parse(dev)?;
    let fat = Fat::new(dev, &bpb);
    Ok(runs_from_fat(&bpb, &fat, MAX_RUNS))
}

/// 空闲簇 2..=count+1 线性合并。`is_free` Err 视为已分配（保守：宁可漏扫不可误扫）；
/// `max_runs` 到顶截断（调用方以 Σ区间长 为进度目标，不虚报未枚举区间）。参数化只为
/// 可测截断语义（公开入口恒传 `MAX_RUNS`）。
fn runs_from_fat(bpb: &Bpb, fat: &Fat<'_>, max_runs: usize) -> Vec<Range<u64>> {
    let max_cluster = bpb.data_cluster_count() as u64 + 1;
    let cb = bpb.cluster_bytes() as u64;
    let mut out: Vec<Range<u64>> = Vec::new();
    let mut run_start: Option<u64> = None;
    for c in 2..=max_cluster {
        let free = matches!(fat.is_free(c as u32), Ok(true));
        match (free, run_start) {
            (true, None) => run_start = Some(c),
            (false, Some(s)) => {
                out.push(bpb.cluster_to_byte(s as u32)..bpb.cluster_to_byte(c as u32));
                run_start = None;
            }
            _ => {}
        }
        if out.len() >= max_runs {
            break; // 截断：不虚报未扫区间
        }
    }
    if let Some(s) = run_start
        && out.len() < max_runs
    {
        // 末段右界 = 末簇起点 + 簇宽（cluster_to_byte 的契约上界是 count+1，不外推）
        out.push(bpb.cluster_to_byte(s as u32)..bpb.cluster_to_byte(max_cluster as u32) + cb);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::dev_for;
    use xd_device::{DeviceError, DeviceInfo};

    #[test]
    fn fat12_layout_invariants() {
        // 文件占簇 2,3（600B → 2 簇）；run 必须簇对齐、只落数据簇堆、Σ == 空闲簇数 × cb
        let image = xd_fixtures::FatImageBuilder::fat12()
            .add_file("/", "A.BIN", &[0u8; 600])
            .build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        let runs = unallocated_runs(&dev).unwrap();
        let cb = bpb.cluster_bytes() as u64;
        let heap = bpb.cluster_to_byte(2);
        assert!(!runs.is_empty());
        for r in &runs {
            assert!(r.start >= heap, "不得含数据簇堆之前的字节: {r:?}");
            assert_eq!((r.start - heap) % cb, 0, "左界须簇对齐: {r:?}");
            assert_eq!((r.end - heap) % cb, 0, "右界须簇对齐: {r:?}");
        }
        for c in [2u32, 3] {
            let (s, e) = (bpb.cluster_to_byte(c), bpb.cluster_to_byte(c) + cb);
            assert!(
                !runs.iter().any(|r| r.start < e && s < r.end),
                "文件簇 {c} 不得出现在空闲区间: {runs:?}"
            );
        }
        let free = (2..=bpb.data_cluster_count() + 1)
            .filter(|c| fat.is_free(*c).unwrap())
            .count() as u64;
        let run_bytes: u64 = runs.iter().map(|r| r.end - r.start).sum();
        assert_eq!(run_bytes, free * cb);
    }

    #[test]
    fn deleted_file_clusters_become_free() {
        // FAT 删除即清链 → 删除文件的数据簇必须落在某空闲区间内（雕刻可见性）
        let image = xd_fixtures::FatImageBuilder::fat32()
            .add_file("/", "GONE.BIN", &[7u8; 1200]) // 簇 3,4,5（根目录占簇 2）
            .delete("/", "GONE.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let runs = unallocated_runs(&dev).unwrap();
        let cb = bpb.cluster_bytes() as u64;
        for c in [3u32, 4, 5] {
            let (s, e) = (bpb.cluster_to_byte(c), bpb.cluster_to_byte(c) + cb);
            assert!(
                runs.iter().any(|r| r.start <= s && e <= r.end),
                "删除文件簇 {c} 必须被某 run 覆盖: {runs:?}"
            );
        }
        let root = bpb.cluster_to_byte(2);
        assert!(
            !runs.iter().any(|r| r.start <= root && root < r.end),
            "根目录簇仍分配，不得入选: {runs:?}"
        );
    }

    /// 空 fat16 + FAT 手工置 EOC（簇 3/5/7/9 已分配）→ 空闲区间 = 2、4、6、8、10..=末簇（5 区间）。
    fn image_with_five_free_runs() -> Vec<u8> {
        let mut patched = xd_fixtures::FatImageBuilder::fat16().build();
        for c in [3usize, 5, 7, 9] {
            let o = 512 + c * 2; // fat_start_sector = 1，FAT16 表项宽 2 字节
            patched[o..o + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
        }
        patched
    }

    #[test]
    fn truncation_drops_tail_only() {
        let image = image_with_five_free_runs();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        let full = unallocated_runs(&dev).unwrap();
        assert_eq!(full.len(), 5, "前提：无限制确为 5 区间 {full:?}");
        let capped = runs_from_fat(&bpb, &fat, 2);
        assert_eq!(capped.len(), 2, "到顶只丢尾，不得多报: {capped:?}");
        assert_eq!(capped[0], full[0], "前 2 个逐字段等于无限制结果");
        assert_eq!(capped[1], full[1]);
        let sum = |r: &[Range<u64>]| r.iter().map(|x| x.end - x.start).sum::<u64>();
        assert!(sum(&capped) < sum(&full), "Σ 必须严格小于无限制（不虚报）");
    }

    #[test]
    fn cap_at_run_count_keeps_all_runs() {
        // max_runs=3：第 3 个区间由已分配簇 7 在循环内完整终结后恰好到顶 break ——
        // `>=` 截断语义下第 4 个区间不得多报（对 `>=`→`>` 变异必红）
        let image = image_with_five_free_runs();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        let full = unallocated_runs(&dev).unwrap();
        let capped = runs_from_fat(&bpb, &fat, 3);
        assert_eq!(capped.len(), 3, "恰 3 个（第 4 个起丢尾）: {capped:?}");
        for (i, r) in capped.iter().enumerate() {
            assert_eq!(*r, full[i], "第 {i} 个区间逐字段等于无限制结果");
        }
        assert_eq!(
            capped[2].end,
            bpb.cluster_to_byte(7),
            "第 3 个区间为循环内终结的完整区间（非 final-flush 截断）"
        );
    }

    #[test]
    fn sorted_disjoint_invariants() {
        // 删 A 后 C 复用其最低簇 → 空洞在中间：升序、不相交、区间精确
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "A.BIN", &[1u8; 1024]) // 簇 2,3
            .add_file("/", "B.BIN", &[2u8; 1024]) // 簇 4,5
            .delete("/", "A.BIN")
            .add_file("/", "C.BIN", &[3u8; 512]) // 复用最低空闲簇 2
            .build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let runs = unallocated_runs(&dev).unwrap();
        let cb = bpb.cluster_bytes() as u64;
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert_eq!(runs[0], bpb.cluster_to_byte(3)..bpb.cluster_to_byte(4));
        assert_eq!(
            runs[1],
            bpb.cluster_to_byte(6)..bpb.cluster_to_byte(bpb.data_cluster_count() + 1) + cb
        );
        for w in runs.windows(2) {
            assert!(w[0].start < w[1].start, "升序");
            assert!(w[0].end <= w[1].start, "不得相交");
        }
    }

    #[test]
    fn fat_read_errors_are_treated_as_allocated() {
        // FAT 区读失败 → 表项视为已分配（宁可漏扫不可误扫）：空 runs、不 panic、不虚报
        struct NoFatReads<'a> {
            inner: &'a dyn BlockDevice,
        }
        impl BlockDevice for NoFatReads<'_> {
            fn info(&self) -> &DeviceInfo {
                self.inner.info()
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
                if offset >= 512 {
                    return Err(DeviceError::Io(std::io::Error::other(
                        "fat region unreadable",
                    )));
                }
                self.inner.read_at(offset, buf)
            }
        }
        let image = xd_fixtures::FatImageBuilder::fat12()
            .add_file("/", "A.BIN", &[0u8; 600])
            .build();
        let (_f, dev) = dev_for(&image);
        let runs = unallocated_runs(&NoFatReads { inner: &dev }).unwrap();
        assert!(runs.is_empty(), "FAT 不可读时不得声称任何簇空闲: {runs:?}");
        // 对照：同一镜像在可读设备上确实能枚举出空闲区间
        assert!(!unallocated_runs(&dev).unwrap().is_empty());
    }

    #[test]
    fn last_cluster_allocated_shrinks_tail_run() {
        // 两遍式：先取几何，再把末簇（data_cluster_count+1）的 FAT 项置 EOC → 不得入选，
        // 右界缩至末簇起点、Σ 仍 == 空闲簇数 × cb。钉死"末簇被循环上界漏访、再由
        // final-flush 补成整堆"类变异（对 `2..=max`→`2..max` 必红）。
        let mut patched = image_with_five_free_runs();
        let (_f0, dev0) = dev_for(&patched);
        let bpb0 = bpb::parse(&dev0).unwrap();
        let last = bpb0.data_cluster_count() + 1; // 4175
        let o = 512 + last as usize * 2; // fat_start_sector = 1，FAT16 表项宽 2 字节
        patched[o..o + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        let runs = unallocated_runs(&dev).unwrap();
        let cb = bpb.cluster_bytes() as u64;
        let free = (2..=last).filter(|c| fat.is_free(*c).unwrap()).count() as u64;
        assert_eq!(free, 4169, "前提：末簇置 EOC 后 4169 空闲簇");
        let run_bytes: u64 = runs.iter().map(|r| r.end - r.start).sum();
        assert_eq!(run_bytes, free * cb, "Σ 与空闲簇数不符: {runs:?}");
        let last_start = bpb.cluster_to_byte(last);
        assert_eq!(
            runs.last().unwrap().end,
            last_start,
            "右界须缩至末簇起点: {runs:?}"
        );
        assert!(
            !runs
                .iter()
                .any(|r| r.start < last_start + cb && last_start < r.end),
            "已分配的末簇 {last} 不得入选: {runs:?}"
        );
        // max_runs = 0：区间数上限为 0 时一个区间都不得上报（钉死 final-flush 的
        // `out.len() < max_runs` 守卫；无守卫会把 pending 的簇 2 区间推成整堆）
        assert!(
            runs_from_fat(&bpb, &fat, 0).is_empty(),
            "max_runs=0 不得上报任何区间"
        );
    }
}
