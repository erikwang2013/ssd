#![cfg(target_os = "linux")]
// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 环回真块设备 e2e：由 scripts/e2e-loop.sh（root）建好环回并传入环境变量。
//! 未设置时静默通过——无特权环境（本机日常）自动跳过，CI 上由脚本驱动真跑。
use std::path::Path;
use xd_device::BlockDevice; // size_bytes/read_at 是 trait 方法（qual-t1 预告的编译修正）

#[test]
fn loop_device_reads_identical_bytes() {
    let (Ok(dev_path), Ok(img_path)) = (std::env::var("XD_LOOP_DEV"), std::env::var("XD_LOOP_IMG"))
    else {
        eprintln!("skip: XD_LOOP_DEV/XD_LOOP_IMG 未设置（无特权环境）");
        return;
    };
    let dev = xd_device::linux::LinuxBlockDevice::open(Path::new(&dev_path)).unwrap();
    let img = std::fs::read(&img_path).unwrap();
    assert_eq!(
        dev.size_bytes(),
        img.len() as u64,
        "sysfs 大小必须与镜像一致"
    );
    let mut buf = vec![0u8; img.len()];
    let n = dev.read_at(0, &mut buf).unwrap();
    assert_eq!(n, img.len(), "必须读满");
    assert_eq!(buf, img, "块设备读出必须与镜像逐字节一致");
    // 抽样中段/尾部读（验证偏移寻址）
    let mid = img.len() / 2;
    let mut tail = [0u8; 64];
    dev.read_at((img.len() - 64) as u64, &mut tail).unwrap();
    assert_eq!(&tail[..], &img[img.len() - 64..]);
    let mut m = [0u8; 64];
    dev.read_at(mid as u64, &mut m).unwrap();
    assert_eq!(&m[..], &img[mid..mid + 64]);
}
