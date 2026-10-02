// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! Linux 物理块设备：sysfs 枚举（根可注入）+ 只读块设备后端。
//!
//! 设计约束（M1e 简报，本机实测）：
//! - `device.list` 零 `open()`：枚举只读 sysfs（普通权限即可工作）。
//! - 块设备 `stat.st_size` 恒 0——大小必须走 sysfs，恒 512 字节单位。
//! - 虚拟设备（loop/ram/zram/dm/md/nbd）canonicalize 后以 `/sys/devices/virtual/` 开头，跳过。
//! - `removable` 不可作过滤（USB-SATA 硬盘盒报 0）；运输类型仅作分组提示。
//! - 只读铁律落点 = 类型系统（`BlockDevice` 无写方法）+ `O_RDONLY`。

use crate::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileExt, FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

// Linux x86_64 O_NONBLOCK（man 2 open）；块设备忽略该位，仅防 --device <fifo> 卡死在 open。
const O_NONBLOCK: i32 = 0o4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    Mmc,
    Nvme,
    Virtio,
    Sata,
    Other,
}

#[derive(Debug, Clone)]
pub struct RawDisk {
    pub node: PathBuf,       // /dev/sda
    pub kernel_name: String, // sda
    pub name: String,        // vendor + model（缺失回退 kernel_name）
    pub size_bytes: u64,
    pub removable: bool,
    pub transport: Transport,
    pub kind: DeviceKind,
}

impl RawDisk {
    /// 映射为 IPC 契约类型。id 语法 `unix:<节点>` 集中此处（daemon 侧不再格式化）。
    pub fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("unix:{}", self.node.display()),
            name: self.name.clone(),
            kind: self.kind,
            size_bytes: self.size_bytes,
            removable: self.removable,
            fs_guess: None, // device.list 零 open()；FS 探测归 M1b 按需调用
        }
    }
}

pub struct BlockEnumerator {
    sysfs_root: PathBuf,
}

impl BlockEnumerator {
    pub fn new() -> Self {
        Self {
            sysfs_root: PathBuf::from("/sys"),
        }
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self {
            sysfs_root: root.into(),
        }
    }

    /// 枚举整盘（physical）设备。逐条目容错：单条目读失败跳过，不中止整体。
    pub fn list(&self) -> Result<Vec<RawDisk>, DeviceError> {
        let class = self.sysfs_root.join("class/block");
        let entries = std::fs::read_dir(&class).map_err(DeviceError::Io)?;
        let mut out: Vec<RawDisk> = Vec::new();
        for e in entries.flatten() {
            let Some(name) = e.file_name().to_str().map(String::from) else {
                continue;
            };
            let Some(mut d) = parse_disk_entry(&class, &name) else {
                continue;
            };
            if d.kind != DeviceKind::Physical {
                continue; // M1e 只列整盘（分区归 M2 卷级流程）
            }
            if !is_listable_name(&name) {
                continue;
            }
            // 虚拟设备防御（真 /sys 下生效；假根单测走 is_virtual_sysfs_path 纯函数覆盖）
            if let Ok(real) = std::fs::canonicalize(class.join(&name)) {
                if is_virtual_sysfs_path(&real.to_string_lossy()) {
                    continue;
                }
                d.transport = classify_transport(&real.to_string_lossy());
            }
            out.push(d);
        }
        out.sort_by(|a, b| a.kernel_name.cmp(&b.kernel_name));
        Ok(out)
    }
}

impl Default for BlockEnumerator {
    fn default() -> Self {
        Self::new()
    }
}

/// 展示名合成：vendor + model（去重后不丢型号）；model 缺失回退内核名。
/// parse（枚举）与 open（--device）共用，保证同一块盘的 device.list 行名一致。
fn compose_disk_name(vendor: &str, model: &str, kernel_name: &str) -> String {
    let (v, m) = (vendor.trim(), model.trim());
    if m.is_empty() {
        kernel_name.to_string()
    } else if !v.is_empty() && !m.starts_with(v) {
        format!("{v} {m}")
    } else {
        m.to_string()
    }
}

/// 解析一个 sysfs 条目；size==0 或读不到 size → None（逐条目容错）。
pub fn parse_disk_entry(class_dir: &Path, name: &str) -> Option<RawDisk> {
    let dir = class_dir.join(name);
    let s = std::fs::read_to_string(dir.join("size")).ok()?;
    let sectors: u64 = s.trim().parse().ok()?;
    if sectors == 0 {
        return None;
    }
    let removable = std::fs::read_to_string(dir.join("removable"))
        .map(|s| s.trim() == "1")
        .unwrap_or(false);
    // sysfs 值带尾随 \n，去空白在 compose_disk_name 内统一处理
    let model = std::fs::read_to_string(dir.join("device/model")).unwrap_or_default();
    let vendor = std::fs::read_to_string(dir.join("device/vendor")).unwrap_or_default();
    let name_field = compose_disk_name(&vendor, &model, name);
    let kind = if dir.join("partition").exists() {
        DeviceKind::Volume
    } else {
        DeviceKind::Physical
    };
    Some(RawDisk {
        node: PathBuf::from(format!("/dev/{name}")),
        kernel_name: name.to_string(),
        name: name_field,
        size_bytes: sectors * 512, // sysfs 恒 512 字节单位
        removable,
        transport: Transport::Other, // 由调用方按 realpath 归类
        kind,
    })
}

/// 纯字符串：/sys 真实路径是否虚拟设备（loop/ram/zram/dm-/md*/nbd* 全在 virtual 下）。
pub fn is_virtual_sysfs_path(realpath: &str) -> bool {
    realpath.starts_with("/sys/devices/virtual/")
}

/// 纯字符串：realpath → 运输类型（仅分组提示，不作过滤依据）。
pub fn classify_transport(realpath: &str) -> Transport {
    if realpath.contains("/usb") {
        Transport::Usb
    } else if realpath.contains("/mmc") {
        Transport::Mmc
    } else if realpath.contains("/nvme") {
        Transport::Nvme
    } else if realpath.contains("virtio") {
        Transport::Virtio
    } else if realpath.contains("/ata") || realpath.contains("/sata") {
        Transport::Sata
    } else {
        Transport::Other
    }
}

/// 名称白名单（整盘形态）：sd*、nvme*、mmcblk<纯数字>、vd*。
/// 分区的排除靠 `kind == Volume`（sysfs 的 `partition` 文件），不按名字数位猜。
fn is_listable_name(name: &str) -> bool {
    if ["loop", "ram", "zram", "dm-", "md", "nbd", "sr"]
        .iter()
        .any(|p| name.starts_with(p))
    {
        return false;
    }
    if let Some(rest) = name.strip_prefix("mmcblk") {
        // 紧化形态：mmcblk<纯数字>。boot0/boot1/rpmb/gp0 等伪设备（幻影盘）天然排除。
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    }
    name.starts_with("sd") || name.starts_with("nvme") || name.starts_with("vd")
}

/// glibc dev_t 编码（gnu_dev_makedev / major / minor）。
pub fn makedev(major: u64, minor: u64) -> u64 {
    ((major & 0xfff) << 8) | (minor & 0xff) | ((minor & !0xff) << 12) | ((major & !0xfff) << 32)
}

pub fn major_minor(dev: u64) -> (u64, u64) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major, minor)
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
        // lib.rs 契约：读取直到填满 buf 或到达 EOF；块设备短读常见，需循环补齐。
        let mut done = 0usize;
        while done < buf.len() {
            match self.file.read_at(&mut buf[done..], offset + done as u64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) => return Err(DeviceError::Io(e)),
            }
        }
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
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
