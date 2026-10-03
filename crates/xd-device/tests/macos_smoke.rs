#![cfg(target_os = "macos")]
// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! macOS 平台层冒烟（CI macOS runner 真跑）：`/dev/diskN` **枚举 ≥1 为硬断言**；打开根盘并读
//! 512B 为**弱断言**（计划 Task 3 Step 2 口径：返回 `PermissionDenied` 亦接受并 `eprintln` 标注
//! ——`/dev/diskN` 为 `brw-r----- root:operator`，runner 普通用户预期 EACCES/EPERM；**不照抄**
//! T2 Windows 冒烟的硬断言口径）。真机语义本身「未验证（需真机）」——本文件只证 runner 上
//! 这条路径可走通（枚举不依赖权限；打开/读视权限而定）。

use std::path::Path;

use xd_device::BlockDevice;
use xd_device::macos::{MacosBlockDevice, enumerate};

fn is_permission_denied(e: &xd_device::DeviceError) -> bool {
    matches!(e, xd_device::DeviceError::Io(io)
        if io.kind() == std::io::ErrorKind::PermissionDenied)
}

#[test]
fn enumerates_at_least_one_disk_on_ci_runner() {
    let disks = enumerate().expect("枚举 /dev 失败");
    assert!(!disks.is_empty(), "CI runner 至少有一块根盘（/dev/disk0）");
    for d in &disks {
        let node = d.node.display().to_string();
        assert!(node.starts_with("/dev/disk"), "整盘节点形态: {node}");
        assert_eq!(d.info.id, format!("unix:{node}"), "id 与节点同源");
        // 整盘过滤：disk 后缀必须全为数字（分区 disk0s1 / 裸盘 rdisk0 不得出现）
        let stem = d.node.file_name().unwrap().to_str().unwrap();
        assert!(
            stem.strip_prefix("disk")
                .is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit())),
            "只列整盘 disk<N>: {stem}"
        );
    }
}

/// `/dev/disk0` 只读打开并读 512B（MBR/GPT 头）；runner 无权限（PermissionDenied）亦接受。
#[test]
fn opens_root_disk_readonly_or_reports_permission() {
    let disks = enumerate().expect("枚举 /dev 失败");
    let first = disks.first().expect("CI runner 至少有一块盘");
    assert_eq!(first.node, Path::new("/dev/disk0"), "盘号排序后首盘应为 0");
    match MacosBlockDevice::open(&first.node) {
        Ok(dev) => {
            assert_eq!(
                dev.size_bytes(),
                first.info.size_bytes,
                "open 与枚举的容量必须一致"
            );
            let mut boot = [0u8; 512];
            let n = dev.read_at(0, &mut boot).expect("读 512B 不 panic");
            assert_eq!(n, 512, "首 512 字节（MBR/GPT 头）应读满");
            eprintln!(
                "macos_smoke: {} 打开成功，读 {n}B（size={}）",
                first.node.display(),
                dev.size_bytes()
            );
        }
        Err(e) if is_permission_denied(&e) => {
            eprintln!(
                "macos_smoke: {} → PermissionDenied（runner 非 root/无 FDA，按计划弱断言接受）: {e}",
                first.node.display()
            );
        }
        Err(e) => panic!("open 应成功或 PermissionDenied，实际: {e}"),
    }
}
