// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! xd-fs-ntfs：NTFS 只读解析与删除文件恢复（只读铁律：无任何写设备路径；只依赖
//! `xd_device::BlockDevice`）。
//!
//! **M2 T1 骨架**：Error 类型与三签名就位，v1 实现恒 `Err(Unsupported)`——T2/T3 填实
//! （live 枚举 / `$MFT` 删除恢复 / `$Bitmap` 分级 / 读路径 / 深扫）。骨架**绝不**
//! `unimplemented!()`/`panic!`：本 crate 运行在 worker 的 `catch_unwind` 崩溃隔离边界内，
//! 「未实现」必须是可判定的错误（任务 failed = -32603），不得伪装成崩溃。

use std::ops::Range;

use xd_device::BlockDevice;

/// 与 `xd_fs_fat::FatError`/`xd_fs_exfat::ExfatError` 同构（`non_exhaustive`）。
#[derive(Debug)]
#[non_exhaustive]
pub enum NtfsError {
    /// 引导区/几何/结构非法
    InvalidBoot(String),
    /// 设备读取失败
    Io(String),
    /// 引擎未实现（仅 T1 骨架期；填实后不再产出）
    Unsupported(String),
}

impl std::fmt::Display for NtfsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NtfsError::InvalidBoot(m) => write!(f, "invalid ntfs boot: {m}"),
            NtfsError::Io(m) => write!(f, "ntfs io: {m}"),
            NtfsError::Unsupported(m) => write!(f, "ntfs unsupported: {m}"),
        }
    }
}
impl std::error::Error for NtfsError {}

/// 恢复质量（值域与 M1 两引擎一致；契约串映射在 xd-core 侧）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverQuality {
    /// 全部数据簇经 `$Bitmap` 确证空闲（或 live 项可读）
    Complete,
    /// 有簇已被复用 / 元数据不可判定
    MaybeDamaged,
}

/// 扫描条目：T2 `scan.rs` 的契约映射源（`record_id` = MFT 记录号，契约 `recordId`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtfsEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub first_cluster: u32,
    pub deleted: bool,
    pub is_dir: bool,
    pub quality: RecoverQuality,
    pub ext: String,
    /// MFT 记录号（读取路径以它重定位，与 `first_cluster` 分工）。
    pub record_id: u64,
}

/// 快扫（T2/T3 填实）：从根走目录树产 live 条目 + `$MFT` 全表删除项（`$Bitmap` 分级）。
pub fn scan_with_observer(
    _dev: &dyn BlockDevice,
    _observer: &mut dyn FnMut(&NtfsEntry),
) -> Result<Vec<NtfsEntry>, NtfsError> {
    Err(unsupported())
}

/// 读取 `[offset, offset+length)`（T3 填实）：`record_id` → 重解析 MFT 记录 → `$DATA`
/// （常驻直接切片 / 非常驻 runlist + 稀疏与 VDL 补零）。
pub fn read_file_range(
    _dev: &dyn BlockDevice,
    _record_id: u64,
    _offset: u64,
    _length: u64,
) -> Result<Vec<u8>, NtfsError> {
    Err(unsupported())
}

/// 空闲簇 → 有序不相交字节区间（深扫输入，T3 填实；`$Bitmap` 不可读 ⇒ Err）。
pub fn unallocated_runs(_dev: &dyn BlockDevice) -> Result<Vec<Range<u64>>, NtfsError> {
    Err(unsupported())
}

fn unsupported() -> NtfsError {
    NtfsError::Unsupported("ntfs engine is not implemented yet (M2 T1 skeleton)".into())
}
