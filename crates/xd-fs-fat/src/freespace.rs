// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 未分配空间枚举（雕刻输入，§M1c）：FAT 表空闲项（0x000/0x0000/0x00000000）合并为
//! 字节区间。**FAT 删除即清链 → 删除文件的数据簇恰好落在此处（雕刻主战场）**。
//! 返回已排序、互不相交、簇边界对齐的 `[start,end)`，全部落于数据簇堆内
//! （FAT12/16 的固定根目录区永不入选）。BPB/FAT 解析失败 → Err；
//! `is_free` Err（表项读失败）视为已分配（保守：宁可漏扫不可误扫）。
//! ponytail: 逐簇一次 read_at（沿用 fat.rs 的取舍）；1M 簇盘实测慢再引入扇区缓存/预读。

use std::ops::Range;

use crate::FatError;
use crate::bpb::{self, Bpb};
use crate::fat::Fat;
use xd_device::BlockDevice;

pub const MAX_RUNS: usize = 100_000;

pub fn unallocated_runs(dev: &dyn BlockDevice) -> Result<Vec<Range<u64>>, FatError> {
    let bpb = bpb::parse(dev)?;
    let fat = Fat::new(dev, &bpb);
    Ok(runs_from_fat(&bpb, &fat))
}

/// 空闲簇 2..=count+1 线性合并。`is_free` Err 视为已分配（保守：宁可漏扫不可误扫）；
/// MAX_RUNS 到顶截断（调用方以 Σ区间长 为进度目标，不虚报未枚举区间）。
fn runs_from_fat(bpb: &Bpb, fat: &Fat<'_>) -> Vec<Range<u64>> {
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
        if out.len() >= MAX_RUNS {
            break; // 截断：不虚报未扫区间
        }
    }
    if let Some(s) = run_start
        && out.len() < MAX_RUNS
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
}
