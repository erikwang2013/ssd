// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! FAT12/16/32 只读解析：快速扫描（删除文件找回）与文件读取。
//! 全部输入经 `xd_device::BlockDevice`（任意偏移只读），零平台耦合。

pub mod bpb;
pub mod dirent;
pub mod fat;

/// 引擎统一错误类型。
#[derive(Debug)]
#[non_exhaustive]
pub enum FatError {
    Device(xd_device::DeviceError),
    InvalidBpb(String),
}

impl std::fmt::Display for FatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FatError::Device(e) => write!(f, "device error: {e}"),
            FatError::InvalidBpb(m) => write!(f, "invalid fat bpb: {m}"),
        }
    }
}

impl std::error::Error for FatError {}

impl From<xd_device::DeviceError> for FatError {
    fn from(e: xd_device::DeviceError) -> Self {
        FatError::Device(e)
    }
}
