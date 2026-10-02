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
