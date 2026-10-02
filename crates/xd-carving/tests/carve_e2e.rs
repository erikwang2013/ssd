// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 雕刻端到端（公共 API 级）：真实 FS 镜像 → `unallocated_runs` → `carve_runs` 全链。
//!
//! **恢复率回归门禁（设计 §8.2 的 v1 基线）**：本文件的 FS 级 e2e——恢复率门禁 2 枚
//! （exfat/fat 各一）+ 诚实截断门禁 1 枚——即门禁主体。恢复率口径：合成镜像埋 N 个真件
//! （含搅局件）→ 找回率必须 **100%（N/N）且假阳性 0**；任一测试失败即门禁失败，任何 PR
//! 把恢复率弄低都会在此变红。
//!
//! 搅局件一律"空壳形"（签名为真、结构裁决拒收、零上报）：JPEG 空壳 = SOI+段+EOI **永不过
//! SOS**（走链器 `eoi_without_sos_is_rejected` 裁决拒收）；PNG 空壳 = magic + 非 IHDR 首块
//! （`carve_png` 首块裁决）。反例警告：`FFD8FF` 后接构成"结构破裂截断"的垃圾**会被如实
//! 上报**为 ≥4 字节 stub（见 carver.rs `garbage_after_soi_reports_stub`）——那类字节**不得**
//! 当作"零上报"搅局件用。

use std::ops::Range;

use xd_carving::{CarveEvent, CarvedEntry, carve_runs};
use xd_device::image::ImageFileDevice;
use xd_fixtures::{ExfatImageBuilder, FatImageBuilder};

fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(image).unwrap();
    f.flush().unwrap();
    let dev = ImageFileDevice::open(f.path()).unwrap();
    (f, dev)
}

/// 空壳 JPEG 搅局件：SOI + APP0 段 + EOI、**无 SOS** → 走链器裁决 None（纯签名扫描器会误
/// 当 JPEG 上报——这是"假阳性 0"要拦的形态）。
fn decoy_jpeg_shell() -> Vec<u8> {
    let mut d = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]; // SOI + APP0（段长 16 含自身）
    d.extend_from_slice(&[0u8; 14]); // 段体（全 0，不含任何子签名）
    d.extend_from_slice(&[0xFF, 0xD9]); // EOI：见 EOI 而未见 SOS → 拒绝
    d
}

/// 空壳 PNG 搅局件：magic + 结构合法但首块非 IHDR → `carve_png` 首块裁决 None。
fn decoy_png_shell() -> Vec<u8> {
    let mut d = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    xd_fixtures::chunk(&mut d, b"IDAT", b"zz");
    d
}

/// 全收一次 carve（出现序）；返回 (条目, 尝试扫描字节)。
fn carve_all(dev: &ImageFileDevice, runs: &[Range<u64>]) -> (Vec<CarvedEntry>, u64) {
    let mut got = Vec::new();
    let stats = carve_runs(dev, runs, xd_carving::MAX_FILE_BYTES, &mut |ev| {
        if let CarveEvent::Entry(e) = ev {
            got.push(*e);
        }
        true
    });
    assert_eq!(got.len() as u64, stats.found, "found 与收条数必须一致");
    (got, stats.scanned_bytes)
}

/// (偏移, 长度, 扩展名, 完整性) 投影——逐字段断言用。
fn sig(e: &CarvedEntry) -> (u64, u64, &'static str, bool) {
    (e.byte_offset, e.size, e.signature.ext(), e.complete)
}

#[test]
fn carves_planted_files_in_free_runs_end_to_end() {
    let j = xd_fixtures::mini_jpeg(4096);
    let p = xd_fixtures::mini_png(b"e2e-payload");
    let decoy = [0xFFu8, 0xD8, 0xFF, 0xFF, 0xD9]; // JPEG 空壳（未过 SOS）→ 必须零上报
    let mut img = vec![0u8; 16384];
    xd_fixtures::plant_in_run(&mut img, 512, &j);
    xd_fixtures::plant_in_run(&mut img, 8192, &p);
    xd_fixtures::plant_in_run(&mut img, 12000, &decoy);
    let (_f, dev) = dev_for(&img);
    let runs: Vec<Range<u64>> = std::iter::once(0..img.len() as u64).collect(); // 单个 run = 整卷未分配
    let (got, scanned) = carve_all(&dev, &runs);
    assert_eq!(got.len(), 2, "空壳不得计入: {got:?}");
    assert_eq!(
        got.iter().map(sig).collect::<Vec<_>>(),
        vec![
            (512, j.len() as u64, "jpg", true),
            (8192, p.len() as u64, "png", true),
        ],
        "恢复集合逐字节对齐（偏移/长度/类型/完整性）"
    );
    assert_eq!(scanned, img.len() as u64, "进度收尾 100%");
}

#[test]
fn exfat_free_space_recovery_rate_is_100_percent_no_false_positives() {
    // §8.2 门禁（exFAT 臂）：exFAT 删除=位图清位、数据留盘 → 被删 JPG 的簇区落回空闲（雕刻
    // 主战场）。埋 2 真件（JPEG 就埋在删除簇区内）+ 2 枚空壳搅局件 → 必须 2/2 找回、0 假阳性。
    let mut b = ExfatImageBuilder::new();
    b.add_file("/", "HOLIDAY.JPG", &[0x41u8; 20000]) // 5 簇 = 6..=10
        .add_file("/", "LIVE.TXT", b"live") // 1 簇 = 11
        .delete("/", "HOLIDAY.JPG"); // 位图清位 → 簇 6..=10 空闲
    let mut img = b.build();

    let (_f0, dev0) = dev_for(&img);
    let boot = xd_fs_exfat::boot::parse(&dev0).unwrap();
    assert_eq!(boot.cluster_bytes(), 4096, "夹具几何漂移必在此响亮失败");
    let (del_lo, del_hi) = (boot.cluster_to_byte(6), boot.cluster_to_byte(11));
    let (tail_lo, tail_hi) = (
        boot.cluster_to_byte(12),
        boot.cluster_to_byte(253) + boot.cluster_bytes(),
    );

    let j = xd_fixtures::mini_jpeg(20000);
    let (jpeg_off, png_off) = (del_lo + 100, tail_lo + 4096);
    let (decoy_j_off, decoy_p_off) = (tail_lo + 8192, tail_lo + 12288);
    xd_fixtures::plant_in_run(&mut img, jpeg_off, &j);
    xd_fixtures::plant_in_run(&mut img, png_off, xd_fixtures::TINY_PNG);
    xd_fixtures::plant_in_run(&mut img, decoy_j_off, &decoy_jpeg_shell());
    xd_fixtures::plant_in_run(&mut img, decoy_p_off, &decoy_png_shell());

    let (_f, dev) = dev_for(&img);
    let runs = xd_fs_exfat::freespace::unallocated_runs(&dev).unwrap();
    assert_eq!(
        runs,
        vec![del_lo..del_hi, tail_lo..tail_hi],
        "空闲区间 = 被删 JPG 的簇区 + 卷尾段"
    );
    assert!(
        runs.iter().any(|r| r.contains(&jpeg_off)),
        "删除文件的簇区必须可扫（雕刻可见性）：JPEG 埋点 {jpeg_off} 未被任何 run 覆盖"
    );

    let (got, scanned) = carve_all(&dev, &runs);
    assert_eq!(
        got.len(),
        2,
        "§8.2 门禁：2 真件必须 2/2 找回且搅局件零上报: {got:?}"
    );
    assert_eq!(
        got.iter().map(sig).collect::<Vec<_>>(),
        vec![
            (jpeg_off, j.len() as u64, "jpg", true),
            (png_off, xd_fixtures::TINY_PNG.len() as u64, "png", true),
        ],
        "逐字段对齐（偏移/长度/类型/完整性）"
    );
    let target: u64 = runs.iter().map(|r| r.end - r.start).sum();
    assert_eq!(scanned, target, "进度=尝试扫描口径，收尾 100%");
}

#[test]
fn jpeg_fragmented_across_runs_is_honestly_truncated() {
    // 裁定（v1）：候选只在**所在 run 内**重组——跨 run 拼接必跨过已分配区把他人数据缝进来，
    // 顺序不可验证（违反"宁可漏报不可错报"）。形状：JPEG 头贴住 run A 右界、续段埋 run B
    // 起点，中间隔 KEEP.BIN 的已分配簇 → 恰 1 条 complete=false 且长度 == run A 内实际字节；
    // run B 的续段无签名开头 → 不重报（不跨洞缝合）。
    let mut b = ExfatImageBuilder::new();
    b.add_file_in_clusters("/", "DEL.BIN", &[0u8; 16384], &[6, 7, 8, 9], true) // 随后删除
        .add_file_in_clusters("/", "KEEP.BIN", &[0u8; 8192], &[10, 11], true) // 把两段空闲隔开
        .delete("/", "DEL.BIN");
    let mut img = b.build();

    let (_f0, dev0) = dev_for(&img);
    let boot = xd_fs_exfat::boot::parse(&dev0).unwrap();
    let (a_lo, a_hi) = (boot.cluster_to_byte(6), boot.cluster_to_byte(10));
    let (b_lo, b_hi) = (
        boot.cluster_to_byte(12),
        boot.cluster_to_byte(253) + boot.cluster_bytes(),
    );

    let j = xd_fixtures::mini_jpeg(20000);
    let first: usize = 1000; // 头段（SOI/APP0/SOF0/SOS + 957 字节熵段，无 EOI）
    let jpeg_off = a_hi - first as u64;
    xd_fixtures::plant_in_run(&mut img, jpeg_off, &j[..first]);
    xd_fixtures::plant_in_run(&mut img, b_lo, &j[first..]); // 续段：熵段 0xAA 开头，无签名

    let (_f, dev) = dev_for(&img);
    let runs = xd_fs_exfat::freespace::unallocated_runs(&dev).unwrap();
    assert_eq!(runs, vec![a_lo..a_hi, b_lo..b_hi]);

    let (got, _) = carve_all(&dev, &runs);
    assert_eq!(got.len(), 1, "续段无签名 → 不得重报: {got:?}");
    assert_eq!(
        sig(&got[0]),
        (jpeg_off, first as u64, "jpg", false),
        "恰 1 条 complete=false，交付长度 == run A 内实际字节（跨 run 不缝合）"
    );
}

#[test]
fn fat_deleted_file_clusters_recover_with_zero_false_positives() {
    // §8.2 门禁（FAT 臂）：FAT 删除即清链 → 被删文件的数据簇落回空闲。布局（fat16 夹具：
    // reserved 1 + FAT 17 + 根目录 32 → 数据区自扇区 50；簇 2 = 50*512 = 25600，簇长 512）：
    // DEL.JPG 占簇 2..=65、KEEP.BIN 占簇 66..=81；删除后 DEL.JPG 的 64 簇整体成空闲区间。
    let mut b = FatImageBuilder::fat16();
    b.add_file("/", "DEL.JPG", &[0x77u8; 32768])
        .add_file("/", "KEEP.BIN", &[0x11u8; 8192])
        .delete("/", "DEL.JPG");
    let mut img = b.build();

    let del_lo = 25600u64; // 簇 2 = 数据区起点
    let tail_lo = 66560u64; // 簇 82 = 25600 + 80*512（KEEP.BIN 之后）
    let j = xd_fixtures::mini_jpeg(20000);
    let (jpeg_off, png_off) = (del_lo + 100, tail_lo + 4096);
    let (decoy_j_off, decoy_p_off) = (tail_lo + 8192, tail_lo + 12288);
    xd_fixtures::plant_in_run(&mut img, jpeg_off, &j);
    xd_fixtures::plant_in_run(&mut img, png_off, xd_fixtures::TINY_PNG);
    xd_fixtures::plant_in_run(&mut img, decoy_j_off, &decoy_jpeg_shell());
    xd_fixtures::plant_in_run(&mut img, decoy_p_off, &decoy_png_shell());

    let (_f, dev) = dev_for(&img);
    let runs = xd_fs_fat::freespace::unallocated_runs(&dev).unwrap();
    assert_eq!(
        runs[0],
        del_lo..del_lo + 32768,
        "删除文件的 64 簇必成空闲区间（雕刻可见性）"
    );
    assert_eq!(runs.len(), 2, "KEEP.BIN 把空闲切成两段: {runs:?}");
    assert_eq!(runs[1], tail_lo..img.len() as u64, "尾段边界精确");

    let (got, scanned) = carve_all(&dev, &runs);
    assert_eq!(
        got.len(),
        2,
        "§8.2 门禁（FAT 臂）：2 真件必须 2/2 找回且搅局件零上报: {got:?}"
    );
    assert_eq!(
        got.iter().map(sig).collect::<Vec<_>>(),
        vec![
            (jpeg_off, j.len() as u64, "jpg", true),
            (png_off, xd_fixtures::TINY_PNG.len() as u64, "png", true),
        ],
        "逐字段对齐（偏移/长度/类型/完整性）"
    );
    assert_eq!(
        scanned,
        runs.iter().map(|r| r.end - r.start).sum::<u64>(),
        "进度=尝试扫描口径，收尾 100%"
    );
}
