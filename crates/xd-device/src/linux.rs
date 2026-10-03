// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! Linux 物理块设备：只读块设备后端（`LinuxBlockDevice`）+ sysfs 枚举重导出。
//!
//! 设计约束（M1e 简报，本机实测）：
//! - `device.list` 零 `open()`：枚举只读 sysfs（普通权限即可工作）。
//! - 块设备 `stat.st_size` 恒 0——大小必须走 sysfs，恒 512 字节单位。
//! - 虚拟设备（loop/ram/zram/dm/md/nbd）canonicalize 后以 `/sys/devices/virtual/` 开头，跳过。
//! - `removable` 不可作过滤（USB-SATA 硬盘盒报 0）；运输类型仅作分组提示。
//! - 只读铁律落点 = 类型系统（`BlockDevice` 无写方法）+ `O_RDONLY`。
//!
//! 枚举/归类层（`BlockEnumerator`、`RawDisk`/`Transport`、transport 映射）在
//! `linux/enumerate.rs`（T7 机械拆分：本文件超 500 行线宽）；本文件重导出，
//! 公开路径 `xd_device::linux::*` 全部不变。

use crate::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::Path;

mod enumerate;
pub use self::enumerate::{
    BlockEnumerator, RawDisk, Transport, classify_transport, is_virtual_sysfs_path,
    parse_disk_entry,
};
use self::enumerate::{compose_disk_name, sysfs_transport};

// Linux x86_64 O_NONBLOCK（man 2 open）；块设备忽略该位，仅防 --device <fifo> 卡死在 open。
const O_NONBLOCK: i32 = 0o4000;

/// glibc dev_t 编码（gnu_dev_makedev / major / minor）。
pub fn makedev(major: u64, minor: u64) -> u64 {
    ((major & 0xfff) << 8) | (minor & 0xff) | ((minor & !0xff) << 12) | ((major & !0xfff) << 32)
}

pub fn major_minor(dev: u64) -> (u64, u64) {
    // 实现已提升为 `crate::dev_major_minor`（unix 共用：macOS 复用同一解码——同盘校验
    // 两侧同式的承重件，见 lib.rs）；本函数保留公开路径与原行为（T3 纯重构）。
    crate::dev_major_minor(dev)
}

/// 任意节点大小：rdev → /sys/dev/block/<maj>:<min>/size × 512。
pub fn block_size_bytes(rdev: u64, sysfs_root: &Path) -> Result<u64, DeviceError> {
    let (maj, min) = major_minor(rdev);
    let p = sysfs_root.join(format!("dev/block/{maj}:{min}/size"));
    let s = std::fs::read_to_string(&p).map_err(DeviceError::Io)?;
    s.trim().parse::<u64>().map(|n| n * 512).map_err(|e| {
        DeviceError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("坏 size 字段 {}: {e}", p.display()),
        ))
    })
}

/// 只读块设备后端（O_RDONLY；类型系统无写方法）。
pub struct LinuxBlockDevice {
    info: DeviceInfo,
    file: File,
}

impl LinuxBlockDevice {
    pub fn open(node: &Path) -> Result<Self, DeviceError> {
        Self::open_with_sysfs(node, Path::new("/sys"))
    }

    pub fn open_with_sysfs(node: &Path, sysfs_root: &Path) -> Result<Self, DeviceError> {
        // O_RDONLY + O_NONBLOCK（后者防 --device <fifo> 卡死在 open；块设备忽略该位）
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(O_NONBLOCK)
            .open(node)?;
        let md = file.metadata()?;
        if !md.file_type().is_block_device() {
            return Err(DeviceError::NotAFile(format!(
                "不是块设备节点: {}",
                node.display()
            )));
        }
        // 规范路径：--device /dev/disk/by-id/... 解到 /dev/sdX，与枚举项 id 必须逐字一致（T2 去重依赖）
        let canon = std::fs::canonicalize(node).unwrap_or_else(|_| node.to_path_buf());
        let kernel_name = canon
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| canon.display().to_string());
        let size_bytes = block_size_bytes(std::os::unix::fs::MetadataExt::rdev(&md), sysfs_root)?;
        // removable 与展示名都只在 sysfs（块设备 stat 无此信息）；条目缺失 → false / 回退内核名
        let dir = sysfs_root.join("block").join(&kernel_name);
        let removable = std::fs::read_to_string(dir.join("removable"))
            .map(|s| s.trim() == "1")
            .unwrap_or(false);
        // 型号名与枚举项一致（去重后 --device 行不丢型号）；model/vendor 读失败静默回退 kernel_name
        let model = std::fs::read_to_string(dir.join("device/model")).unwrap_or_default();
        let vendor = std::fs::read_to_string(dir.join("device/vendor")).unwrap_or_default();
        let name = compose_disk_name(&vendor, &model, &kernel_name);
        Ok(Self {
            info: DeviceInfo {
                id: format!("unix:{}", canon.display()),
                name,
                kind: DeviceKind::Physical,
                size_bytes,
                removable,
                fs_guess: None,
                transport: sysfs_transport(sysfs_root, &kernel_name), // 与 device.list 同源归类
            },
            file,
        })
    }
}

impl BlockDevice for LinuxBlockDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        crate::read_at_fill(&self.file, offset, buf)
    }

    /// 覆写：fstat **已打开的 fd**（不重开路径、无写路径——只读铁律不破），取 `st_rdev`。
    /// 导出目标同盘校验（-32006）的源侧事实。实现在 `crate::source_rdev_of`（unix 共用，
    /// macOS 同函数；T3 提升，Linux 行为逐位不变）。
    fn source_rdev(&self) -> Option<(u64, u64)> {
        crate::source_rdev_of(&self.file)
    }
}

#[cfg(test)]
mod tests {
    use super::enumerate::{is_listable_name, transport_str}; // pub(crate)：仅测试直接钉白名单/契约值域
    use super::*;
    use std::fs;

    /// 假 sysfs 条目：(name, model, size_sectors, removable, vendor?, is_partition)
    type FakeEntry<'a> = (&'a str, &'a str, u64, bool, Option<&'a str>, bool);

    /// 假根枚举出的 kernel_name（list() 已排序）
    fn listed_names(root: &Path) -> Vec<String> {
        let disks = BlockEnumerator::with_root(root).list().unwrap();
        disks.into_iter().map(|d| d.kernel_name).collect()
    }

    /// 造一个假 sysfs 根：class/block/<name>/{size,removable,device/model,partition?}
    fn fake_sysfs(entries: &[FakeEntry]) -> tempfile::TempDir {
        // (name, model, size_sectors, removable, vendor?, is_partition)
        let root = tempfile::tempdir().unwrap();
        let class = root.path().join("class/block");
        for (name, model, sectors, removable, vendor, part) in entries {
            let d = class.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("size"), format!("{sectors}\n")).unwrap();
            fs::write(d.join("removable"), if *removable { "1\n" } else { "0\n" }).unwrap();
            let dev = d.join("device");
            fs::create_dir_all(&dev).unwrap();
            if !model.is_empty() {
                fs::write(dev.join("model"), format!("{model}\n")).unwrap();
            }
            if let Some(v) = vendor {
                fs::write(dev.join("vendor"), format!("{v}\n")).unwrap();
            }
            if *part {
                fs::write(d.join("partition"), "1\n").unwrap();
            }
        }
        root
    }

    #[test]
    fn compose_disk_name_vendor_model() {
        // vendor+model 拼接 / model 空回退内核名 / model 已含 vendor 前缀不重复拼 / 两侧空白（sysfs 尾随 \n）
        assert_eq!(
            compose_disk_name("ATA", "ST2000LM015-2E81", "sda"),
            "ATA ST2000LM015-2E81"
        );
        assert_eq!(compose_disk_name("", "", "nvme0n1"), "nvme0n1");
        assert_eq!(
            compose_disk_name("ATA", "ATA ST2000LM015-2E81", "sda"),
            "ATA ST2000LM015-2E81"
        );
        assert_eq!(compose_disk_name("  ", " X \n", "sda"), "X");
    }

    #[test]
    fn parse_entry_reads_fields_and_scales_size() {
        let root = fake_sysfs(&[("sdb", "Ultra Fit", 3907029168, true, None, false)]);
        let d = parse_disk_entry(&root.path().join("class/block"), "sdb").unwrap();
        assert_eq!(d.kernel_name, "sdb");
        assert_eq!(d.name, "Ultra Fit");
        assert_eq!(d.size_bytes, 3907029168 * 512); // 恒 512 单位
        assert!(d.removable);
        assert_eq!(d.kind, DeviceKind::Physical);
        assert_eq!(d.node, Path::new("/dev/sdb"));
    }

    #[test]
    fn parse_entry_model_fallback_and_vendor_join() {
        // 无 model → 回退内核名
        let root = fake_sysfs(&[("nvme0n1", "", 1000, false, None, false)]);
        let d = parse_disk_entry(&root.path().join("class/block"), "nvme0n1").unwrap();
        assert_eq!(d.name, "nvme0n1");
        // model 带 vendor 前缀时拼 vendor
        let root = fake_sysfs(&[("sda", "ST2000LM015-2E81", 1000, false, Some("ATA"), false)]);
        let d = parse_disk_entry(&root.path().join("class/block"), "sda").unwrap();
        assert_eq!(d.name, "ATA ST2000LM015-2E81");
    }

    #[test]
    fn parse_entry_marks_partition_as_volume() {
        let root = fake_sysfs(&[("sda1", "X", 100, false, None, true)]);
        let d = parse_disk_entry(&root.path().join("class/block"), "sda1").unwrap();
        assert_eq!(d.kind, DeviceKind::Volume);
    }

    #[test]
    fn parse_entry_zero_size_is_none() {
        let root = fake_sysfs(&[("loop0", "", 0, false, None, false)]);
        assert!(parse_disk_entry(&root.path().join("class/block"), "loop0").is_none());
    }

    #[test]
    fn enumerate_classifies_transport_from_sysfs() {
        // 假根路径含 /usb → list() 内 canonicalize → classify_transport 应判 Usb
        // （qual 变异 11 回归：把该归类改成常量 Other 时本测试必须红）。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("usb1");
        let entry = root.join("class/block/sdb");
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("size"), "1000\n").unwrap();
        let disks = BlockEnumerator::with_root(&root).list().unwrap();
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].transport, Transport::Usb);
        assert_eq!(disks[0].device_info().transport.as_deref(), Some("usb"));
    }

    #[test]
    fn sysfs_transport_classifies_and_none_when_missing() {
        // 假根路径含 /usb（classify_transport 为纯字符串判定）→ 确定性验证
        // canonicalize → classify_transport → 契约字符串 全链；缺失条目 → None。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("usb1");
        std::fs::create_dir_all(root.join("class/block/sda")).unwrap();
        assert_eq!(sysfs_transport(&root, "sda").as_deref(), Some("usb"));
        assert_eq!(sysfs_transport(&root, "nope"), None);
    }

    #[test]
    fn transport_str_covers_contract_values() {
        // 契约值域（proto/v1/README.md）：usb|mmc|nvme|sata|virtio|other，逐变体不遗漏
        for (t, s) in [
            (Transport::Usb, "usb"),
            (Transport::Mmc, "mmc"),
            (Transport::Nvme, "nvme"),
            (Transport::Virtio, "virtio"),
            (Transport::Sata, "sata"),
            (Transport::Other, "other"),
        ] {
            assert_eq!(transport_str(t), s);
        }
        let root = fake_sysfs(&[("sdb", "USB Disk", 1000, true, None, false)]);
        let mut d = parse_disk_entry(&root.path().join("class/block"), "sdb").unwrap();
        d.transport = Transport::Usb;
        assert_eq!(d.device_info().transport.as_deref(), Some("usb"));
    }

    #[test]
    fn is_virtual_sysfs_path_pure() {
        assert!(is_virtual_sysfs_path("/sys/devices/virtual/block/loop0"));
        assert!(is_virtual_sysfs_path("/sys/devices/virtual/block/dm-0"));
        assert!(!is_virtual_sysfs_path(
            "/sys/devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda"
        ));
    }

    #[test]
    fn classify_transport_pure() {
        assert_eq!(
            classify_transport("/sys/.../usb1/1-2/1-2:1.0/host6/.../block/sdb"),
            Transport::Usb
        );
        assert_eq!(
            classify_transport("/sys/.../mmc_host/mmc0/.../block/mmcblk0"),
            Transport::Mmc
        );
        assert_eq!(
            classify_transport("/sys/.../nvme/nvme0/nvme0n1"),
            Transport::Nvme
        );
        assert_eq!(
            classify_transport("/sys/devices/pci.../virtio2/block/vda"),
            Transport::Virtio
        );
        assert_eq!(
            classify_transport("/sys/.../ata1/.../block/sda"),
            Transport::Sata
        );
        assert_eq!(
            classify_transport("/sys/devices/platform/foo/block/x"),
            Transport::Other
        );
    }

    #[test]
    fn list_allowlist_and_denylist() {
        let root = fake_sysfs(&[
            ("sda", "Disk A", 1000, false, None, false),
            ("sdb", "Disk B", 2000, true, None, false),
            ("nvme0n1", "", 3000, false, None, false),
            ("mmcblk0", "", 4000, false, None, false),
            ("vda", "", 5000, false, None, false),
            // 以下都不该出现：
            ("loop0", "", 6000, false, None, false),
            ("ram0", "", 7000, false, None, false),
            ("zram0", "", 8000, false, None, false),
            ("dm-0", "", 9000, false, None, false),
            ("md0", "", 10000, false, None, false),
            ("nbd0", "", 11000, false, None, false),
            ("sr0", "", 12000, false, None, false),
            ("mmcblk0boot0", "", 13000, false, None, false),
            ("mmcblk0boot1", "", 13001, false, None, false),
            ("mmcblk0rpmb", "", 14000, false, None, false),
            ("sda1", "Part", 15000, false, None, true), // 分区（volume）
        ]);
        let names = listed_names(root.path());
        assert_eq!(names, ["mmcblk0", "nvme0n1", "sda", "sdb", "vda"]); // 排序后
    }

    #[test]
    fn list_skips_unreadable_entry_not_abort() {
        // 一个条目 size 读不出（近似 EIO 路径）→ 跳过该条目，其余照常返回。
        // 用同名目录替代 chmod 000：read_to_string 必 EISDIR，root 下同样成立。
        let root = fake_sysfs(&[
            ("sda", "A", 1000, false, None, false),
            ("sdb", "B", 2000, false, None, false),
        ]);
        let sdb_size = root.path().join("class/block/sdb/size");
        fs::remove_file(&sdb_size).unwrap();
        fs::create_dir(&sdb_size).unwrap();
        assert_eq!(listed_names(root.path()), ["sda"]);
    }

    #[test]
    fn list_skips_non_utf8_entry_name() {
        use std::os::unix::ffi::OsStrExt;
        let root = fake_sysfs(&[("sda", "A", 1000, false, None, false)]);
        // 非 UTF-8 名字的合法条目（size 可读）→ 必须由名字分支排除，而非靠解析失败兜底
        let weird: &std::ffi::OsStr = std::ffi::OsStr::from_bytes(b"sdc\xff");
        let weird_dir = root.path().join("class/block").join(weird);
        fs::create_dir(&weird_dir).unwrap();
        fs::write(weird_dir.join("size"), "1000\n").unwrap();
        assert_eq!(listed_names(root.path()), ["sda"]);
    }

    #[test]
    fn parse_entry_size_non_numeric_is_none() {
        let root = tempfile::tempdir().unwrap();
        let d = root.path().join("class/block/sda");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("size"), "abc\n").unwrap();
        assert!(parse_disk_entry(&root.path().join("class/block"), "sda").is_none());
    }

    #[test]
    fn is_listable_name_mmc_forms() {
        // 整盘 = mmcblk<纯数字>；boot0/boot1/rpmb/gp0 等伪设备（幻影盘）一律排除
        assert!(is_listable_name("mmcblk0"));
        assert!(is_listable_name("mmcblk12"));
        assert!(!is_listable_name("mmcblk0boot0"));
        assert!(!is_listable_name("mmcblk0boot1"));
        assert!(!is_listable_name("mmcblk0rpmb"));
        assert!(!is_listable_name("mmcblk0gp0"));
        assert!(!is_listable_name("mmcblk"));
    }

    #[test]
    fn list_missing_class_dir_is_err() {
        let root = tempfile::tempdir().unwrap();
        assert!(BlockEnumerator::with_root(root.path()).list().is_err());
    }

    #[test]
    fn dev_t_major_minor_roundtrip_kat() {
        // glibc 编码 KAT（gnu_dev_makedev 公式）
        assert_eq!(major_minor(makedev(8, 0)), (8, 0));
        assert_eq!(major_minor(makedev(8, 16)), (8, 16));
        assert_eq!(major_minor(makedev(259, 5)), (259, 5));
        assert_eq!(major_minor(makedev(259, 4097)), (259, 4097));
    }

    #[test]
    fn block_size_from_fake_sysfs() {
        let root = tempfile::tempdir().unwrap();
        let d = root.path().join("dev/block/8:0");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("size"), "3907029168\n").unwrap();
        assert_eq!(
            block_size_bytes(makedev(8, 0), root.path()).unwrap(),
            2000398934016
        );
        // 缺失 → Err
        assert!(block_size_bytes(makedev(7, 7), root.path()).is_err());
        // 坏字段 → Io(InvalidData)（不再误报 NotAFile）
        let bad = root.path().join("dev/block/9:9");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("size"), "abc\n").unwrap();
        assert!(matches!(
            block_size_bytes(makedev(9, 9), root.path()),
            Err(DeviceError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidData
        ));
    }

    #[test]
    fn linux_block_device_rejects_non_block_node() {
        // 单测环境无法造真块设备（需 root）；以常规文件节点验证拒绝路径
        let f = tempfile::NamedTempFile::new().unwrap();
        assert!(matches!(
            LinuxBlockDevice::open_with_sysfs(f.path(), Path::new("/sys")),
            Err(DeviceError::NotAFile(_))
        ));
    }
}
