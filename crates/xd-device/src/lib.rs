//! 块设备抽象：M0 仅实现只读镜像文件后端。

pub mod image;

use serde::{Deserialize, Serialize};

/// 设备信息（IPC 契约类型，JSON 用 camelCase，见 proto/v0/README.md）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub size_bytes: u64,
    pub removable: bool,
    pub fs_guess: Option<String>,
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
}
