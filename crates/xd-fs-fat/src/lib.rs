// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! FAT12/16/32 只读解析：快速扫描（删除文件找回）与文件读取。
//! 全部输入经 `xd_device::BlockDevice`（任意偏移只读），零平台耦合。

pub mod bpb;
pub mod dirent;
pub mod fat;
pub mod freespace;
pub mod read;
pub mod scan;

#[cfg(test)]
pub(crate) mod testutil {
    use xd_device::image::ImageFileDevice;

    /// 镜像字节 → 落盘临时文件 + 只读镜像设备（scan/read 测试共用；与 xd-fs-exfat 同名助手同构）。
    pub(crate) fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(image).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        (f, dev)
    }
}

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
