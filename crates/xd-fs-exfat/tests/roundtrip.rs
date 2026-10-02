// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! M1a2 出口标准：exFAT 合成镜像落盘 → 扫描 → 删除照片字节级 + 全名找回
//! （「相机卡删照片」的 exFAT 版；对比 FAT 版的红利：名字一字不差）。

use xd_device::image::ImageFileDevice;

#[test]
fn deleted_photo_fully_recovered_from_exfat_image() {
    let photo: Vec<u8> = (0..12000u32).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let image_bytes = xd_fixtures::ExfatImageBuilder::new()
        .add_subdir("/", "DCIM")
        .add_file("/DCIM", "旅行照片_0001.JPG", &photo)
        .add_file("/", "READ_ME.TXT", b"keep me")
        .delete("/DCIM", "旅行照片_0001.JPG")
        .build();

    let mut f = tempfile::NamedTempFile::new().unwrap();
    use std::io::Write;
    f.write_all(&image_bytes).unwrap();
    f.flush().unwrap();
    let dev = ImageFileDevice::open(f.path()).unwrap();

    let entries = xd_fs_exfat::scan::scan(&dev).unwrap();
    let photo_entry = entries
        .iter()
        .find(|e| e.deleted && !e.is_dir && e.ext == "jpg")
        .expect("deleted jpg not found");
    assert_eq!(
        photo_entry.name, "旅行照片_0001.JPG",
        "exFAT 红利：删除后名字一字不差"
    );
    assert_eq!(photo_entry.size_bytes, 12000);
    assert_eq!(photo_entry.path, "/DCIM");
    assert_eq!(
        photo_entry.quality,
        xd_fs_exfat::scan::RecoverQuality::Complete
    );
    assert_eq!(entries.iter().filter(|e| e.deleted).count(), 1);

    let recovered = xd_fs_exfat::scan::read_file(&dev, photo_entry).unwrap();
    assert_eq!(recovered, photo, "recovered bytes differ from original");

    // 只读铁律：扫描/读取不得改动镜像一个字节
    assert_eq!(
        std::fs::read(f.path()).unwrap(),
        image_bytes,
        "扫描/读取不得改动镜像"
    );

    assert!(
        entries
            .iter()
            .any(|e| e.name == "READ_ME.TXT" && !e.deleted)
    );
}

#[test]
fn deleted_chained_photo_recovered_byte_exact() {
    let photo: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
    // 碎片化：数据段 0/1/2 分别落在簇 6/9/7（NoFatChain=0 → 读侧须按 stale FAT 链序拼接）
    let image_bytes = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "BURST.BIN", &photo, &[6, 9, 7], false)
        .delete("/", "BURST.BIN")
        .build();

    let mut f = tempfile::NamedTempFile::new().unwrap();
    use std::io::Write;
    f.write_all(&image_bytes).unwrap();
    f.flush().unwrap();
    let dev = ImageFileDevice::open(f.path()).unwrap();

    let e = xd_fs_exfat::scan::scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.deleted)
        .expect("deleted file not found");
    assert_eq!(e.name, "BURST.BIN");
    assert_eq!(xd_fs_exfat::scan::read_file(&dev, &e).unwrap(), photo);
}
