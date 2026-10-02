// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! M1a 出口标准：合成镜像落盘 → BlockDevice 打开 → 扫描 → 读回删除文件
//! 的字节与原始数据完全一致（「U 盘删照片」的镜像版）。

use xd_device::image::ImageFileDevice;

#[test]
fn deleted_photo_recovered_byte_exact_from_image_file() {
    // 造一张"相机卡"：500 字节的"照片"（内容确定），删掉它
    let photo: Vec<u8> = (0..500u32).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let image_bytes = xd_fixtures::FatImageBuilder::fat16()
        .add_subdir("/", "DCIM")
        .add_file("/DCIM", "IMG_0001.JPG", &photo)
        .add_file("/", "READ_ME.TXT", b"keep me")
        .delete("/DCIM", "IMG_0001.JPG")
        .build();

    // 落盘为镜像文件，走真实 BlockDevice 通路
    let mut f = tempfile::NamedTempFile::new().unwrap();
    use std::io::Write;
    f.write_all(&image_bytes).unwrap();
    f.flush().unwrap();
    let dev = ImageFileDevice::open(f.path()).unwrap();

    // 扫描
    let entries = xd_fs_fat::scan::scan(&dev).unwrap();
    let photo_entry = entries
        .iter()
        .find(|e| e.deleted && !e.is_dir && e.ext == "jpg") // is_dir 忠实 attr 后须排除已删目录（qual-t6 I3）
        .expect("deleted jpg not found");
    assert_eq!(photo_entry.size_bytes, 500);

    // 字节级找回
    let recovered = xd_fs_fat::scan::read_file(&dev, photo_entry).unwrap();
    assert_eq!(recovered, photo, "recovered bytes differ from original");

    // 存活文件不受影响
    assert!(
        entries
            .iter()
            .any(|e| e.name == "READ_ME.TXT" && !e.deleted)
    );
}
