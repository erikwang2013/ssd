# 小盾 M1e：Linux 物理设备枚举 + 提权 + 打包实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Linux 平台层三件套：① sysfs 设备枚举（零新依赖、可注入根做无特权单测）；② daemon 真设备接入（`--device` + `device.list` 零 `open()`）+ 环回设备 e2e；③ 提权落地件（udev uaccess 规则 + polkit 策略 + root 路径校验）与 deb 打包。

**Architecture:** 枚举直读 sysfs（std 即可）；真块设备后端 `LinuxBlockDevice`（O_RDONLY + `/sys/dev/block/M:m/size`）；提权 = **udev uaccess 为主（零代码改动）、pkexec 兜底**（策略 `auth_admin` 不 keep——pkexec 不校验参数，见安全注记）。

**关键实证结论（来自 `/tmp/xd-m1e-brief.md`，均本机实测）：**
- 块设备 `stat.st_size == 0`——大小必须走 sysfs（枚举用 `class/block/<n>/size`，任意节点用 `stat.st_rdev → /sys/dev/block/M:m/size`），恒 512 字节单位。
- `removable` **不可作过滤**（USB-SATA 硬盘盒报 0）；可移动性只用 realpath `/usb` 子串作分组提示。
- 虚拟设备一刀切：`canonicalize` 后以 `/sys/devices/virtual/` 开头即跳过（覆盖 loop/ram/zram/dm/md/nbd）。
- 只读铁律在 Linux **只能**落在类型系统（`BlockDevice` 无写方法）+ O_RDONLY；uaccess/disk 组给的都是 **rw**。
- **pkexec 陷阱**：策略必须 `auth_admin`（非 `_keep`）——否则 `pkexec xd-daemon --image /etc/shadow` 是本地任意文件读取。
- CI：GH `ubuntu-latest` 有免密 sudo（losetup e2e 可跑）；本机 deepin 无（脚本探测式跳过）。
- **M1e 决策钉死**：只列整盘（physical）、不按 removable 过滤、**deb + uaccess 主 / pkexec 兜底 / AppImage 不做**（AppImage 与特权模型结构性冲突：FUSE nosuid + 装不了 udev/polkit）。

**契约耦合（归 M1b v1，M1e 只提需求不改 proto）**：`DeviceInfo` 需增 `transport`（usb/mmc/nvme/…）与 `accessible`（EACCES 提示位）；M1e 内部先用自有 `Transport` 枚举，不落 v0 契约。

**未验证边界（平台专有，标注"未验证"）**：pkexec 真实认证路径、真 U 盘热插拔、`/dev/sdX` 真实介质行为、udev 规则在真机上的生效——CI 都覆盖不到，需真机手测（M1 出口门槛）。

---

## 文件结构

```
crates/
├── xd-device/src/linux.rs        # 枚举 + LinuxBlockDevice（T1）
├── xd-core/src/lib.rs(+handlers) # CoreCtx::add_list_only（T2）
└── xd-daemon/src/main.rs         # --device 参数 + 启动枚举（T2）+ root 参数校验（T3）
packaging/
├── udev/71-xiaodun-uaccess.rules # 提权（T3）
├── polkit/com.erik.xiaodun.policy# 提权（T3）
└── deb/                          # deb 树（T4）
scripts/
├── e2e-loop.sh                   # 环回真块设备 e2e（T2）
└── e2e-deb.sh                    # 容器内 deb 装/卸验证（T4）
```

---

### Task 1: xd-device::linux —— sysfs 枚举 + 真块设备后端

**Files:**
- Create: `crates/xd-device/src/linux.rs`
- Modify: `crates/xd-device/src/lib.rs`（+`#[cfg(target_os = "linux")] pub mod linux;`）

- [ ] **Step 1: 测试（linux.rs 末尾；全部无特权、全平台可跑——只依赖 tempdir 假 sysfs 根与纯函数）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 造一个假 sysfs 根：class/block/<name>/{size,removable,device/model,partition?}
    fn fake_sysfs(entries: &[(&str, &str, u64, bool, Option<&str>, bool)]) -> tempfile::TempDir {
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
        assert_eq!(classify_transport("/sys/.../usb1/1-2/1-2:1.0/host6/.../block/sdb"), Transport::Usb);
        assert_eq!(classify_transport("/sys/.../mmc_host/mmc0/.../block/mmcblk0"), Transport::Mmc);
        assert_eq!(classify_transport("/sys/.../nvme/nvme0/nvme0n1"), Transport::Nvme);
        assert_eq!(classify_transport("/sys/devices/pci.../virtio2/block/vda"), Transport::Virtio);
        assert_eq!(classify_transport("/sys/.../ata1/.../block/sda"), Transport::Sata);
        assert_eq!(classify_transport("/sys/devices/platform/foo/block/x"), Transport::Other);
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
            ("mmcblk0rpmb", "", 14000, false, None, false),
            ("sda1", "Part", 15000, false, None, true), // 分区（volume）
        ]);
        let disks = BlockEnumerator::with_root(root.path()).list().unwrap();
        let names: Vec<&str> = disks.iter().map(|d| d.kernel_name.as_str()).collect();
        assert_eq!(names, vec!["mmcblk0", "nvme0n1", "sda", "sdb", "vda"]); // 排序后
    }

    #[test]
    fn list_skips_unreadable_entry_not_abort() {
        // 一个条目 size 文件不可读（近似 EIO 路径）→ 跳过该条目，其余照常返回
        let root = fake_sysfs(&[("sda", "A", 1000, false, None, false), ("sdb", "B", 2000, false, None, false)]);
        let sdb_size = root.path().join("class/block/sdb/size");
        let mut perms = fs::metadata(&sdb_size).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o000);
        fs::set_permissions(&sdb_size, perms).unwrap();
        let disks = BlockEnumerator::with_root(root.path()).list().unwrap();
        assert_eq!(disks.iter().map(|d| d.kernel_name.as_str()).collect::<Vec<_>>(), vec!["sda"]);
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
        assert_eq!(block_size_bytes(makedev(8, 0), root.path()).unwrap(), 2000398934016);
        // 缺失 → Err
        assert!(block_size_bytes(makedev(9, 9), root.path()).is_err());
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
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xd-device`
Expected: 编译失败（`linux` 模块与各符号未定义）。

- [ ] **Step 3: 实现 `crates/xd-device/src/linux.rs`**

```rust
//! Linux 物理块设备：sysfs 枚举（根可注入）+ 只读块设备后端。
//!
//! 设计约束（M1e 简报，本机实测）：
//! - `device.list` 零 `open()`：枚举只读 sysfs（普通权限即可工作）。
//! - 块设备 `stat.st_size` 恒 0——大小必须走 sysfs，恒 512 字节单位。
//! - 虚拟设备（loop/ram/zram/dm/md/nbd）canonicalize 后以 `/sys/devices/virtual/` 开头，跳过。
//! - `removable` 不可作过滤（USB-SATA 硬盘盒报 0）；运输类型仅作分组提示。
//! - 只读铁律落点 = 类型系统（`BlockDevice` 无写方法）+ `O_RDONLY`。

use crate::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};
use std::fs::File;
use std::os::unix::fs::{FileExt, FileTypeExt};
use std::path::{Path, PathBuf};

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

pub struct BlockEnumerator {
    sysfs_root: PathBuf,
}

impl BlockEnumerator {
    pub fn new() -> Self {
        Self { sysfs_root: PathBuf::from("/sys") }
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { sysfs_root: root.into() }
    }

    /// 枚举整盘（physical）设备。逐条目容错：单条目读失败跳过，不中止整体。
    pub fn list(&self) -> Result<Vec<RawDisk>, DeviceError> {
        let class = self.sysfs_root.join("class/block");
        let entries = std::fs::read_dir(&class).map_err(DeviceError::Io)?;
        let mut out: Vec<RawDisk> = Vec::new();
        for e in entries.flatten() {
            let Some(name) = e.file_name().to_str().map(String::from) else { continue };
            let Some(mut d) = parse_disk_entry(&class, &name) else { continue };
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

/// 解析一个 sysfs 条目；size==0 或读不到 size → None（逐条目容错）。
pub fn parse_disk_entry(class_dir: &Path, name: &str) -> Option<RawDisk> {
    let dir = class_dir.join(name);
    let sectors: u64 = std::fs::read_to_string(dir.join("size")).ok()?.trim().parse().ok()?;
    if sectors == 0 {
        return None;
    }
    let removable = std::fs::read_to_string(dir.join("removable"))
        .map(|s| s.trim() == "1")
        .unwrap_or(false);
    let model = std::fs::read_to_string(dir.join("device/model"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let vendor = std::fs::read_to_string(dir.join("device/vendor"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let name_field = if model.is_empty() {
        name.to_string()
    } else if !vendor.is_empty() && !model.starts_with(&vendor) {
        format!("{vendor} {model}")
    } else {
        model
    };
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

/// 名称白名单（整盘形态）：sd*、nvme*、mmcblk*（排除 boot0/rpmb）、vd*。
/// 分区的排除靠 `kind == Volume`（sysfs 的 `partition` 文件），不按名字数位猜。
fn is_listable_name(name: &str) -> bool {
    if ["loop", "ram", "zram", "dm-", "md", "nbd", "sr"]
        .iter()
        .any(|p| name.starts_with(p))
    {
        return false;
    }
    if name.ends_with("boot0") || name.ends_with("rpmb") {
        return false;
    }
    name.starts_with("sd")
        || name.starts_with("nvme")
        || name.starts_with("mmcblk")
        || name.starts_with("vd")
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
    s.trim()
        .parse::<u64>()
        .map(|n| n * 512)
        .map_err(|e| DeviceError::NotAFile(format!("坏 size 字段 {}: {e}", p.display())))
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
        let file = File::open(node)?; // O_RDONLY
        let md = file.metadata()?;
        if !md.file_type().is_block_device() {
            return Err(DeviceError::NotAFile(format!("不是块设备节点: {}", node.display())));
        }
        let size_bytes = block_size_bytes(std::os::unix::fs::MetadataExt::rdev(&md), sysfs_root)?;
        let kernel_name = node
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| node.display().to_string());
        Ok(Self {
            info: DeviceInfo {
                id: format!("unix:{}", node.display()),
                name: kernel_name,
                kind: DeviceKind::Physical,
                size_bytes,
                removable: false,
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
        self.file.read_at(buf, offset).map_err(DeviceError::Io)
    }
}
```

> **设计取舍（已定案）**：`name` = `vendor + " " + model`（vendor 非空且 model 未以 vendor 开头时拼接；model 缺失回退 kernel_name）。理由：内核把 model 截断到 16 字符，长型号需 vendor 补全辨识度；UI 列表直接可用。

- [ ] **Step 4: 运行** → **12 passed**（xd-device 由 **4 → 16**；worktree workspace 88 → **100**）

- [ ] **Step 5: Commit** `feat(device): Linux sysfs 枚举与只读块设备后端（可注入根）`

**修订轮（执行侧，2026-10-02）**：实施提交 `b0c760c`（分支 m1e-linux）。偏离 3 条均机械性：
① clippy::type_complexity → `type FakeEntry<'a> = (...)` 别名（无 lint 抑制）；② rustfmt 后处理；③ 本步计数勘误
（计划原写"6→18"，实际基线 4 → 16，已就地更正）。
**计划外实证（超出合成测试）**：`BlockEnumerator::new().list()` 对**真机 /sys** 跑通——sda
"ATA ST2000LM015-2E81" 2000398934016B/Sata + sdb "ATA Samsung SSD 850" 500107862016B/Sata；
loop0-7 与全部分区正确过滤；vendor 拼接实证有据（sysfs model 恰 16 字符截断 "ST2000LM015-2E81"，
lsblk FULL 为 "ST2000LM015-2E8174"）；rdev→sysfs 往返对真实设备节点（8:0/8:16/8:1/8:22）逐一对上 lsblk -b。
**咨询备注（无需改码，留档）**：a) `major_minor` 的 minor 掩码 `& !0xff` 对合成越界 dev_t（major ≥ 2^12）往返
false——内核 major 12 位不可达；b) `list_skips_unreadable_entry_not_abort` 用 0o000 手法，root/CAP_DAC_OVERRIDE
下会失效（CI 无暴露面）。

**修复轮（qual-t1，2026-10-02）**：质量审查 2 Important + 若干 Minor（Critical 无），修复提交 `abc52e5` + 终修 `0cd6b96`：

- **I1（T2 计划缺口，已在本文件 T2 修正）**：`device.list` 同盘重复（`--device /dev/sda` 与枚举项 id 撞车）→
  `CoreCtx::device_infos` 按 id 去重（打开者优先）+ 专用测试。
- **I2**：`is_listable_name` mmc 形态紧化（`strip_prefix("mmcblk")` + 全 ASCII 数字）——`mmcblk0boot1`/`gp*`
  幻影盘天然排除，后缀黑名单删除；谓词 KAT `is_listable_name_mmc_forms`。
- **Minor 3-8**：`read_at` 循环补齐（对齐 trait 契约与 `image.rs`）；坏 size 字段 → `Io(InvalidData)`；
  `canonicalize` 派生 id/kernel_name（by-id 场景真机验证 `ata-…→/dev/sdb`）+ sysfs `removable`；
  `O_NONBLOCK`（FIFO 打开 15.6µs 返回 `NotAFile`——实测防挂死）；0o000 → EISDIR（root 下亦稳）；
  非 UTF-8 条目名与 size 非数字测试。
- **终修 `0cd6b96`**：非 UTF-8 测试加齿（补 size 使其"否则合法"，否则该测试无判别力）；linux.rs 折行至
  **恰 500 行**；`InvalidData` 断言；三处机械折行（语义零变化）。
- **归因更正**：minor 掩码的"glibc 0xfff00"说法不精确——`0xfff00` 是**内核** `new_decode_dev` 掩码；glibc
  （`bits/sysmacros.h`）掩码不同但可达域（major<2^12、minor<2^20）逐值一致。结论"不可达、无需改码"不变。

计数：xd-device **19**、worktree workspace **103**。M1e-T1 关闭。

---

### Task 2: daemon 接入（`--device` + 启动枚举）与环回 e2e

**Files:**
- Modify: `crates/xd-device/src/linux.rs`（**追加** `impl RawDisk { pub fn device_info(&self) -> DeviceInfo }`——映射放 xd-device，
  避免 "unix:" 语法在 daemon 侧二次格式化漂移；也是去重的实现点）
- Modify: `crates/xd-core/src/handlers.rs`（CoreCtx + list_only + 去重）
- Modify: `crates/xd-daemon/src/main.rs`（`--device` 参数 + 启动枚举）
- Create: `crates/xd-device/tests/loop_e2e.rs`（环境变量门控，无特权静默跳过）
- Create: `scripts/e2e-loop.sh`
- Modify: `.github/workflows/ci.yml`（Linux e2e 步骤后加 `bash scripts/e2e-loop.sh`）

- [ ] **Step 1: 测试**

xd-core handlers.rs 测试模块新增：

```rust
    #[test]
    fn device_list_merges_list_only_after_opened() {
        use xd_device::{DeviceInfo, DeviceKind};
        let f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f.as_file(), &vec![0u8; 512]).unwrap();
        let img = ImageFileDevice::open(f.path()).unwrap();
        let ctx = CoreCtx::new(vec![Box::new(img)]).with_list_only(vec![DeviceInfo {
            id: "unix:/dev/sda".into(),
            name: "Disk".into(),
            kind: DeviceKind::Physical,
            size_bytes: 1 << 40,
            removable: false,
            fs_guess: None,
        }]);
        let infos = ctx.device_infos();
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].kind, DeviceKind::Image); // devices 在前（既有顺序不变）
        assert_eq!(infos[1].id, "unix:/dev/sda");
    }
```
（xd-core dev-deps 需已有 tempfile——若缺，加 dev-dependency。）

再加去重测试（同 id 的打开项 + 枚举项 → 只出现一次，打开者优先）：

```rust
    #[test]
    fn device_list_dedupes_by_id() {
        use xd_device::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};
        struct Stub;
        impl BlockDevice for Stub {
            fn info(&self) -> &DeviceInfo {
                static I: std::sync::OnceLock<DeviceInfo> = std::sync::OnceLock::new();
                I.get_or_init(|| DeviceInfo {
                    id: "unix:/dev/sda".into(),
                    name: "opened".into(),
                    kind: DeviceKind::Physical,
                    size_bytes: 42,
                    removable: false,
                    fs_guess: None,
                })
            }
            fn read_at(&self, _o: u64, _b: &mut [u8]) -> Result<usize, DeviceError> {
                Ok(0)
            }
        }
        let ctx = CoreCtx::new(vec![Box::new(Stub)]).with_list_only(vec![DeviceInfo {
            id: "unix:/dev/sda".into(), // 与打开项同 id → 必须被去重
            name: "enumerated".into(),
            kind: DeviceKind::Physical,
            size_bytes: 42,
            removable: false,
            fs_guess: None,
        }]);
        let infos = ctx.device_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "opened");
    }
```

`crates/xd-device/tests/loop_e2e.rs`：

```rust
//! 环回真块设备 e2e：由 scripts/e2e-loop.sh（root）建好环回并传入环境变量。
//! 未设置时静默通过——无特权环境（本机日常）自动跳过，CI 上由脚本驱动真跑。
use std::path::Path;
use xd_device::BlockDevice; // size_bytes/read_at 是 trait 方法（qual-t1 预告的编译修正）

#[test]
fn loop_device_reads_identical_bytes() {
    let (Ok(dev_path), Ok(img_path)) = (std::env::var("XD_LOOP_DEV"), std::env::var("XD_LOOP_IMG")) else {
        eprintln!("skip: XD_LOOP_DEV/XD_LOOP_IMG 未设置（无特权环境）");
        return;
    };
    let dev = xd_device::linux::LinuxBlockDevice::open(Path::new(&dev_path)).unwrap();
    let img = std::fs::read(&img_path).unwrap();
    assert_eq!(dev.size_bytes(), img.len() as u64, "sysfs 大小必须与镜像一致");
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
```

- [ ] **Step 2: 实现**

handlers.rs：

```rust
pub struct CoreCtx {
    devices: Vec<Box<dyn BlockDevice>>,
    /// 枚举到但**未打开**的设备（device.list 用；零 open()——M1e 契约要求）。
    list_only: Vec<DeviceInfo>,
}

impl CoreCtx {
    pub fn new(devices: Vec<Box<dyn BlockDevice>>) -> Self {
        Self { devices, list_only: Vec::new() }
    }

    pub fn with_list_only(mut self, infos: Vec<DeviceInfo>) -> Self {
        self.list_only = infos;
        self
    }

    /// 打开的设备优先；`list_only` 中与已打开 id 重复的条目丢弃——
    /// 否则 `--device /dev/sda` 会与枚举出的同一块盘在 device.list 里出现两次（qual-t1 I1）。
    pub fn device_infos(&self) -> Vec<DeviceInfo> {
        let mut v: Vec<DeviceInfo> = self.devices.iter().map(|d| d.info().clone()).collect();
        for info in &self.list_only {
            if !v.iter().any(|e| e.id == info.id) {
                v.push(info.clone());
            }
        }
        v
    }
}
```

`crates/xd-device/src/linux.rs` 追加（映射集中一处，qual-t1 建议）：

```rust
impl RawDisk {
    /// 映射为 IPC 契约类型。id 语法 `unix:/dev/<node>` 集中此处（daemon 侧不再格式化）。
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
```

daemon main.rs：
- 新增 `--device <node>`（可重复）：Linux 下 `LinuxBlockDevice::open(&path)` 注册进 devices（错误 → exit 2，同 `--image` 风格）；非 Linux 平台 → 报错 "仅 Linux 支持"。
- 启动枚举（Linux，cfg-gated）：

```rust
    #[cfg(target_os = "linux")]
    let list_only: Vec<xd_device::DeviceInfo> = xd_device::linux::BlockEnumerator::new()
        .list()
        .unwrap_or_default() // 枚举失败不阻塞 daemon 启动
        .iter()
        .map(|d| d.device_info()) // 映射在 xd-device（qual-t1 建议）
        .collect();
    #[cfg(not(target_os = "linux"))]
    let list_only: Vec<xd_device::DeviceInfo> = Vec::new();

    let ctx = CoreCtx::new(devices).with_list_only(list_only);
```
（启动横幅加一行 `· 枚举到 N 个物理磁盘`。）

`scripts/e2e-loop.sh`（完整内容）：

```bash
#!/usr/bin/env bash
# 真块设备端到端（Linux）：镜像 → 环回只读设备 → LinuxBlockDevice 字节级 + daemon device.list。
# 无免密 sudo（本机日常）自动跳过；GitHub ubuntu-latest 免密 sudo → 真跑。
set -euo pipefail
cd "$(dirname "$0")/.."

sudo -n true 2>/dev/null || { echo "skip: 无免密 sudo（真块设备 e2e 需 root 建环回）"; exit 0; }

img=$(mktemp /tmp/xd-loop-$$-XXXX.img)
cargo run -q --locked -p xd-fixtures --example gen_fat_image -- "$img"
cargo build -q --locked -p xd-daemon
before=$(sha256sum "$img" | cut -d' ' -f1)

loop=$(sudo losetup -r -f --show "$img")   # -r：内核强制只读
trap 'sudo losetup -d "$loop"' EXIT
echo "loop=$loop (sysfs ro=$(cat "/sys/block/$(basename "$loop")/ro"))"

# 1) LinuxBlockDevice：全量字节比对 + 偏移抽样（测试内断言）
XD_LOOP_DEV="$loop" XD_LOOP_IMG="$img" cargo test -q --locked -p xd-device --test loop_e2e -- --nocapture

# 2) daemon：--device 注册 + device.list 零 open() 路径出现 unix:$loop
out=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"device.list","params":null}' \
  | sudo ./target/debug/xd-daemon --device "$loop")
grep -q "unix:$loop" <<<"$out" || { echo "FAIL: device.list 未含 $loop"; echo "$out"; exit 1; }

# 3) 只读铁律：源镜像扫描前后 sha256 不变
[ "$before" = "$(sha256sum "$img" | cut -d' ' -f1)" ] || { echo "FAIL: 源镜像被改写"; exit 1; }
echo "LOOP E2E OK"
```

ci.yml：Linux e2e 步骤后追加一步 `bash scripts/e2e-loop.sh`（手动触发策略不变）。

- [ ] **Step 3: 运行** → xd-core +2 测试（合并 + 去重）；xd-device 集成测试本地静默跳过；workspace 全绿；`bash scripts/e2e-loop.sh` 本机输出 `skip: 无免密 sudo…`（退出 0）
- [ ] **Step 4: Commit** `feat(daemon): --device 注册与启动枚举（device.list 零 open），环回 e2e 脚本`

**执行记录（2026-10-02）**：实施提交 `06801c7` + 跟进 `d213cd5`（Dart 侧修补）。计数：worktree workspace
103 → **106**（xd-core +2、loop_e2e 集成 +1 静默通过）。偏离 6 条：

1. **计划外文件 `crates/xd-daemon/tests/ipc.rs`**：`device_list_with_image` 的 `len()==1` 与启动枚举冲突（本机实测 3 条）
   → 按 `kind=="image"` 过滤断言（保原意；下标 0 顺序断言由新核心单测覆盖）。
2. **计划外文件 `ui/test/ipc_integration_test.dart`（同因漏改，spec 审查发现）**：真 daemon 集成测试
   `hasLength(1)`/`.single` 于 Linux 必挂 → 同法过滤；本机（真物理盘，比 CI 更能暴露）flutter test 10/10
   红→绿实证；repo 级同类断言全扫（golden 解码/e2e.sh grep/Handler 构造类均无需改）。
3. `loop_e2e.rs` 加 `#![cfg(target_os="linux")]`（计划漏，win/mac 矩阵编译必需）。
4. handlers 测试 `&vec![]` → `&[0u8;512]`（clippy useless_vec）。
5. `linux.rs` 现 **514 行**（+14 为 `device_info()` 内聚 impl，rustfmt 强制展开；接受，先例 644/631 行）。
6. 横幅「N 个设备已注册 · 枚举到 M 个物理磁盘」（原"镜像设备"字面在 --device 上线后失准）。

**独立验证（spec 审查，LD_PRELOAD 替代 strace）**：纯枚举 0 条 /dev open（对照组 `cat /dev/null` 被捕获）；
`/proc/<pid>/fd` 空闲仅 0/1/2；`unshare -rm` 遮蔽 /sys 后"枚举 0 盘"不阻塞 daemon；Windows 交叉
`cargo check --target x86_64-pc-windows-msvc --all-targets` 干净；`--device` 四类错误路径 exit 2 清晰。
计划教训（两条断言漏改）记 M1e 账：**daemon 行为变更必须同步扫 Rust 与 Dart 两侧测试**。

**修复轮（qual-t2，2026-10-02）**：质量审查 2 Important + 5 Minor（+增量复审 Important-δ），修复提交
`cef6654` + 终修 `6c719d7`：

- **I1（CI 阻断）**：非特权 `cargo test` 打不开 `brw-rw---- root:disk` 的环回节点 → `sudo chmod 666 "$loop"`；
  **增量复审再修**：udevd 会在 loop attach 的 change 事件按**编译默认值（0660）拉回**节点权限（udev(7)
  原文 + /dev/loop0 实测 0660 佐证）——chmod 前加 `sudo udevadm settle`（一行），消除毫秒级竞态
  （否则出口 CI 掷硬币）。
- **I2（去重丢型号名）**：`compose_disk_name` 纯函数（trim 收进函数内）供 parse/open 共用，`open_with_sysfs`
  读 vendor/model → `--device /dev/sda` 行名与枚举一致。真机核到文件级（`/sys/block` 与 `/sys/class/block`
  同符号链接目标；`device/{model,vendor}` 非特权可读）。CI 只覆盖 loop 无 `device/` 的回退分支——
  **M1 出口手测清单加一行：`--device /dev/sdb` → 行名应为 `Samsung SSD 850`**。
- **Minor 1-5**：trap 守卫式清理（`loop=""` + `rm -f img`，防 EXIT trap 非零翻转成功脚本）；env 配对 panic；
  两处 `n==64` 断言；枚举失败 `warn:` 入 stderr；双向 first-wins 去重 + 双开测试。
- **裁量**：多出的 xd-core 去重测试**保留**（行为分支应有守护）→ 计数 **108**；`linux.rs` **534 行**接受。
- **M2 tripwire（收紧版）**：**linux.rs 任何实质改动（netlink 热插拔/卷级）开工前先拆** `linux/mod.rs` +
  `pub use`（保持 `xd_device::linux::{LinuxBlockDevice, BlockEnumerator, RawDisk}` 路径——xd-daemon 依赖）
  + `linux/device.rs`；私有 `is_listable_name` 与其测试同文件搬入子模块，无需放宽可见性。

---

### Task 3: 提权落地件（udev uaccess + polkit + root 参数纵深防御）

**Files:**
- Create: `packaging/udev/71-xiaodun-uaccess.rules`
- Create: `packaging/polkit/com.erik.xiaodun.policy`
- Create: `crates/xd-daemon/src/privcheck.rs`（+main.rs 接入）
- Create: `docs/security/linux-privilege-model.md`（本决策的落点记录）

- [ ] **Step 1: 文件内容（终稿）**

`packaging/udev/71-xiaodun-uaccess.rules`：
```
# 小盾数据恢复：活动会话用户对 USB 存储与 SD 卡免密只读访问。
# 71- 晚于 60-persistent-storage.rules，此时 ENV{ID_BUS} 已由 blkid 填好。
# 注意：uaccess 授予的是 **rw**（内核层面没有"只读 ACL"）；只读铁律由
# 小盾类型系统（BlockDevice 无写方法）+ O_RDONLY 保证，见 docs/security/linux-privilege-model.md。
ACTION=="add|change", SUBSYSTEM=="block", ENV{ID_BUS}=="usb", TAG+="uaccess"
ACTION=="add|change", SUBSYSTEM=="block", KERNEL=="mmcblk[0-9]*", TAG+="uaccess"
```

`packaging/polkit/com.erik.xiaodun.policy`：
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE policyconfig PUBLIC "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN"
                              "http://www.freedesktop.org/standards/PolicyKit/1/policyconfig.dtd">
<policyconfig>
  <vendor>erik.xyz</vendor>
  <action id="com.erik.xiaodun.daemon.run">
    <description>小盾数据恢复：以特权只读访问磁盘</description>
    <message>需要授权以只读方式读取磁盘设备</message>
    <defaults>
      <allow_any>no</allow_any>
      <allow_inactive>auth_admin</allow_inactive>
      <allow_active>auth_admin</allow_active>
      <!-- 安全注（勿改）：pkexec 不校验参数；一旦改为 auth_admin_keep 或 yes，
           攻击者可用 pkexec 以 root 读任意文件（例如影子口令文件）。
           注：本注释体不得出现双连字符（XML 1.0 —— 早期版本曾因此致 polkitd 拒绝解析）。 -->
    </defaults>
    <annotate key="org.freedesktop.policykit.exec.path">/usr/libexec/xiaodun/xd-daemon</annotate>
  </action>
</policyconfig>
```

`crates/xd-daemon/src/privcheck.rs`：
```rust
//! euid==0（pkexec 兜底路径）下的参数纵深防御（第二道；第一道是 polkit 策略 auth_admin 不 keep）。

use std::io;
use std::path::Path;

/// --image 在 root 模式下的准入：必须是普通文件、属主 == PKEXEC_UID、PKEXEC_UID 必须存在。
pub fn check_image_arg(file_uid: u32, is_regular_file: bool, pkexec_uid: Option<u32>) -> Result<(), String> {
    if !is_regular_file {
        return Err("root 模式下 --image 必须是普通文件".into());
    }
    match pkexec_uid {
        Some(p) if p == file_uid => Ok(()),
        Some(p) => Err(format!("root 模式下 --image 属主 {file_uid} 须为调用者 {p}（PKEXEC_UID）")),
        None => Err("root 模式缺少 PKEXEC_UID（非 pkexec 启动？）——拒绝".into()),
    }
}

/// 打开 --image：O_NOFOLLOW（拒符号链接换靶）。
pub fn open_image_no_follow(p: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    const O_NOFOLLOW: i32 = 0o400000; // Linux x86_64 值；见 man 2 open
    std::fs::OpenOptions::new().read(true).custom_flags(O_NOFOLLOW).open(p)
}

/// 当前进程 effective uid（零依赖读 /proc/self/status 的 Uid 行第 2 列）。
pub fn effective_uid() -> Option<u32> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_arg_checks() {
        assert!(check_image_arg(1000, true, Some(1000)).is_ok());
        assert!(check_image_arg(1000, true, Some(0)).is_err());     // 属主不符
        assert!(check_image_arg(1000, true, None).is_err());         // 无 PKEXEC_UID
        assert!(check_image_arg(1000, false, Some(1000)).is_err());  // 非普通文件
    }

    #[test]
    fn effective_uid_matches_process() {
        // 本进程（非 root）解析值应与 `id -u` 一致；root CI 下也成立
        let uid = effective_uid().unwrap();
        let real: u32 = String::from_utf8(
            std::process::Command::new("id").arg("-u").output().unwrap().stdout,
        ).unwrap().trim().parse().unwrap();
        assert_eq!(uid, real);
    }
}
```
main.rs 接入：处理 `--image` 前，若 `effective_uid() == Some(0)` 走：`open_image_no_follow` + metadata(uid, is_file) + `std::env::var("PKEXEC_UID").ok().and_then(|s| s.parse().ok())` → `check_image_arg(...)` 失败则 exit 2 并打印原因。`--device` 在 root 模式下额外断言"是块设备节点"（`LinuxBlockDevice::open` 已内含 NotAFile 拒绝 ✓，无需重复）。

`docs/security/linux-privilege-model.md`（要点）：三方案取舍（A 主/C 兜底/D 不做）、pkexec `auth_admin` 不 keep 的原因（参数不校验 → 本地提权）、uaccess 是 rw 而只读铁律的落点是类型系统 + O_RDONLY、中期硬化（root 只做 open → setuid 回 PKEXEC_UID；rustix/libc 已在 Cargo.lock）。

- [ ] **Step 2: 运行** → daemon 新增 2 测试（xd-daemon 首次有测试）；workspace 全绿
- [ ] **Step 3: 标注**：udev/polkit 的**真机生效**为"未验证（需真机 root）"——E3 出口需手测（装包 → 插 U 盘 → 免密扫）。
- [ ] **Step 4: Commit** `feat(security): udev uaccess + polkit auth_admin 策略与 root 参数纵深防御`

**执行记录（2026-10-02）**：实施提交 `b3688ba`（workspace 110）。偏离 3 条：① privcheck 与调用处
`#[cfg(target_os="linux")]`（跨平台编译必要，Windows 交叉 check 为证）；② main.rs 文件头注释更新；
③ **TOCTOU 残留披露**（O_NOFOLLOW 校验后 `ImageFileDevice::open` 按路径重开；硬约束未动 xd-device；
docs 第 3 节含可达性分析与 M4 硬化路径）。
**超计划验证**：`unshare -r` 造 euid==0 对真二进制 10 例（缺/空 PKEXEC_UID→exit 2、属主匹配→0、不符→2、
ELOOP 40、目录→2、`--device` 不受 root 检查）——CI 覆盖不到的 root 路径由此闭环。

**修复轮（spec-m1e-t3，2026-10-02）**：规格审查 **❌ 1 项（发布级）**——`com.erik.xiaodun.policy:14` 注释体含
`--`（**计划终稿原生缺陷逐字传导**）：XML 1.0 禁止注释体内 `--`，polkitd 实链 expat 会拒绝解析
→ action 不注册 → pkexec 兜底（方案 C）失效（CI 无 xmllint 未拦）。证据链三层独立（xmllint / expat C 探针
REJECTED / 仅改注释的对照 ACCEPTED）。修复提交 `043bb86`：注释改写为无 `--` 文本 + 防回归说明；
docs 可达性措辞改"须诱导真人完成一次认证（auth_admin 是每次调用的一次性授权，非缓存、非口令）"；
全仓 `git ls-files | grep -E '\.(xml|policy|svg)$'` 逐个 `xmllint --noout` 无 BAD。
**门禁增补（T4 打包脚本落实）**：`packaging/` 下 XML 类文件纳入 `xmllint --noout` 校验。
**账（记 M1e）**：XML 注释体是校验盲区——模板文本必须过一遍真实 parser，不能只靠肉眼。

---

### Task 4: deb 打包（裸 dpkg-deb 树）

**Files:**
- Create: `packaging/deb/DEBIAN/control`、`packaging/deb/DEBIAN/postinst`
- Create: `scripts/build-deb.sh`、`scripts/e2e-deb.sh`
- Modify: `.github/workflows/ci.yml`（+手动触发的 `package-deb` job）

- [ ] **Step 1: control/postinst（终稿）**

`packaging/deb/DEBIAN/control`（`@VERSION@` 由 build-deb.sh 注入）：
```
Package: xiaodun
Version: @VERSION@
Architecture: amd64
Maintainer: erik <erik@erik.xyz>
Depends: libc6, libgtk-3-0 | libgtk-3-0t64
Section: utils
Priority: optional
Description: 小盾数据恢复工具
 跨平台数据恢复（M1 Linux 预览版）：FAT/exFAT 扫描与删除文件恢复。
```

`packaging/deb/DEBIAN/postinst`：
```sh
#!/bin/sh
set -e
# 让已插入的块设备重新触发规则（U 盘通常安装后才插，无需此步也可）
if command -v udevadm >/dev/null 2>&1; then
  udevadm trigger --action=change --subsystem-match=block || true
fi
exit 0
```

- [ ] **Step 2: `scripts/build-deb.sh`（完整内容）**

```bash
#!/usr/bin/env bash
# 组装 deb：/usr/libexec/xiaodun/xd-daemon + polkit + udev + （Flutter bundle 到位后）opt/xiaodun。
# 版本号从 workspace Cargo.toml 注入，避免两处手改。
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
[ -n "$version" ] || { echo "FAIL: 未取到版本号"; exit 1; }
echo "packaging xiaodun v$version"

cargo build -q --locked --release -p xd-daemon

stage=$(mktemp -d /tmp/xd-deb-$$-XXXX)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/DEBIAN" "$stage/usr/libexec/xiaodun" \
         "$stage/usr/share/polkit-1/actions" "$stage/usr/lib/udev/rules.d"
cp packaging/deb/DEBIAN/control "$stage/DEBIAN/control"
sed -i "s/@VERSION@/$version/" "$stage/DEBIAN/control"
cp packaging/deb/DEBIAN/postinst "$stage/DEBIAN/postinst"
chmod 755 "$stage/DEBIAN/postinst"
install -m 755 target/release/xd-daemon "$stage/usr/libexec/xiaodun/xd-daemon"
install -m 644 packaging/polkit/com.erik.xiaodun.policy "$stage/usr/share/polkit-1/actions/"
install -m 644 packaging/udev/71-xiaodun-uaccess.rules "$stage/usr/lib/udev/rules.d/"

out=dist/xiaodun_${version}_amd64.deb
mkdir -p dist
dpkg-deb --build --root-owner-group "$stage" "$out"
echo "built: $out"
dpkg-deb -I "$out" | head -12
```
（`/opt/xiaodun` 的 Flutter bundle 由 M1d 产物接入——本脚本留 TODO 注释一句，不阻塞。）

- [ ] **Step 3: `scripts/e2e-deb.sh`（完整内容）**

```bash
#!/usr/bin/env bash
# 容器内 deb 装/卸验证：无 docker 静默跳过。
set -euo pipefail
cd "$(dirname "$0")/.."

command -v docker >/dev/null 2>&1 || { echo "skip: 无 docker"; exit 0; }
bash scripts/build-deb.sh
deb=$(ls -t dist/xiaodun_*_amd64.deb | head -1)

docker run --rm -v "$PWD/dist:/pkg:ro" ubuntu:24.04 bash -c '
  set -e
  apt-get update -qq && apt-get install -y -qq udev policykit-1 >/dev/null
  dpkg -i /pkg/'"$(basename "$deb")"' || apt-get -f install -y -qq
  dpkg -L xiaodun | grep -q /usr/libexec/xiaodun/xd-daemon
  dpkg -L xiaodun | grep -q com.erik.xiaodun.policy
  dpkg -L xiaodun | grep -q 71-xiaodun-uaccess.rules
  /usr/libexec/xiaodun/xd-daemon </dev/null >/dev/null 2>/tmp/banner || true
  grep -q "小盾 xd-daemon" /tmp/banner
  dpkg -r xiaodun
  test ! -e /usr/libexec/xiaodun/xd-daemon
  echo "DEB E2E OK"
'
```

- [ ] **Step 4: ci.yml 新增 `package-deb` job**（`workflow_dispatch` 手动触发，仅 ubuntu-latest）：
`cargo build --release -p xd-daemon` → `bash scripts/build-deb.sh` → `bash scripts/e2e-deb.sh` → 上传 deb 为 artifact。

- [ ] **Step 5: 运行**：本机 `bash scripts/build-deb.sh`（dpkg-deb 可用）产出 dist/*.deb；`bash scripts/e2e-deb.sh` 有 docker 则跑、无则 skip。Commit：`feat(packaging): deb 树与 build/e2e 脚本（daemon 装 /usr/libexec/xiaodun）`

---

### Task 5: M1e 出口验收

- [ ] `cargo test --workspace --locked` 全绿（xd-device +12、xd-core +1、xd-daemon +2）；clippy `-D warnings`、fmt 干净
- [ ] `bash scripts/e2e-loop.sh`：本机 skip（无免密 sudo）；**CI（手动触发）必须 LOOP E2E OK**——未过不得关闭 M1e
- [ ] `bash scripts/build-deb.sh` 产出 deb；`bash scripts/e2e-deb.sh` 本机（有 docker）容器装/卸 OK
- [ ] `strace -f -e trace=openat ./target/debug/xd-daemon </dev/null 2>&1 | grep -E "openat.*(/dev/(sd|nvme|mmc|vd))" ` → **无输出**（device.list 零 open 的机器断言；手测记录入库）
- [ ] `bash scripts/apply-copyright.sh` 幂等（新 .rs 已带头；rules/policy/脚本为配置件不加头）
- [ ] **未验证清单（平台专有，交付标注"未验证"，M1 出口真机手测）**：pkexec 真实认证路径（CI 无 tty 必 127）、
      udev uaccess 真机生效（需装包+插盘）、真 U 盘删除照片全链路、`/dev/sdX` 真实介质行为
- [ ] `provenance.sha256` 重生成——待发布时执行

## 后续切片（各自独立计划）

- **M1b**：契约 v1（含 M1e 提出的 `transport`/`accessible` 字段需求 + EACCES 错误码位）
- **M1d**：UI 侧 `EACCES → pkexec 重启 daemon` 兜底（本计划 E3 的 Dart 半边）
- **M1c**：雕刻 v1
- rpm 打包与热插拔订阅（netlink）→ M2

---

© 2026 erik · https://erik.xyz · erik@erik.xyz
