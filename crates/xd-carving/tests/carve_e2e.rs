// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 雕刻端到端（公共 API 级）：T8 将在此扩建恢复率/假阳性门禁；本骨架先钉住全链——
//! 「未分配区间」上埋已知文件 → `carve_runs` 全收 → 数量/偏移/长度逐字节对齐，搅局件零上报。

use std::ops::Range;

use xd_carving::{CarveEvent, CarvedEntry, carve_runs};
use xd_device::image::ImageFileDevice;

fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(image).unwrap();
    f.flush().unwrap();
    let dev = ImageFileDevice::open(f.path()).unwrap();
    (f, dev)
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
    let mut got: Vec<CarvedEntry> = Vec::new();
    let stats = carve_runs(&dev, &runs, xd_carving::MAX_FILE_BYTES, &mut |ev| {
        if let CarveEvent::Entry(e) = ev {
            got.push(*e);
        }
        true
    });
    assert_eq!(stats.found, 2, "空壳不得计入: {got:?}");
    assert_eq!(
        got.iter()
            .map(|e| (e.byte_offset, e.size, e.signature.ext(), e.complete))
            .collect::<Vec<_>>(),
        vec![
            (512, j.len() as u64, "jpg", true),
            (8192, p.len() as u64, "png", true),
        ],
        "恢复集合逐字节对齐（偏移/长度/类型/完整性）"
    );
    assert_eq!(stats.scanned_bytes, img.len() as u64, "进度收尾 100%");
}
