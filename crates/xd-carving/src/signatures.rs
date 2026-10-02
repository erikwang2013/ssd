// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 签名识别与游标：签名只是"候选"，是否成立由结构走链器（jpeg.rs/png.rs）裁决。

use xd_device::BlockDevice;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signature {
    Jpeg,
    Png,
}

pub const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// 扫描窗口需在块边界保留的重叠字节数（≥ 最长签名 + 余量）。
pub const OVERLAP: usize = 16;

/// 在窗口内找所有候选签名起点（朴素扫描；4MiB 块上 memchr 级开销可接受）。
pub fn find_candidates(buf: &[u8]) -> Vec<(usize, Signature)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] == 0xFF && buf[i + 1] == 0xD8 && buf[i + 2] == 0xFF {
            out.push((i, Signature::Jpeg));
            i += 3;
            continue;
        }
        if i + 8 <= buf.len() && buf[i..i + 8] == PNG_MAGIC {
            out.push((i, Signature::Png));
            i += 8;
            continue;
        }
        i += 1;
    }
    out
}

/// 结果：重组长度（字节）+ 是否结构完整（找到 EOI/IEND）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Carved {
    pub len: u64,
    pub complete: bool,
}

/// 绝对字节游标：只前进、止于 `end`（所在 run 的右界）；设备坏读/越界 → None（诚实截断）。
pub struct Cursor<'a> {
    dev: &'a dyn BlockDevice,
    pub pos: u64,
    pub end: u64,
}

impl<'a> Cursor<'a> {
    pub fn new(dev: &'a dyn BlockDevice, pos: u64, end: u64) -> Self {
        Self { dev, pos, end }
    }

    /// 读满 buf 并前进；任一步失败/到界 → None（游标停在失败处）。
    pub fn take(&mut self, buf: &mut [u8]) -> Option<()> {
        if buf.is_empty() {
            return Some(());
        }
        if self.pos + buf.len() as u64 > self.end {
            return None;
        }
        match self.dev.read_at(self.pos, buf) {
            Ok(n) if n == buf.len() => {
                self.pos += n as u64;
                Some(())
            }
            _ => None,
        }
    }

    /// 读 1/2/4 字节便捷。
    pub fn u8(&mut self) -> Option<u8> {
        let mut b = [0u8; 1];
        self.take(&mut b)?;
        Some(b[0])
    }
    pub fn u16_be(&mut self) -> Option<u16> {
        let mut b = [0u8; 2];
        self.take(&mut b)?;
        Some(u16::from_be_bytes(b))
    }
    pub fn u32_be(&mut self) -> Option<u32> {
        let mut b = [0u8; 4];
        self.take(&mut b)?;
        Some(u32::from_be_bytes(b))
    }

    /// 跳过 n 字节（不读，只走位；用于长度字段声明的段体）。
    pub fn skip(&mut self, n: u64) -> Option<()> {
        if self.pos + n > self.end {
            return None;
        }
        self.pos += n;
        Some(())
    }

    /// 读一段（≤ cap）进内存（CRC 用；cap 防大段爆内存——分块调用方自理）。
    pub fn take_vec(&mut self, n: usize) -> Option<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.take(&mut v)?;
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xd_device::image::ImageFileDevice;

    /// 镜像字节 → 落盘临时文件 + 只读镜像设备（与 xd-fs-fat/xd-fs-exfat 的 testutil::dev_for 同构）。
    fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(image).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        (f, dev)
    }

    #[test]
    fn take_accepts_exact_end_and_rejects_past_end() {
        let (_f, dev) = dev_for(&[1, 2, 3, 4]);
        let mut cur = Cursor::new(&dev, 2, 4);
        let mut b = [0u8; 2];
        assert_eq!(cur.take(&mut b), Some(()), "恰读到 end 必须成功");
        assert_eq!(b, [3, 4]);
        assert_eq!(cur.pos, 4);
        // 越过 end：拒绝且游标不动（诚实截断的支点）
        assert_eq!(cur.take(&mut [0u8; 1]), None);
        assert_eq!(cur.pos, 4);
        // 空 buf 恒成功、不推进
        assert_eq!(cur.take(&mut []), Some(()));
        assert_eq!(cur.pos, 4);
        // take_vec 同界：长度不足 → None 且不推进；够则读出
        let mut c2 = Cursor::new(&dev, 0, 2);
        assert_eq!(c2.take_vec(3), None);
        assert_eq!(c2.pos, 0);
        assert_eq!(c2.take_vec(2), Some(vec![1, 2]));
    }

    #[test]
    fn take_short_read_stops_cursor() {
        // end 声 8 但文件只有 4 字节：界内却短读 → None，pos 停在失败处
        let (_f, dev) = dev_for(&[1, 2, 3, 4]);
        let mut cur = Cursor::new(&dev, 0, 8);
        assert_eq!(cur.take(&mut [0u8; 6]), None, "短读必须 None");
        assert_eq!(cur.pos, 0, "失败不得推进游标");
        let mut b = [0u8; 4];
        assert_eq!(cur.take(&mut b), Some(()), "界内读不受影响");
        assert_eq!(b, [1, 2, 3, 4]);
    }

    #[test]
    fn skip_bounds_and_be_byte_order() {
        let (_f, dev) = dev_for(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);
        let mut cur = Cursor::new(&dev, 0, 6);
        assert_eq!(cur.u16_be(), Some(0x1234), "u16 大端（小端会得 0x3412）");
        assert_eq!(cur.u32_be(), Some(0x5678_9ABC), "u32 大端");
        assert_eq!(cur.pos, 6);
        assert_eq!(cur.skip(0), Some(()), "恰在 end 的 0 步合法");
        assert_eq!(cur.skip(1), None, "越界跳过 → None");
        assert_eq!(cur.pos, 6, "失败的 skip 不得推进");
        assert_eq!(cur.u8(), None, "越界读 → None");
        assert_eq!(cur.pos, 6);
    }

    #[test]
    fn find_candidates_jpeg_png_and_no_false_positive() {
        // 夹具走根导出 `xd_fixtures::{mini_jpeg, mini_png}`（`mod carving` 是私有 mod，别绕）
        let j = xd_fixtures::mini_jpeg(4);
        let p = xd_fixtures::mini_png(b"xyz");
        assert_eq!(find_candidates(&j), vec![(0, Signature::Jpeg)]);
        assert_eq!(find_candidates(&p), vec![(0, Signature::Png)]);
        // 偏移按各自起点给出
        let mut both = vec![0u8; 3];
        both.extend_from_slice(&p);
        both.extend_from_slice(&j);
        assert_eq!(
            find_candidates(&both),
            vec![(3, Signature::Png), (3 + p.len(), Signature::Jpeg)]
        );
        // 反例：FF D8 后非记号（00）；PNG magic 差一字节
        assert!(find_candidates(&[0xFF, 0xD8, 0x00, 0x00]).is_empty());
        let mut bad = PNG_MAGIC;
        bad[7] = 0x0B;
        assert!(find_candidates(&bad).is_empty());
    }
}
