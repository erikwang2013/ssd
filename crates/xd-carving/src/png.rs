// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! PNG 重组：chunk 走链 + CRC32 逐块验证（比纯签名强得多的假阳性防线）+ IEND 收束。
//! 返回 `None` = 假阳性裁决（首块非 13 字节 IHDR；或 magic 复读未成，交由扫描器兜底）；
//! `Some(Carved)` = 接管该区间给出的重组结果（`complete=false` 即诚实截断）。
//!
//! **算法：** 8 字节 magic 复读比对 → 首 chunk 裁决：**必须是 IHDR 且 len=13**（CRC 同
//! 在循环内统一验证）否则 None → chunk 循环：`len(u32 BE) + type(4) + data(len) + crc(4)`，
//! CRC = crc32(type ‖ data) **流式**逐块验证（不分块拼接，内存 O(64KiB)）；CRC 不符 →
//! 截断至坏块起点（不含，CRC 是强判据，此行之后的数据不可信）；IEND（CRC 正确）→
//! **完整**，**不校验 len==0**——真实 PNG 的 IEND 恒为空，len≠0 属坏编码但结构已收束；
//! 严格拒收会把尾部单字段损坏升级为整文件不完整。`len > MAX_CHUNK`（16MiB，防长度字段
//! 攻击）→ 截断；到 `max_len` 或 run 界（游标 None）→ 诚实截断。
//!
//! **`max_len` 是返回值的硬上限**（唯一出口归一：任何路径 `len ≤ max_len`；恰跨上限的
//! 完整裁决降级为 `complete=false, len=max_len`），且 **chunk 数据读取循环逐轮检查上限**
//! （单个 ≤16MiB 的 chunk 不得拖着读穿上限——同 T4 JPEG 填充循环的纪律）。
//!
//! **扫描器契约承重（T6 carver.rs）：** 本函数可返回 `len < 8` 的截断（`max_len < 8` 时
//! 上限路径即此），扫描器必须**无论裁决结果至少推进签名长度**（PNG=8 字节），否则在原地
//! 打转；首块（IHDR）CRC 即坏时 `cut=8`（仅签名），上层按签名长推进正好衔接。

use crate::crc32::Crc32;
use crate::signatures::{Carved, Cursor, PNG_MAGIC};

const MAX_CHUNK: u32 = 16 * 1024 * 1024;
const CRC_BUF: usize = 64 * 1024;

/// 从 PNG magic 起重组。`cur` 已定位在签名起点（8 字节 magic 未被签名层读取）。
/// `max_len`：重组**硬上限**——任何返回路径 `len ≤ max_len`，越限即诚实截断（`complete=false`）。
pub fn carve_png(cur: &mut Cursor<'_>, max_len: u64) -> Option<Carved> {
    let c = walk(cur, max_len)?;
    // 单一出口归一（硬上限唯一执法点）：越限的完整裁决（IEND 跨上限）降级为截断
    Some(if c.len > max_len {
        Carved {
            len: max_len,
            complete: false,
        }
    } else {
        c
    })
}

/// 走链主体。返回点的 len 语义见 `carve_png`（上限归一由出口统一执法）。
fn walk(cur: &mut Cursor<'_>, max_len: u64) -> Option<Carved> {
    let start = cur.pos;
    let mut magic = [0u8; 8];
    cur.take(&mut magic)?;
    if magic != PNG_MAGIC {
        return None;
    }
    let mut first = true;
    loop {
        if cur.pos - start >= max_len {
            return Some(Carved {
                len: max_len.min(cur.pos - start),
                complete: false,
            });
        }
        let Some(len) = cur.u32_be() else {
            return Some(Carved {
                len: cur.pos - start,
                complete: false,
            });
        };
        if len > MAX_CHUNK {
            return Some(Carved {
                len: cur.pos - start,
                complete: false,
            });
        }
        let mut ty = [0u8; 4];
        if cur.take(&mut ty).is_none() {
            return Some(Carved {
                len: cur.pos - start,
                complete: false,
            });
        }
        if first && (&ty != b"IHDR" || len != 13) {
            return None; // 头部裁决：首块非 13 字节 IHDR → 假阳性
        }
        first = false;
        let mut crc = Crc32::new();
        crc.update(&ty);
        let mut remaining = len as u64;
        let mut buf = vec![0u8; CRC_BUF.min(len.max(1) as usize)];
        while remaining > 0 {
            // 上限逐轮检查（T4 裁定同步）：长数据段不得拖着读穿上限；
            // 最终归一仍在唯一出口，此处是读量防线
            if cur.pos - start >= max_len {
                return Some(Carved {
                    len: max_len.min(cur.pos - start),
                    complete: false,
                });
            }
            let n = remaining.min(buf.len() as u64) as usize;
            if cur.take(&mut buf[..n]).is_none() {
                return Some(Carved {
                    len: cur.pos - start,
                    complete: false,
                });
            }
            crc.update(&buf[..n]);
            remaining -= n as u64;
        }
        let Some(expect) = cur.u32_be() else {
            return Some(Carved {
                len: cur.pos - start,
                complete: false,
            });
        };
        if crc.finalize() != expect {
            // 坏块：交付到坏块起点（不含）= 位置 - 已读坏块字节数（4 len + 4 type + len + 4 crc）
            let cut = cur.pos - start - (len as u64 + 12);
            return Some(Carved {
                len: cut,
                complete: false,
            });
        }
        if &ty == b"IEND" {
            return Some(Carved {
                len: cur.pos - start,
                complete: true,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xd_device::image::ImageFileDevice;

    /// 镜像字节 → 落盘临时文件 + 只读镜像设备（与 jpeg.rs/signatures.rs 测试同构）。
    fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(image).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        (f, dev)
    }

    fn carve_all(
        dev: &dyn xd_device::BlockDevice,
        start: u64,
        end: u64,
        max: u64,
    ) -> Option<Carved> {
        let mut cur = Cursor::new(dev, start, end);
        carve_png(&mut cur, max)
    }

    /// magic + 单块（type/data + 正确 CRC）——首块裁决测试专用（块构造走夹具）。
    fn png_with_first_chunk(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = PNG_MAGIC.to_vec();
        xd_fixtures::chunk(&mut v, ty, data);
        v
    }

    #[test]
    fn carves_tiny_png_complete() {
        let p = xd_fixtures::TINY_PNG;
        let mut img = vec![0u8; 256];
        img[100..100 + p.len()].copy_from_slice(p);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 100, img.len() as u64, 64 << 20).unwrap();
        assert_eq!(r.len, p.len() as u64, "重组长度逐字节等于原文件");
        assert!(r.complete, "{r:?}");
    }

    #[test]
    fn bad_crc_truncates_at_chunk_start() {
        // 破坏 IDAT 的 CRC 首字节 → 交付到坏块（IDAT）起点（不含）：8 magic + 25 IHDR = 33
        let payload = b"payload-bytes";
        let mut p = xd_fixtures::mini_png(payload);
        let idat_crc = 8 + 25 + 8 + payload.len(); // 4 len + 4 type + data 之后
        p[idat_crc] ^= 0xFF;
        let mut img = vec![0u8; 256];
        img[..p.len()].copy_from_slice(&p);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 0, img.len() as u64, 64 << 20).unwrap();
        assert!(!r.complete, "坏 CRC 不得 complete：{r:?}");
        assert_eq!(r.len, 33, "截断至坏块起点（不含）：{r:?}");
    }

    #[test]
    fn first_chunk_not_ihdr_rejected() {
        // 两半都钉：非 IHDR 类型 → None；IHDR 但 len != 13（CRC 合法）→ 也必须 None
        for (ty, data) in [(*b"ABCD", vec![1u8, 2, 3]), (*b"IHDR", vec![0u8; 12])] {
            let p = png_with_first_chunk(&ty, &data);
            let (_f, dev) = dev_for(&p);
            assert_eq!(
                carve_all(&dev, 0, p.len() as u64, 64 << 20),
                None,
                "首块 {ty:?}（len={}）必须拒绝",
                data.len()
            );
        }
    }

    #[test]
    fn iend_with_bad_crc_not_complete() {
        // IEND CRC 末字节破坏 → 不得 complete；截断至坏块（IEND）起点（前链完整交付）
        let mut p = xd_fixtures::mini_png(b"a-data");
        let n = p.len();
        p[n - 1] ^= 0xFF;
        let mut img = vec![0u8; 256];
        img[..n].copy_from_slice(&p);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 0, img.len() as u64, 64 << 20).unwrap();
        assert!(!r.complete, "IEND 坏 CRC 不得 complete：{r:?}");
        assert_eq!(r.len, (n - 12) as u64, "截断至坏块（IEND）起点：{r:?}");
    }

    #[test]
    fn iend_nonzero_len_accepted_complete() {
        // IEND len=3（非空、CRC 合法）：结构已收束 → complete；且停在**第一个** IEND，
        // 尾随垃圾不并入（头注：IEND 不校验 len==0——len≠0 属坏编码不属假阳性）
        let mut p = PNG_MAGIC.to_vec();
        xd_fixtures::chunk(&mut p, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        xd_fixtures::chunk(&mut p, b"IDAT", b"z");
        xd_fixtures::chunk(&mut p, b"IEND", &[0x11, 0x22, 0x33]); // len=3
        let iend_end = p.len();
        p.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // 尾垃圾：不得并入
        let (_f, dev) = dev_for(&p);
        let r = carve_all(&dev, 0, p.len() as u64, 64 << 20).unwrap();
        assert!(r.complete, "IEND len≠0 结构已收束 → complete：{r:?}");
        assert_eq!(r.len, iend_end as u64, "必须停在第一个 IEND 结束处：{r:?}");
    }

    #[test]
    fn max_len_cap_truncates() {
        // cap=40：IHDR 整块（33）通过后，IDAT 头读毕（41）越限 → 硬限 40
        let p = xd_fixtures::mini_png(&[0xAA; 64]);
        let mut img = vec![0u8; 256];
        img[..p.len()].copy_from_slice(&p);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 0, img.len() as u64, 40).unwrap();
        assert!(!r.complete, "上限截断不得 complete：{r:?}");
        assert_eq!(r.len, 40, "上限截断");
    }

    #[test]
    fn png_cut_short_truncates_honest() {
        // run 界（cursor end）三砍点皆诚实截断：!complete 且**欠报不过报**（len 不越过 run 界）。
        // 构型 mini_png(payload=64B) 文件 121B：IDAT 头 33..41、数据 41..105、
        // CRC 105..109、IEND 109..121。三砍点来历：
        //   41  = IDAT 数据起点（首轮 64B 读不满 → take 失败不推进游标，欠报到数据起点）
        //   105 = IDAT CRC 字段内（数据已全交付，CRC 的 u32 读不全）
        //   109 = IEND 块头（IDAT 整块 CRC 已验证，下一块 len 字段读不到）
        let p = xd_fixtures::mini_png(&[0x77; 64]);
        assert_eq!(p.len(), 121, "构型前提");
        let (_f, dev) = dev_for(&p);
        for end in [41u64, 105, 109] {
            let r = carve_all(&dev, 0, end, 64 << 20).unwrap();
            assert!(!r.complete, "run 界截断不得 complete（end={end}）：{r:?}");
            assert_eq!(r.len, end, "欠报不过报：len 恰为已核实字节数（end={end}）");
        }
    }

    #[test]
    fn max_chunk_gate_bounds_length_field() {
        // MAX_CHUNK 门是 `>`（恰 16MiB 放行，不误伤边界）且门在长度字段读毕即判（不吞 type）。
        // 构型：magic + 合法 IHDR(13) 后接第二个块，cap=41：
        //   len == MAX_CHUNK   → 门放行（读到 type，pos=41）→ 由 cap 收束 → len=41
        //   len == MAX_CHUNK+1 → 门在长度字段处截断（pos=8+25+4=37）→ len=37
        // 门改 `>=` 或删门，两态分别必红。
        let mut head = PNG_MAGIC.to_vec();
        xd_fixtures::chunk(&mut head, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        for (len, want) in [(MAX_CHUNK, 41u64), (MAX_CHUNK + 1, 37)] {
            let mut p = head.clone();
            p.extend_from_slice(&len.to_be_bytes());
            p.extend_from_slice(b"IDAT");
            p.extend_from_slice(&[0u8; 64]); // 数据不足也无妨：两态都在上限/门处先收束
            let (_f, dev) = dev_for(&p);
            let mut cur = Cursor::new(&dev, 0, p.len() as u64);
            let r = carve_png(&mut cur, 41).unwrap();
            assert!(
                r.len == want && !r.complete,
                "长度字段 {len} 应得 len={want}：{r:?}"
            );
        }
    }

    #[test]
    fn cap_exactly_full_length_stays_complete() {
        // cap 恰等于完整 PNG 长度（70）：IEND 在 cap 内收束 → 必须仍 complete
        // （出口归一用 `>` 而非 `>=`；`>=` 语义下此测必红）
        let p = xd_fixtures::TINY_PNG;
        let mut img = vec![0u8; 128];
        img[..p.len()].copy_from_slice(p);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 0, img.len() as u64, p.len() as u64).unwrap();
        assert!(
            r.complete && r.len == p.len() as u64,
            "恰满上限不得误判截断：{r:?}"
        );
    }

    #[test]
    fn cap_lands_mid_chunk_data_clamps_hard() {
        // cap=345 落在 200_000B IDAT 数据段中间（单段跨多轮 64KiB 读，逼出数据循环的
        // 逐轮上限检查）：返回硬限 345，且游标在第二轮检查即停。
        // 无出口归一 → len=65577；无逐轮检查 → 读穿整个 chunk（cur.pos=200045）。
        let p = xd_fixtures::mini_png(&vec![0x5A; 200_000]);
        let mut img = vec![0u8; p.len()];
        img.copy_from_slice(&p);
        let (_f, dev) = dev_for(&img);
        let mut cur = Cursor::new(&dev, 0, img.len() as u64);
        let r = carve_png(&mut cur, 345).unwrap();
        assert!(
            r.len == 345 && !r.complete,
            "cap 落数据段中间必须硬钳：{r:?}"
        );
        assert_eq!(
            cur.pos,
            41 + CRC_BUF as u64,
            "数据循环必须逐轮检查上限即停（41 = magic+IHDR+IDAT 头）"
        );
    }

    #[test]
    fn cap_lands_in_crc_field_clamps_hard() {
        // cap=68 落在 TINY_PNG（70B）IEND 的 CRC 字段中（66..70）：CRC 读完时游标已到 70，
        // IEND 裁决 len=70/complete 越限 → 单一出口归一降级为 len=68/complete=false。
        // 无出口归一 → 返回 len=70/complete=true（此测必红）。
        let p = xd_fixtures::TINY_PNG;
        assert_eq!(p.len(), 70);
        let mut img = vec![0u8; 128];
        img[..p.len()].copy_from_slice(p);
        let (_f, dev) = dev_for(&img);
        let mut cur = Cursor::new(&dev, 0, img.len() as u64);
        let r = carve_png(&mut cur, 68).unwrap();
        assert!(r.len == 68 && !r.complete, "cap 落 CRC 字段必须硬钳：{r:?}");
    }
}
