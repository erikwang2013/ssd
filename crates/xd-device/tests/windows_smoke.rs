#![cfg(windows)]
// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! Windows 平台层冒烟（CI windows runner 真跑）：SetupAPI 枚举 ≥1 盘且 size>0；
//! 只读打开 `\\.\PhysicalDrive0` 并读引导区 512B；越尾钳位短读、远越界 `Ok(0)`，全程不 panic。
//! 真机语义本身「未验证（需真机）」——本文件只证 runner（管理员上下文）上这条路径可走通。
use xd_device::BlockDevice;
use xd_device::windows::{WindowsBlockDevice, enumerate};

#[test]
fn enumerates_at_least_one_disk_on_ci_runner() {
    let disks = enumerate().expect("SetupAPI 枚举失败");
    assert!(!disks.is_empty(), "CI runner 至少有一块系统盘");
    assert!(
        disks.iter().all(|d| d.info.size_bytes > 0),
        "每盘 size 必须 > 0（GET_LENGTH_INFO）"
    );
    for d in &disks {
        assert!(
            d.path.starts_with(r"\\.\PhysicalDrive"),
            "设备路径形态: {}",
            d.path
        );
        assert_eq!(d.info.id, format!("win:{}", d.path), "id 与路径同源");
    }
}

/// `PhysicalDrive0` 读 512B（MBR/GPT 头）不 panic；越尾短读/越界零读。
#[test]
fn opens_readonly_and_reads_boot_area() {
    let disks = enumerate().expect("SetupAPI 枚举失败");
    let first = disks.first().expect("CI runner 至少有一块盘");
    assert_eq!(
        first.path, r"\\.\PhysicalDrive0",
        "盘号排序后首盘应为 0（runner 恒有系统盘）"
    );
    let dev = WindowsBlockDevice::open(&first.path).expect("只读打开 PhysicalDrive0");
    assert_eq!(
        dev.size_bytes(),
        first.info.size_bytes,
        "open 与枚举的大小必须一致"
    );
    let mut boot = [0u8; 512];
    let n = dev.read_at(0, &mut boot).expect("读引导区 512B 不 panic");
    assert_eq!(n, 512, "首 512 字节（MBR/GPT 头）应读满");
    // 越尾：钳位 = 短读（Ok，不 Err、不 panic）
    let size = dev.info().size_bytes;
    let mut tail = [0u8; 1024];
    let n = dev
        .read_at(size.saturating_sub(512), &mut tail)
        .expect("越尾读不应 Err");
    assert_eq!(n, 512, "钳位到设备大小后恰读 512");
    // 远越界：恒 Ok(0)（不发起越界 IO）
    let mut none = [0u8; 16];
    assert_eq!(dev.read_at(size + 4096, &mut none).unwrap(), 0);
}
