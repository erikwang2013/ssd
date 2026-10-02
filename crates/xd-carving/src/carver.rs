// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 顺序块扫描器：只做 I/O 与调度；暂停/取消/落库由 `ev` 回调承载（回调返回 false = 停止）。
//!
//! **runs 契约**：区间须**互不相交**（重叠段会重复计入 `scanned`，百分比虚高）；本层不校验，
//! 契约由 freespace 侧合并算法保证。
//!
//! **扫描器不变量：**
//! 1. 每个候选无论裁决结果，扫描位置**至少推进其签名长度**（len=0 不原地打转）——
//!    对现签名集**不可观测**（JPEG/PNG 结构互斥，无跨签名重叠起点），属未来跨签名防护；
//!    可观测的是强化臂：命中条目后 `pos = offset + size`（`adjacent_files_not_rescanned` 钉死）。
//! 2. 窗口重叠由「重读窗口尾部 7 字节」实现：`next_read = max(pos, buf_end - 7)`；
//!    跨块签名不丢（PNG magic 8 字节，取 -7 即最坏起点回读）——`k=1..=8` 循环 pin 钉死。
//! 3. 跨块签名未取全且 run 未尽 → 记 `next_read = abs`，下一轮从候选处重读。
//!    **防御性**：当前 `find_candidates` 契约（候选恒整体在窗内）下不可达，
//!    未来支持部分匹配时启用（代码保留）。
//! 4. 坏读/空读：该窗口 span 计入 `scanned`（进度 = 尝试扫描字节，诚实到 100%），前进不中断。
//!    `scanned` 口径 = **已尝试到的最远绝对位置**（run 内单调，跨窗口重叠不重复计数）——
//!    收尾恰为 Σrun 长，百分比不会 >100%（逐窗口累加 span 会因 7 字节回读重复计数而超报）。
//! 5. `Scanned(累计)` 事件在**每窗口**发出（≤4MiB 粒度：暂停/取消/检查点响应及时）；
//!    `Entry` 事件每条雕刻发出；回调返回 `false` = 停止（取消）。
//! 6. **退化窗口守卫**（`next_read <= window_start` → 跳 span）：窗口 ≤7 字节时 `advance_to`
//!    打不过窗口起点，不跳会在原地打转——**承重**（`short_run_terminates_and_scans` 钉死）。
//!
//! **退化条目裁定（T5 移交①）**：生效臂 `e.size > sig.len()`（**严格大于**）——PNG 首块
//! CRC 坏的最小非 cap 返回恰为签名长 8，不得当条目上报；退化件走 `_` 臂按签名长推进。
//! **PNG 断尾重扫裁定（T5 移交②）**：`pos = offset + size` 会落回断块内部（交付到最后一个
//! 完整读取轮），断块尾部重扫**可能报出嵌在残段里的签名**——雕刻语义下为预期行为，
//! 不做特殊处理（残段 < 签名长时按 `_` 臂推进）。

use std::ops::Range;

use xd_device::BlockDevice;

use crate::jpeg::carve_jpeg;
use crate::png::carve_png;
use crate::signatures::{Cursor, Signature, find_candidates};

pub const CHUNK_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// 签名最大长度（PNG magic 8）——窗口回读安全边界。
const MAX_SIG_LEN: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarvedEntry {
    pub byte_offset: u64,
    pub size: u64,
    pub complete: bool,
    pub signature: Signature,
}

impl Signature {
    pub fn ext(self) -> &'static str {
        match self {
            Signature::Jpeg => "jpg",
            Signature::Png => "png",
        }
    }
    fn len(self) -> u64 {
        match self {
            Signature::Jpeg => 3,
            Signature::Png => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CarveStats {
    pub scanned_bytes: u64,
    pub found: u64,
}

/// 扫描事件：窗口级进度 + 每条雕刻结果。回调返回 false → 立即停止（取消）。
pub enum CarveEvent<'a> {
    Scanned(u64),
    Entry(&'a CarvedEntry),
}

pub fn carve_runs(
    dev: &dyn BlockDevice,
    runs: &[Range<u64>],
    max_file_bytes: u64,
    ev: &mut dyn FnMut(CarveEvent) -> bool,
) -> CarveStats {
    let mut stats = CarveStats::default();
    for run in runs {
        if !scan_run(dev, run, max_file_bytes, ev, &mut stats) {
            return stats; // 取消
        }
    }
    stats
}

/// 单 run 扫描。返回 false = 取消。
fn scan_run(
    dev: &dyn BlockDevice,
    run: &Range<u64>,
    max_file_bytes: u64,
    ev: &mut dyn FnMut(CarveEvent) -> bool,
    stats: &mut CarveStats,
) -> bool {
    let mut pos = run.start; // 已处理到的绝对位置（候选起点下界）
    let mut next_read = run.start; // 下一窗口读取起点
    let base = stats.scanned_bytes; // 本 run 前的累计（不变量 4 的口径见模块头注）
    while next_read < run.end {
        let window_start = next_read;
        let want = ((run.end - next_read) as usize).min(CHUNK_BYTES);
        let mut buf = vec![0u8; want];
        let got = dev.read_at(next_read, &mut buf).unwrap_or_default();
        let span_end = next_read + want as u64;
        stats.scanned_bytes = base + (span_end - run.start); // 不变量 4：尝试扫描到的最远处
        if !ev(CarveEvent::Scanned(stats.scanned_bytes)) {
            return false;
        }
        if got == 0 {
            next_read = span_end;
            pos = pos.max(span_end);
            continue;
        }
        buf.truncate(got);
        let buf_end = next_read + got as u64;
        // 不变量 2：窗口尾部 7 字节回读——但窗口已到 run 界时无需回读（界外无字节可拼，
        // 跨界签名按 run 界诚实截断），否则末窗会以 7 字节小窗重复读一遍尾巴
        let mut advance_to = if buf_end < run.end {
            buf_end.saturating_sub(MAX_SIG_LEN - 1)
        } else {
            buf_end
        };
        for (i, sig) in find_candidates(&buf) {
            let abs = next_read + i as u64;
            if abs < pos {
                continue; // 重叠区里上一轮已处理过
            }
            if abs + sig.len() > buf_end && buf_end < run.end {
                advance_to = abs; // 不变量 3（防御性：现契约不可达，见模块头注）
                break;
            }
            let mut cur = Cursor::new(dev, abs, run.end);
            let carved = match sig {
                Signature::Jpeg => carve_jpeg(&mut cur, max_file_bytes),
                Signature::Png => carve_png(&mut cur, max_file_bytes),
            };
            let entry = carved.map(|c| CarvedEntry {
                byte_offset: abs,
                size: c.len,
                complete: c.complete,
                signature: sig,
            });
            match entry {
                Some(e) if e.size > sig.len() => {
                    pos = e.byte_offset + e.size;
                    if !ev(CarveEvent::Entry(&e)) {
                        return false;
                    }
                    stats.found += 1;
                    advance_to = advance_to.max(pos); // 不变量 1 的强化：跳过已重组区间（内嵌容器不重报）
                }
                _ => {
                    pos = abs + sig.len(); // 不变量 1（现签名集不可观测，见模块头注）
                    advance_to = advance_to.max(pos);
                }
            }
        }
        next_read = next_read.max(advance_to);
        if next_read <= pos && pos < run.end {
            next_read = pos; // 防死角推进
        }
        if next_read <= window_start {
            // 退化窗口（≤7 字节，advance_to 打不过窗口起点）：本 span 已尽读，
            // 跳过（与坏读同义的诚实跳过；否则在短 run 上原地打转）
            next_read = span_end;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use xd_device::image::ImageFileDevice;

    use crate::signatures::PNG_MAGIC;

    fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(image).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        (f, dev)
    }

    /// 收集条目 + 统计（回调恒 true）。
    fn carve_all(dev: &dyn BlockDevice, len: u64, max: u64) -> (CarveStats, Vec<CarvedEntry>) {
        let mut out = Vec::new();
        let stats = carve_runs(dev, std::slice::from_ref(&(0..len)), max, &mut |ev| {
            if let CarveEvent::Entry(e) = ev {
                out.push(*e);
            }
            true
        });
        (stats, out)
    }

    #[test]
    fn finds_both_formats_in_one_run() {
        let j = xd_fixtures::mini_jpeg(1000);
        let p = xd_fixtures::mini_png(b"payload");
        let mut img = vec![0u8; 16384];
        xd_fixtures::plant_in_run(&mut img, 100, &j);
        xd_fixtures::plant_in_run(&mut img, 8000, &p);
        let (_f, dev) = dev_for(&img);
        let (stats, e) = carve_all(&dev, img.len() as u64, 64 << 20);
        assert_eq!(e.len(), 2, "{e:?}");
        assert_eq!(
            (e[0].byte_offset, e[0].size, e[0].signature, e[0].complete),
            (100, j.len() as u64, Signature::Jpeg, true)
        );
        assert_eq!(
            (e[1].byte_offset, e[1].size, e[1].signature, e[1].complete),
            (8000, p.len() as u64, Signature::Png, true)
        );
        assert_eq!((e[0].signature.ext(), e[1].signature.ext()), ("jpg", "png"));
        assert_eq!(stats.found, 2);
        assert_eq!(
            stats.scanned_bytes,
            img.len() as u64,
            "单窗口：100% 尝试扫描"
        );
    }

    #[test]
    fn signature_straddling_chunk_boundary_is_found() {
        // k=1..=8：mini_png 起点放 CHUNK_BYTES-k。k≤7 时窗口 1 尾部只含 magic 前缀
        // （find_candidates 不认）→ 不变量 2 的回读（buf_end-7）必须把窗口 2 推到 magic
        // 起点前；k=8 为界线另一侧（magic 整体在窗 1 尾，逐档都要找到）。
        // 钉死常量 7：回读缩到 -6 时 k=7 档（magic 起点 CHUNK-7 < 窗 2 起点 CHUNK-6）必漏报。
        let p = xd_fixtures::mini_png(b"straddle");
        for k in 1..=8u64 {
            let mut img = vec![0u8; CHUNK_BYTES + 8192];
            xd_fixtures::plant_in_run(&mut img, CHUNK_BYTES as u64 - k, &p);
            let (_f, dev) = dev_for(&img);
            let (stats, e) = carve_all(&dev, img.len() as u64, 64 << 20);
            assert_eq!(e.len(), 1, "k={k}: {e:?}");
            assert_eq!(
                e[0].byte_offset,
                CHUNK_BYTES as u64 - k,
                "k={k}: offset 精确"
            );
            assert_eq!(e[0].size, p.len() as u64, "k={k}");
            assert!(e[0].complete, "k={k}");
            assert_eq!(stats.scanned_bytes, img.len() as u64, "k={k}");
        }
    }

    /// [bad.start, bad.end) 内的读取一律 Err（模拟坏区）。
    struct BadRegion<'a> {
        inner: &'a dyn BlockDevice,
        bad: Range<u64>,
    }
    impl BlockDevice for BadRegion<'_> {
        fn info(&self) -> &xd_device::DeviceInfo {
            self.inner.info()
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, xd_device::DeviceError> {
            let end = offset + buf.len() as u64;
            if offset < self.bad.end && self.bad.start < end {
                return Err(xd_device::DeviceError::Io(std::io::Error::other(
                    "bad region",
                )));
            }
            self.inner.read_at(offset, buf)
        }
    }

    #[test]
    fn bad_region_skipped_and_progress_reaches_target() {
        // 9MiB 镜像（>2 个窗口）：[4MiB,5MiB) 坏读 → 该窗口整 span 跳过；后续窗口候选照找；
        // scanned 恒 = run 长（尝试扫描字节，不因坏读缩水）
        let a = xd_fixtures::mini_jpeg(1000);
        let b = xd_fixtures::mini_jpeg(2000);
        let mut img = vec![0u8; 9 << 20];
        xd_fixtures::plant_in_run(&mut img, 100, &a);
        xd_fixtures::plant_in_run(&mut img, (8 << 20) + 4096, &b);
        let (_f, inner) = dev_for(&img);
        let dev = BadRegion {
            inner: &inner,
            bad: (4 << 20)..(5 << 20),
        };
        let (stats, e) = carve_all(&dev, img.len() as u64, 64 << 20);
        assert_eq!(e.len(), 2, "坏区两侧候选皆须找到: {e:?}");
        assert_eq!(e[0].byte_offset, 100);
        assert_eq!(e[1].byte_offset, (8 << 20) + 4096);
        assert_eq!(
            stats.scanned_bytes,
            img.len() as u64,
            "坏读计入进度：scanned == Σrun 长"
        );
    }

    #[test]
    fn bad_read_in_last_window_still_counts_progress() {
        // 坏读计入的判别力只在**末窗**：中段坏读的置值会被后续窗口覆盖（口径 = 最远尝试
        // 位置），故单靠上测无牙。run 尾 1MiB 坏读 → 仍 scanned == Σrun（100%）且终止。
        let a = xd_fixtures::mini_jpeg(1000);
        let mut img = vec![0u8; 9 << 20];
        xd_fixtures::plant_in_run(&mut img, 100, &a);
        let (_f, inner) = dev_for(&img);
        let dev = BadRegion {
            inner: &inner,
            bad: (8 << 20)..(img.len() as u64), // 末窗（第 3 窗）全坏
        };
        let (stats, e) = carve_all(&dev, img.len() as u64, 64 << 20);
        assert_eq!(e.len(), 1, "坏区前的候选仍须找到: {e:?}");
        assert_eq!(e[0].byte_offset, 100);
        assert_eq!(
            stats.scanned_bytes,
            img.len() as u64,
            "末窗坏读不得吞进度：scanned == Σrun（100%）"
        );
    }

    #[test]
    fn cancel_stops_immediately() {
        let j = xd_fixtures::mini_jpeg(100);
        let mut img = vec![0u8; 4096];
        xd_fixtures::plant_in_run(&mut img, 100, &j);
        let (_f, dev) = dev_for(&img);
        let mut entries = 0;
        let stats = carve_runs(
            &dev,
            std::slice::from_ref(&(0..img.len() as u64)),
            64 << 20,
            &mut |ev| {
                if matches!(ev, CarveEvent::Entry(_)) {
                    entries += 1;
                }
                false // 取消：首个事件即停
            },
        );
        assert_eq!(entries, 0, "取消后不得再发 Entry");
        assert_eq!(stats.found, 0);
        assert_eq!(
            stats.scanned_bytes, 4096,
            "只覆盖首个窗口（run 恰一个窗口）"
        );
    }

    #[test]
    fn decoy_rejected() {
        // 两枚假阳性：① JPEG 空壳（FFD8FF + 填充 FF + EOI，未过 SOS）→ carve_jpeg 裁决 None；
        // ② PNG magic + 非 IHDR 首块（CRC 合法）→ carve_png 裁决 None。皆不得成条目。
        // 注：结构破裂型垃圾（如 FFD8FF+41 42…）在 T4 语义下返回 Some(len=4>3) 的诚实截断
        // stub——那是另一形态（见 garbage_after_soi_reports_stub），本测钉 None 裁决路径。
        let decoy_jpeg = [0xFFu8, 0xD8, 0xFF, 0xFF, 0xD9];
        let mut decoy_png = PNG_MAGIC.to_vec();
        xd_fixtures::chunk(&mut decoy_png, b"ABCD", &[1, 2, 3]);
        let mut img = vec![0u8; 16384];
        xd_fixtures::plant_in_run(&mut img, 100, &decoy_jpeg);
        xd_fixtures::plant_in_run(&mut img, 200, &decoy_png);
        let (_f, dev) = dev_for(&img);
        let (stats, e) = carve_all(&dev, img.len() as u64, 64 << 20);
        assert!(e.is_empty(), "假阳性零容忍: {e:?}");
        assert_eq!(stats.found, 0);
        assert_eq!(stats.scanned_bytes, img.len() as u64, "拒绝不影响进度");
    }

    #[test]
    fn garbage_after_soi_reports_stub() {
        // 已知形态记录（非褒非贬）：FFD8FF + 结构破裂垃圾 → T4 返回 Some(len=4>3)，扫描器
        // 如实上报 4 字节 complete=false stub（退化过滤只挡 ≤签名长的裁决）。T8 门禁的
        // 搅局件若用此形态会把 stub 计成"发现"——须知情（见 T6 报告）。
        let decoy = [0xFFu8, 0xD8, 0xFF, 0x41, 0x42, 0x43];
        let mut img = vec![0u8; 4096];
        xd_fixtures::plant_in_run(&mut img, 100, &decoy);
        let (_f, dev) = dev_for(&img);
        let (_, e) = carve_all(&dev, img.len() as u64, 64 << 20);
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(
            (e[0].byte_offset, e[0].size, e[0].complete),
            (100, 4, false)
        );
    }

    #[test]
    fn degenerate_png_bad_first_crc_not_reported() {
        // 裁定（T5 移交①）：8 magic + IHDR（CRC 坏）→ carve_png 裁出 len=8 == 签名长 →
        // 退化条目不得上报；扫描器按签名长推进，紧跟其后的真 PNG 仍须找到
        let mut degenerate = PNG_MAGIC.to_vec();
        xd_fixtures::chunk(&mut degenerate, b"IHDR", &[0u8; 13]);
        let n = degenerate.len();
        degenerate[n - 4] ^= 0xFF; // IHDR CRC 首字节
        assert_eq!(degenerate.len(), 33);
        let good = xd_fixtures::mini_png(b"after-degenerate");
        let mut img = vec![0u8; 4096];
        xd_fixtures::plant_in_run(&mut img, 100, &degenerate);
        // 真件放在退化件全长（33B）之后——叠放会被真件 magic 覆写退化件的长度字段，
        // 改变被测裁决形态（首跑实证：变成 len=12 的另一假阳性）
        xd_fixtures::plant_in_run(&mut img, 200, &good);
        let (_f, dev) = dev_for(&img);
        let (stats, e) = carve_all(&dev, img.len() as u64, 64 << 20);
        assert_eq!(e.len(), 1, "退化 8 字节条目不得上报: {e:?}");
        assert_eq!(e[0].byte_offset, 200);
        assert_eq!(e[0].size, good.len() as u64);
        assert!(e[0].complete);
        assert_eq!(stats.found, 1);
    }

    #[test]
    fn adjacent_files_not_rescanned() {
        // 两个 JPEG 紧邻（间隔 1 字节）：第一条重组到 EOI 后从其后继续，第二条必须找到；
        // 且不得把第一条的区间当重叠区跳过（pos 推进精确）
        let a = xd_fixtures::mini_jpeg(500);
        let b = xd_fixtures::mini_jpeg(700);
        let mut img = vec![0u8; 8192];
        xd_fixtures::plant_in_run(&mut img, 100, &a);
        let second = 100 + a.len() as u64 + 1;
        xd_fixtures::plant_in_run(&mut img, second, &b);
        let (_f, dev) = dev_for(&img);
        let (_, e) = carve_all(&dev, img.len() as u64, 64 << 20);
        assert_eq!(e.len(), 2, "{e:?}");
        assert_eq!(e[0].byte_offset, 100);
        assert_eq!(e[1].byte_offset, second);
        assert!(e.iter().all(|x| x.complete));
    }

    #[test]
    fn short_run_terminates_and_scans() {
        // 退化 run（< 8 字节：advance_to 的 buf_end-7 打不过窗口起点）不得死循环；
        // span 照计入进度（与坏读同义的诚实跳过）
        let (_f, dev) = dev_for(&[0x55u8; 4]);
        let (stats, e) = carve_all(&dev, 4, 64 << 20);
        assert!(e.is_empty());
        assert_eq!(stats.scanned_bytes, 4);
    }
}
