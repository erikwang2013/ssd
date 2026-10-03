// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 块设备抽象：M0 仅实现只读镜像文件后端。

pub mod image;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

use serde::{Deserialize, Serialize};

/// 设备信息（IPC 契约类型，JSON 用 camelCase，见 proto/v0/README.md 与 proto/v1/README.md）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub size_bytes: u64,
    pub removable: bool,
    pub fs_guess: Option<String>,
    /// 运输类型（v1）：`usb|mmc|nvme|sata|virtio|other`；缺失 = 未知（镜像恒缺），
    /// 序列化省略 null 键——v0 golden 与既有输出不受影响。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Physical,
    Volume,
    Image,
}

#[derive(Debug)]
pub enum DeviceError {
    Io(std::io::Error),
    NotAFile(String),
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceError::Io(e) => write!(f, "io error: {e}"),
            DeviceError::NotAFile(p) => write!(f, "not a regular file: {p}"),
        }
    }
}

impl std::error::Error for DeviceError {}

impl From<std::io::Error> for DeviceError {
    fn from(e: std::io::Error) -> Self {
        DeviceError::Io(e)
    }
}

/// 只读块设备：没有任何写接口，只读铁律由类型系统保证。
pub trait BlockDevice: Send + Sync {
    fn info(&self) -> &DeviceInfo;

    fn size_bytes(&self) -> u64 {
        self.info().size_bytes
    }

    /// 从 offset 起读取直到填满 buf、到达 EOF 或出错；返回实际读取字节数。
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError>;

    /// 源为物理块设备时返回节点 `st_rdev` 的 (major, minor)；镜像/未知恒 `None`。
    /// 默认实现 `None`——既有实现零改动（导出目标同盘校验用；镜像不做该校验，
    /// 见 `xd_core::export::check_target`）。
    fn source_rdev(&self) -> Option<(u64, u64)> {
        None
    }
}

/// unix 系共用的源设备 rdev（`linux.rs` 与 `macos.rs` 各自 `impl` 都调这里）：
/// fstat **已打开的 fd**（不重开路径、无写路径——只读铁律不破）→ `st_rdev` → (major, minor)。
///
/// 解码恒用 glibc 式（[`dev_major_minor`]，与 `xd-core::export` 的本地副本 `major_minor` 同式）：
/// 同盘校验 -32006 是 `st_dev(目标) == st_rdev(源)` 的**相等比较**，两侧必须同解码才不失效——
/// macOS 的真实 dev_t 布局（Darwin：major 高 8 位 / minor 低 24 位）与此不同，但确定性解码不改变
/// 相等判定；换 Darwin 解码而 xd-core 侧不改，会让 -32006 在 macOS 上**静默失效**（见 macos.rs 头注）。
#[cfg(unix)]
pub(crate) fn source_rdev_of(file: &std::fs::File) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some(dev_major_minor(file.metadata().ok()?.rdev()))
}

/// glibc `dev_t` 解码（gnu_dev_major/minor）。跨 unix 共用——`xd-core` 有同式本地副本
/// （`export.rs::major_minor`，那份需跨 unix 编译、不能依赖本 crate 的 linux 模块）。
#[cfg(unix)]
pub(crate) fn dev_major_minor(dev: u64) -> (u64, u64) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major, minor)
}
