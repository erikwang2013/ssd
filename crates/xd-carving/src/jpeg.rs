// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! JPEG 重组：marker 走链 + 熵段 EOI。返回 `None` = 结构裁决假阳性（调用方继续扫）；
//! `Some(Carved)` = 接管该区间给出的重组结果（`complete=false` 即诚实截断）。
//!
//! **算法：** SOI → 段循环（段 = `FF marker` + 2 字节大端长度（含自身，≥2）+ 段体跳过；
//! 记号白名单：APPn/DQT/SOFn/DRI/DHT/COM/TEM；0xFF 填充字节允许）→ SOS 段（声明的头部
//! 跳过后）→ **熵段扫描**：`FF 00` 转义跳过、`FF D0..D7` RST 跳过、`FF FF` 回退一字节按
//! 填充重判、其它 `FF xx` = 新记号（**回段循环**——渐进式 JPEG 多次 SOS 合法）→
//! `FF D9` = EOI（**完整**）。**未过 SOS 即 EOI = 无图像数据的空壳 → 假阳性拒绝（None）**；
//! 非法记号/段长越界/结构破裂 → 截断（`complete=false`，len=已耗字节）；到 `max_len` 或
//! run 界（游标 None）→ 诚实截断。**`max_len` 是返回值的硬上限**（唯一出口归一：任何路径
//! `len ≤ max_len`；EOI 恰好跨上限即降级为截断——`complete=false, len=max_len`），
//! 且 FF 填充循环逐轮检查上限（不被长填充区拖着读穿）。
//!
//! **扫描器契约承重（T6 carver.rs）：** 本函数可返回 `len=0` 的截断（SOI 后立刻破结构），
//! 扫描器必须**无论裁决结果至少推进签名长度**（JPEG=3 字节），否则在原地打转；
//! 本模块自身不做特殊处理（约定由扫描器承重）。

use crate::signatures::{Carved, Cursor};

const MAX_SEGMENT: u32 = 1024 * 1024; // 单段上限（防长度字段攻击；合法 JPEG 段远小于此）

/// 从 SOI 起重组。`cur` 已定位在 SOI（FFD8FF 已被签名层确认前 3 字节）。
/// `max_len`：重组**硬上限**——任何返回路径 `len ≤ max_len`，越限即诚实截断（`complete=false`）。
pub fn carve_jpeg(cur: &mut Cursor<'_>, max_len: u64) -> Option<Carved> {
    let c = walk(cur, max_len)?;
    // 单一出口归一（硬上限唯一执法点）：越限的完整裁决（EOI 跨上限）降级为截断
    Some(if c.len > max_len {
        Carved {
            len: max_len,
            complete: false,
        }
    } else {
        c
    })
}

/// 走链主体。返回点的 len 语义见 `carve_jpeg`（上限归一由出口统一执法）。
fn walk(cur: &mut Cursor<'_>, max_len: u64) -> Option<Carved> {
    let start = cur.pos;
    if cur.u8()? != 0xFF || cur.u8()? != 0xD8 {
        return None;
    }
    let mut saw_sos = false;
    loop {
        if cur.pos - start >= max_len {
            return Some(Carved {
                len: max_len.min(cur.pos - start),
                complete: false,
            });
        }
        // 记号：允许 0xFF 填充
        let Some(mut m) = cur.u8() else {
            return Some(Carved {
                len: cur.pos - start,
                complete: false,
            });
        };
        if m != 0xFF {
            // 结构破裂 → 截断
            return Some(Carved {
                len: cur.pos - start - 1,
                complete: false,
            });
        }
        while m == 0xFF {
            // 填充分隔；EOF/界 → 截断
            let Some(b) = cur.u8() else {
                return Some(Carved {
                    len: cur.pos - start,
                    complete: false,
                });
            };
            m = b;
            // 上限逐轮检查：长填充区不得拖着读穿（返回值的上限由出口归一兜底）
            if cur.pos - start >= max_len {
                return Some(Carved {
                    len: max_len.min(cur.pos - start),
                    complete: false,
                });
            }
        }
        match m {
            0xD9 => {
                // EOI：未过 SOS = 无图像数据的空壳 → 假阳性拒绝
                return if saw_sos {
                    Some(Carved {
                        len: cur.pos - start,
                        complete: true,
                    })
                } else {
                    None
                };
            }
            0xDA => {
                // SOS：长度（含 2 字节自身）→ 跳头部 → 熵段扫描
                let Some(len) = cur.u16_be() else {
                    return Some(Carved {
                        len: cur.pos - start,
                        complete: false,
                    });
                };
                if !(2..=MAX_SEGMENT).contains(&u32::from(len)) {
                    return Some(Carved {
                        len: cur.pos - start,
                        complete: false,
                    });
                }
                if cur.skip(len as u64 - 2).is_none() {
                    return Some(Carved {
                        len: cur.pos - start,
                        complete: false,
                    });
                }
                saw_sos = true;
                // 熵段扫描
                loop {
                    if cur.pos - start >= max_len {
                        return Some(Carved {
                            len: max_len.min(cur.pos - start),
                            complete: false,
                        });
                    }
                    let Some(b) = cur.u8() else {
                        return Some(Carved {
                            len: cur.pos - start,
                            complete: false,
                        });
                    };
                    if b != 0xFF {
                        continue;
                    }
                    let Some(n) = cur.u8() else {
                        return Some(Carved {
                            len: cur.pos - start,
                            complete: false,
                        });
                    };
                    match n {
                        0x00 | 0xD0..=0xD7 => continue, // 转义/RST 跳
                        0xFF => cur.pos -= 1,           // 连续 FF：回退一个当填充重判
                        0xD9 => {
                            return Some(Carved {
                                len: cur.pos - start,
                                complete: true,
                            });
                        }
                        _ => {
                            cur.pos -= 2; // 新记号（渐进式多扫描/段间）：退回 FF xx 交回段循环
                            break;
                        }
                    }
                }
            }
            // 段记号白名单：白名单外（如 0xD8 二次 SOI）→ 截断；0x00 非法
            0xC0..=0xCF | 0xDB | 0xDD | 0xE0..=0xEF | 0xFE | 0x01 => {
                let Some(len) = cur.u16_be() else {
                    return Some(Carved {
                        len: cur.pos - start,
                        complete: false,
                    });
                };
                if !(2..=MAX_SEGMENT).contains(&u32::from(len)) {
                    // 长度非法：可能是假阳性（随机数据）——但已走这么远，按截断处理并交由上层去重
                    return Some(Carved {
                        len: cur.pos - start,
                        complete: false,
                    });
                }
                if cur.skip(len as u64 - 2).is_none() {
                    return Some(Carved {
                        len: cur.pos - start,
                        complete: false,
                    });
                }
            }
            _ => {
                return Some(Carved {
                    len: cur.pos - start,
                    complete: false,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xd_device::image::ImageFileDevice;

    /// 镜像字节 → 落盘临时文件 + 只读镜像设备（与 signatures.rs 测试同构）。
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
        carve_jpeg(&mut cur, max)
    }

    #[test]
    fn carves_whole_jpeg_complete() {
        let j = xd_fixtures::mini_jpeg(1000);
        let mut img = vec![0u8; 4096];
        img[100..100 + j.len()].copy_from_slice(&j);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 100, img.len() as u64, 64 << 20).unwrap();
        assert_eq!(r.len, j.len() as u64, "重组长度逐字节等于原文件");
        assert!(r.complete);
    }

    #[test]
    fn stuffed_bytes_and_restart_markers_do_not_end_scan() {
        // 熵段含 FF00 转义、FFD0/FFD7 RST、FF FF 填充回退：均不得误判结束；
        // 尾部 FF FF D9：填充 + EOI（删回退则把 FF 当熵字节吞掉、错过 EOI）
        let base = xd_fixtures::mini_jpeg(0);
        let mut j = base[..base.len() - 2].to_vec(); // 去掉 EOI
        j.extend_from_slice(&[
            0xAA, 0xFF, 0x00, 0xAA, // 转义 FF00
            0xFF, 0xD0, 0xAA, // RST0
            0xFF, 0xFF, 0x00, 0xAA, // 填充 FF + 转义 FF00
            0xFF, 0xD7, 0xAA, // RST7
        ]);
        j.extend_from_slice(&[0xFF, 0xFF, 0xD9]); // 填充 FF + EOI
        let mut img = vec![0u8; 256];
        img[..j.len()].copy_from_slice(&j);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 0, img.len() as u64, 64 << 20).unwrap();
        assert!(r.complete && r.len == j.len() as u64, "{r:?}");
    }

    #[test]
    fn eoi_without_sos_is_rejected() {
        // FFD8 + APP0/SOF0（合法段白名单）+ FFD9：无 SOS、无图像数据的空壳 → 假阳性 None
        let mut j = vec![0xFF, 0xD8]; // SOI
        j.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]); // APP0 len=4
        j.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00,
        ]); // SOF0 len=11
        j.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x02]); // COM len=2（极小段：零段体）
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI（无 SOS）
        let mut img = vec![0u8; 64];
        img[..j.len()].copy_from_slice(&j);
        let (_f, dev) = dev_for(&img);
        assert_eq!(
            carve_all(&dev, 0, img.len() as u64, 64 << 20),
            None,
            "未过 SOS 即 EOI 必须拒绝"
        );
    }

    #[test]
    fn img_without_eoi_truncates_at_run_end() {
        // 砍掉 EOI 的 JPEG 只留前 n 字节 → 到 run 界诚实截断（len=实耗）
        let j = xd_fixtures::mini_jpeg(5000);
        let cut = j.len() - 2; // 到 EOI 前（EOI 恰两字节）
        let mut img = vec![0u8; 1024 + 100];
        let n = (img.len() - 100).min(cut);
        img[100..100 + n].copy_from_slice(&j[..n]);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 100, img.len() as u64, 64 << 20).unwrap();
        assert!(!r.complete && r.len == n as u64, "到 run 界诚实截断：{r:?}");
    }

    #[test]
    fn max_len_cap_truncates() {
        let j = xd_fixtures::mini_jpeg(100_000);
        let mut img = vec![0u8; 100 + j.len()];
        img[100..].copy_from_slice(&j);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 100, img.len() as u64, 4096).unwrap();
        assert!(!r.complete);
        assert_eq!(r.len, 4096, "上限截断");
    }

    #[test]
    fn garbage_after_soi_never_completes() {
        // FFD8FF 后的三种垃圾形态：非记数字节、二次 SOI、段长 0——皆永不 complete
        for tail in [
            vec![0x41u8, 0x42, 0x43, 0x44], // 非记数字节 → 结构破裂
            vec![0xFF, 0xD8],               // 二次 SOI：白名单外 → 非法记号
            vec![0x00, 0x00],               // 段长 0：越界
        ] {
            let mut img = vec![0x55u8; 64];
            img[0] = 0xFF;
            img[1] = 0xD8;
            img[2] = 0xFF;
            img[3..3 + tail.len()].copy_from_slice(&tail);
            let (_f, dev) = dev_for(&img);
            let r = carve_all(&dev, 0, img.len() as u64, 64 << 20).unwrap();
            assert!(!r.complete, "垃圾不得 complete：{tail:02X?} → {r:?}");
        }
    }

    #[test]
    fn progressive_like_multi_sos_completes() {
        // 渐进式形态：SOS…熵…DHT 段…SOS…熵…EOI → 熵段遇新记号回段循环，最终 complete
        let base = xd_fixtures::mini_jpeg(0);
        let mut j = base[..base.len() - 2].to_vec(); // SOI..APP0..SOF0..SOS，去 EOI
        j.extend_from_slice(&[0xAA, 0xFF, 0x00, 0xAA]); // 第一段熵（含转义）
        j.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x06, 0x01, 0x02, 0x03, 0x04]); // DHT 段 len=6
        j.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]); // 第二 SOS
        j.extend_from_slice(&[0xBB; 5]);
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI
        let mut img = vec![0u8; 256];
        img[..j.len()].copy_from_slice(&j);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 0, img.len() as u64, 64 << 20).unwrap();
        assert!(r.complete, "{r:?}");
        assert_eq!(r.len, j.len() as u64, "多次 SOS 全链重组长度精确");
    }

    #[test]
    fn cap_split_across_eoi_truncates_hard() {
        // cap=44，mini_jpeg(0)（45B）的 EOI 第二字节恰落在 45：EOI 未能在上限内收束
        // → 硬上限归一降级为截断（无归一则返回 len=45/complete=true）
        let j = xd_fixtures::mini_jpeg(0);
        assert_eq!(j.len(), 45, "构型前提：EOI 在 44..45");
        let mut img = vec![0u8; 64];
        img[..j.len()].copy_from_slice(&j);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 0, img.len() as u64, 44).unwrap();
        assert!(!r.complete && r.len == 44, "EOI 跨上限必须硬截断：{r:?}");
    }

    #[test]
    fn cap_bounds_ff_fill_run() {
        // cap=4096 + 100k 个 FF 填充后非法记号：返回硬限 4096，且填充循环上限即停
        // （无逐轮检查则游标读穿 100k 填充区；无出口归一则返回 len=100004）
        let mut img = vec![0xFFu8; 3 + 100_000 + 1];
        img[1] = 0xD8; // img[2]=FF 即签名第三字节
        img[3 + 100_000] = 0xD8; // 白名单外 → 非法记号
        let (_f, dev) = dev_for(&img);
        let mut cur = Cursor::new(&dev, 0, img.len() as u64);
        let r = carve_jpeg(&mut cur, 4096).unwrap();
        assert!(!r.complete && r.len == 4096, "填充长跑不得突破上限：{r:?}");
        assert_eq!(cur.pos, 4096, "填充循环必须上限即停");
    }

    #[test]
    fn cap_exactly_full_length_stays_complete() {
        // cap 恰等于完整 JPEG 长度：EOI 在 cap 内收束 → 必须仍 complete
        // （出口归一用 `>` 而非 `>=`；`>=` 语义下此测必红）
        let j = xd_fixtures::mini_jpeg(1000);
        let mut img = vec![0u8; 4096];
        img[100..100 + j.len()].copy_from_slice(&j);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 100, img.len() as u64, j.len() as u64).unwrap();
        assert!(
            r.complete && r.len == j.len() as u64,
            "恰满上限不得误判截断：{r:?}"
        );
    }

    #[test]
    fn cap_clamps_segment_header_overshoot() {
        // cap=5，段头读到第 6 字节才判段长非法（len=0）→ 未归一化时返回 len=6；出口归一钳到 5
        let img = [0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x00, 0x00, 0x00];
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 0, img.len() as u64, 5).unwrap();
        assert!(r.len == 5 && !r.complete, "段中头越限必须钳到上限：{r:?}");
    }
}
