// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 文件雕刻（carving v1：JPEG/PNG）：在未分配字节区间上做签名识别 + **头部结构验证** +
//! 结构走链重组，产出无文件名结果（byteOffset/size/complete）。
//!
//! 裁定（v1）：候选文件**只在其所在 run 内重组**——跨 run 拼接会跨过已分配区把他人数据
//! 缝进来，顺序不可验证，违反"宁可漏报不可错报"；坏读/区间尽头即诚实截断；同一候选内嵌的
//! 容器（EXIF 缩略图）不重复上报。
//! 说明：设计 §4.3 的"多线程并行块扫描"延后（雕刻 I/O 受限，顺序读已达 §4.6 底线；
//! 并行收益须实测后按需加，届时并行的是"多个 run 的读"而非签名匹配）。

mod carver;
pub mod crc32;
mod jpeg;
mod png;
pub mod signatures;

use xd_device::BlockDevice;

pub use carver::{
    CHUNK_BYTES, CarveEvent, CarveStats, CarvedEntry, MAX_FILE_BYTES, carve_runs, carve_runs_from,
};
pub use jpeg::{carve_jpeg, collect_jpeg};
pub use png::{carve_png, collect_png};
pub use signatures::{Carved, Cursor, Signature};

/// 雕刻件回读：自 `byte_offset` 起在**原 run 内**（右界 `run_end`——由调用方用该 FS 的
/// `unallocated_runs` 重新定位到包含 `byte_offset` 的区间后取其 end）重走结构，再按裁决
/// 长度重读字节。重走是确定性的（同设备、同规则）：交付绝不超过雕刻期裁定的区间；
/// `need` 为交付上限（调用方通常传 `offset + length`）。
/// `None` = 与雕刻期同判的假阳性（调用方按空交付处理，不报内部错误）。
pub fn read_back(
    dev: &dyn BlockDevice,
    run_end: u64,
    byte_offset: u64,
    kind: Signature,
    need: u64,
) -> Option<Vec<u8>> {
    let mut cur = Cursor::new(dev, byte_offset, run_end);
    match kind {
        Signature::Jpeg => collect_jpeg(&mut cur, need),
        Signature::Png => collect_png(&mut cur, need),
    }
    .map(|(bytes, _)| bytes)
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

    /// 真实扫描器雕出全部条目（run = 全镜像）。
    fn carve_entries(dev: &dyn BlockDevice, len: u64) -> Vec<CarvedEntry> {
        let mut out = Vec::new();
        carve_runs(
            dev,
            std::slice::from_ref(&(0..len)),
            MAX_FILE_BYTES,
            &mut |ev| {
                if let CarveEvent::Entry(e) = ev {
                    out.push(*e);
                }
                true
            },
        );
        out
    }

    #[test]
    fn read_back_slices_match_original() {
        // plant → carve（真实扫描器）→ 对每条雕出记录：全量回读 == 原字节；
        // 以 `offset+length` 为上限回读 → 恰交付同一切片（分段回读的契约形态）
        let j = xd_fixtures::mini_jpeg(3000);
        let p = xd_fixtures::mini_png(b"payload-payload");
        let mut img = vec![0u8; 65536];
        xd_fixtures::plant_in_run(&mut img, 1000, &j);
        xd_fixtures::plant_in_run(&mut img, 30000, &p);
        let (_f, dev) = dev_for(&img);
        let found = carve_entries(&dev, img.len() as u64);
        assert_eq!(found.len(), 2, "{found:?}");
        for e in &found {
            let orig: &[u8] = match e.signature {
                Signature::Jpeg => &j,
                Signature::Png => &p,
            };
            assert_eq!(e.size, orig.len() as u64, "雕刻长度 == 原文件长度");
            assert!(e.complete);
            let all = read_back(&dev, img.len() as u64, e.byte_offset, e.signature, e.size)
                .expect("真件回读不得为 None");
            assert_eq!(all, orig, "全量回读 == 原字节");
            let (off, len) = (e.size / 3, e.size / 2);
            let part = read_back(
                &dev,
                img.len() as u64,
                e.byte_offset,
                e.signature,
                off + len,
            )
            .unwrap();
            let t = (off + len).min(e.size) as usize;
            assert_eq!(part, orig[..t], "上限回读 = 前缀（{off}+{len}）");
            assert_eq!(
                &part[off as usize..],
                &orig[off as usize..t],
                "切片 == 原字节切片"
            );
        }
    }

    #[test]
    fn read_back_truncated_stops_at_walk_length() {
        // 砍掉 EOI：裁决截断（complete=false、len=实耗）→ 回读恰为被交付的前缀，
        // 绝不越过裁决区间（同 run 内后续字节不得混入）
        let j = xd_fixtures::mini_jpeg(5000);
        let cut = j.len() - 2; // EOI 前
        let mut img = vec![0u8; 100 + cut + 512];
        img[100..100 + cut].copy_from_slice(&j[..cut]);
        let run_end = (100 + cut) as u64; // run 恰止于断点（其后 512 字节不属本 run）
        let (_f, dev) = dev_for(&img);
        let mut cur = Cursor::new(&dev, 100, run_end);
        let c = carve_jpeg(&mut cur, MAX_FILE_BYTES).unwrap();
        assert!(!c.complete && c.len == cut as u64, "{c:?}");
        let all = read_back(&dev, run_end, 100, Signature::Jpeg, cut as u64).unwrap();
        assert_eq!(all, j[..cut], "回读 == 诚实截断前缀");
        let more = read_back(&dev, run_end, 100, Signature::Jpeg, 1 << 20).unwrap();
        assert_eq!(
            more,
            j[..cut],
            "need 超裁决长 → 仍只交付裁决区间（不越 run）"
        );
    }

    #[test]
    fn read_back_false_positive_is_none() {
        // 空壳 JPEG（未过 SOS 即 EOI）→ carve 裁决 None → 回读 None（同判，不伪造）
        let decoy = [0xFFu8, 0xD8, 0xFF, 0xFF, 0xD9];
        let mut img = vec![0u8; 256];
        img[16..16 + decoy.len()].copy_from_slice(&decoy);
        let (_f, dev) = dev_for(&img);
        assert_eq!(
            read_back(&dev, img.len() as u64, 16, Signature::Jpeg, 100),
            None
        );
    }

    #[test]
    fn signature_from_ext_is_strict_lowercase() {
        assert_eq!(Signature::from_ext("jpg"), Some(Signature::Jpeg));
        assert_eq!(Signature::from_ext("png"), Some(Signature::Png));
        assert_eq!(Signature::from_ext("JPG"), None);
        assert_eq!(Signature::from_ext("jpeg"), None);
        assert_eq!(Signature::from_ext(""), None);
    }
}
