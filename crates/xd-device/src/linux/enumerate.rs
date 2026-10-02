// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! sysfs 枚举/归类层：`BlockEnumerator`、`RawDisk`/`Transport` 与 transport 映射。
//! 自 `linux.rs` 机械迁出（T7，纯重构零行为变化）；公开路径经 `linux.rs` 的 `pub use`
//! 重导出，`xd_device::linux::*` 不变。
//!
//! 设计约束（M1e 简报，本机实测）：
//! - `device.list` 零 `open()`：枚举只读 sysfs（普通权限即可工作）。
//! - 虚拟设备（loop/ram/zram/dm/md/nbd）canonicalize 后以 `/sys/devices/virtual/` 开头，跳过。
//! - `removable` 不可作过滤（USB-SATA 硬盘盒报 0）；运输类型仅作分组提示。

use crate::{DeviceError, DeviceInfo, DeviceKind};
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
            transport: Some(transport_str(self.transport).to_string()),
        }
    }
}

/// sysfs class 目录 → 契约 transport 字符串；解析/归类失败 → None（契约：缺失=未知）。
/// `--device` 路径（open_with_sysfs）与 device.list 同源归类——同一块盘两处行不再分歧。
pub(crate) fn sysfs_transport(sysfs_root: &Path, kernel_name: &str) -> Option<String> {
    let real = std::fs::canonicalize(sysfs_root.join("class/block").join(kernel_name)).ok()?;
    Some(transport_str(classify_transport(&real.to_string_lossy())).to_string())
}

/// 运输类型 → 契约字符串（值域见 proto/v1/README.md）。
pub(crate) fn transport_str(t: Transport) -> &'static str {
    match t {
        Transport::Usb => "usb",
        Transport::Mmc => "mmc",
        Transport::Nvme => "nvme",
        Transport::Virtio => "virtio",
        Transport::Sata => "sata",
        Transport::Other => "other",
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
pub(crate) fn compose_disk_name(vendor: &str, model: &str, kernel_name: &str) -> String {
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
/// `pub(crate)`：`linux.rs` 白盒测试直接钉形态（公开面不变）。
pub(crate) fn is_listable_name(name: &str) -> bool {
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
