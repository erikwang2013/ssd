// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 文件读取：与 `scan` 同层（M1a 语义），自 `scan.rs` 机械迁出（T7，公开路径经
//! `scan::read_file` 再导出不变）。存活文件只信 FAT 链；删除项按连续簇回退；交付长度 =
//! 目录项 size，设备边界/坏读 → 诚实短前缀（绝不伪造）。

use crate::FatError;
use crate::bpb;
use crate::bpb::Bpb;
use crate::fat::Fat;
use crate::scan::FatEntry;
use xd_device::BlockDevice;

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

/// 分片读取：交付 `[offset, offset+length)` ∩ `[0, size)` 的字节（流式，绝不物化整文件）。
/// 拓扑与 `read_file` **同裁定**（存活只信链、删除按连续回退），停点条件一一对应（见
/// `read_range`）；短读/设备外 → 同 read_file 的诚实短交付。
pub fn read_file_range(
    dev: &dyn BlockDevice,
    entry: &FatEntry,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, FatError> {
    let size = entry.size_bytes;
    if entry.first_cluster < 2 || offset >= size || length == 0 {
        return Ok(Vec::new());
    }
    let take = length.min(size - offset);
    let bpb = bpb::parse(dev)?;
    let clusters: Vec<u32> = if entry.deleted {
        // 连续回退（与 read_file 同界、同 u64 累积防溢出）
        let max_cluster = bpb.data_cluster_count() as u64 + 1;
        let need = (size as u32).div_ceil(bpb.cluster_bytes()) as usize;
        let mut v = Vec::new();
        let mut c = entry.first_cluster as u64;
        while v.len() < need && c <= max_cluster {
            v.push(c as u32);
            c += 1;
        }
        v
    } else {
        let need = (size as u32).div_ceil(bpb.cluster_bytes()) as usize;
        let fat = Fat::new(dev, &bpb);
        let chain = fat.chain(entry.first_cluster).unwrap_or_default();
        chain[..need.min(chain.len())].to_vec()
    };
    let mut out =
        Vec::with_capacity((take as usize).min(clusters.len() * bpb.cluster_bytes() as usize));
    let mut buf = vec![0u8; bpb.cluster_bytes() as usize];
    read_range(dev, &bpb, clusters, offset, take, &mut out, &mut buf);
    Ok(out)
}

/// 簇序列上取 `[offset, offset+take)` 滑窗：停点条件与 `read_file` 主循环一一对应
/// （坏读/设备外/短读即停），但**只收窗口内字节**——窗口前的整簇仍按序读（停点语义与
/// read_file 严格同构），只是不复制其字节；跨窗簇按簇内偏移截取。
/// `produced` = 自簇序列起点的逻辑字节进度（= read_file 的 out.len()）。
fn read_range(
    dev: &dyn BlockDevice,
    bpb: &Bpb,
    clusters: Vec<u32>,
    offset: u64,
    take: u64,
    out: &mut Vec<u8>,
    buf: &mut [u8],
) {
    let window_end = offset + take; // take ≤ size - offset ⇒ 无溢出
    let mut produced: u64 = 0;
    for c in clusters {
        if produced >= window_end {
            break; // 窗口取满（先于 read_file 的 size 停点，绝不续读）
        }
        let n = match dev.read_at(bpb.cluster_to_byte(c), buf) {
            Ok(n) => n,
            Err(_) => break, // 坏道/越界：保留已读前缀（与 scan 同规则）
        };
        if n == 0 {
            break; // 该簇在设备外
        }
        let chunk_start = produced;
        produced += n as u64;
        if produced > offset {
            let from = offset.saturating_sub(chunk_start) as usize;
            let to = (window_end.saturating_sub(chunk_start) as usize).min(n);
            if from < to {
                out.extend_from_slice(&buf[from..to]);
            }
        }
        if n < buf.len() {
            break; // 短读 → 只收已读部分（不得拿上一簇残字节当数据）
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{RecoverQuality, scan};
    use crate::testutil::dev_for;

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

    /// range/全读对拍：range 的每个窗口必须逐字节等于 `read_file` 的对应切片
    /// （差分测试——range 版不得有独立裁定；含短交付截断点）。
    fn assert_range_matches_full(dev: &dyn BlockDevice, label: &str, deleted: bool) {
        // (offset, length)：簇界横跨/恰界/越尾/整读/零长/超大 length
        const CASES: [(u64, u64); 8] = [
            (0, 10),
            (500, 30),
            (512, 1),
            (1024, 3000),
            (1499, 2),
            (1500, 1),
            (0, 1 << 40),
            (0, 0),
        ];
        // 删除项首字符被 0xE5 抹掉：按 deleted 标志取（本夹具每镜像恰一条）
        let e = scan(dev)
            .unwrap()
            .into_iter()
            .find(|e| e.deleted == deleted)
            .unwrap();
        let full = read_file(dev, &e).unwrap();
        for (off, len) in CASES {
            let got = read_file_range(dev, &e, off, len).unwrap();
            let s = off.min(full.len() as u64) as usize;
            let t = (off + len).min(full.len() as u64) as usize;
            assert_eq!(
                got,
                &full[s..t],
                "{label} offset={off} len={len}（全读 {} 字节）",
                full.len()
            );
        }
    }

    #[test]
    fn ranged_read_matches_full_read_slices() {
        // 六种拓扑 × 八组窗口：live 链 / live 碎片链（FAT[2]=4）/ live 断链（短前缀）/
        // 删除连续回退 / 删除+FAT 被他文件复用（仍连续回退）/ 设备截断
        let data: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
        let images: [(&str, bool, Vec<u8>); 5] = [
            (
                "live 链",
                false,
                xd_fixtures::FatImageBuilder::fat16()
                    .add_file("/", "DATA.BIN", &data)
                    .build(),
            ),
            ("live 碎片链", false, {
                // 乱序碎片链 2→4→3（FAT 改序，不改字节）：真实重组需按链序 2,4,3 拼接
                let mut img = xd_fixtures::FatImageBuilder::fat16()
                    .add_file("/", "DATA.BIN", &data)
                    .build();
                img[512 + 2 * 2..512 + 2 * 2 + 2].copy_from_slice(&4u16.to_le_bytes());
                img[512 + 4 * 2..512 + 4 * 2 + 2].copy_from_slice(&3u16.to_le_bytes());
                img[512 + 3 * 2..512 + 3 * 2 + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
                img
            }),
            ("live 断链", false, {
                let mut img = xd_fixtures::FatImageBuilder::fat16()
                    .add_file("/", "DATA.BIN", &data)
                    .build();
                img[516..518].copy_from_slice(&0xFFFFu16.to_le_bytes()); // FAT[2]=EOC：链只剩首簇
                img
            }),
            (
                "删除连续回退",
                true,
                xd_fixtures::FatImageBuilder::fat16()
                    .add_file("/", "GONE.BIN", &data)
                    .delete("/", "GONE.BIN")
                    .build(),
            ),
            ("删除+FAT 复用", true, {
                let mut img = xd_fixtures::FatImageBuilder::fat16()
                    .add_file("/", "GONE.BIN", &data)
                    .delete("/", "GONE.BIN")
                    .build();
                // 删除后 FAT 被他文件复用（2→7→8）：读取仍走连续回退（M1a 语义）
                img[516..518].copy_from_slice(&7u16.to_le_bytes());
                img[526..528].copy_from_slice(&8u16.to_le_bytes());
                img[528..530].copy_from_slice(&0xFFFFu16.to_le_bytes());
                img
            }),
        ];
        for (label, deleted, image) in images {
            let (_f, dev) = dev_for(&image);
            assert_range_matches_full(&dev, label, deleted);
        }
        // 设备截断（簇 2 全 + 簇 3 前 200B）→ 全读 712，range 同点截断
        let truncated = {
            let image = xd_fixtures::FatImageBuilder::fat16()
                .add_file("/", "DATA.BIN", &data)
                .build();
            image[..26_312].to_vec()
        };
        let (_f, dev) = dev_for(&truncated);
        assert_range_matches_full(&dev, "设备截断", false);
    }

    #[test]
    fn ranged_read_offset_beyond_end_is_empty() {
        let data: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "DATA.BIN", &data)
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "DATA.BIN")
            .unwrap();
        assert!(
            read_file_range(&dev, &e, 1500, 100).unwrap().is_empty(),
            "越尾"
        );
        assert!(read_file_range(&dev, &e, 0, 0).unwrap().is_empty(), "零长");
        assert_eq!(
            read_file_range(&dev, &e, 1499, u64::MAX).unwrap(),
            data[1499..1500],
            "length 无上界不 panic、末字节短交付"
        );
        assert!(
            read_file_range(&dev, &e, u64::MAX, 1).unwrap().is_empty(),
            "offset 极大"
        );
    }

    #[test]
    fn ranged_read_deleted_uses_contiguous_fallback_not_reused_chain() {
        // 删除+FAT 被复用：range 必须与全读同走连续回退（绝不沿他人链）——offset 越过
        // 实际数据尾仍交付原数据切片，而不是复用链指向的字节
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &data)
            .delete("/", "GONE.BIN")
            .build();
        let mut patched = image.clone();
        patched[516..518].copy_from_slice(&7u16.to_le_bytes());
        patched[526..528].copy_from_slice(&8u16.to_le_bytes());
        patched[528..530].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(read_file_range(&dev, &e, 0, 1000).unwrap(), data);
        assert_eq!(
            read_file_range(&dev, &e, 490, 510).unwrap(),
            data[490..1000]
        );
        assert!(
            read_file_range(&dev, &e, 1000, 10).unwrap().is_empty(),
            "越尾空"
        );
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
}
