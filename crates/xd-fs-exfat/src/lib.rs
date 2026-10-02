// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! xd-fs-exfat：exFAT 只读解析与删除文件恢复（只读铁律：无任何写设备路径）。
pub mod bitmap;
pub mod boot;
pub mod dirent;
pub mod fattab;
pub mod freespace;
pub mod read;
pub mod scan;

/// 与 `xd_fs_fat::FatError` 同构（non_exhaustive）。
#[derive(Debug)]
#[non_exhaustive]
pub enum ExfatError {
    /// 引导区/几何/结构非法
    InvalidBoot(String),
    /// 设备读取失败
    Io(String),
}

impl std::fmt::Display for ExfatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExfatError::InvalidBoot(m) => write!(f, "invalid exfat boot: {m}"),
            ExfatError::Io(m) => write!(f, "exfat io: {m}"),
        }
    }
}
impl std::error::Error for ExfatError {}
