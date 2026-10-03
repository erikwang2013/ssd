// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! xd-fs-ext4：ext4 只读解析与删除恢复（只读铁律：无任何写设备路径；只依赖
//! `xd_device::BlockDevice`）。
//!
//! **M2 T1 骨架**：Error 类型与三签名就位，v1 实现恒 `Err(Unsupported)`——T4/T5 填实
//! （几何/inode/目录树/extent 读/深扫 / jbd2 journal 删除恢复）。骨架**绝不**
//! `unimplemented!()`/`panic!`：本 crate 运行在 worker 的 `catch_unwind` 崩溃隔离边界内，
//! 「未实现」必须是可判定的错误（任务 failed = -32603），不得伪装成崩溃。

use std::ops::Range;

use xd_device::BlockDevice;

/// 与 `xd_fs_fat::FatError`/`xd_fs_exfat::ExfatError` 同构（`non_exhaustive`）。
#[derive(Debug)]
#[non_exhaustive]
pub enum Ext4Error {
    /// 超级块/几何/结构非法（含诚实拒绝的特征位：bigalloc/encrypt/inline_data/meta_bg/无 extent）
    InvalidBoot(String),
    /// 设备读取失败
    Io(String),
    /// 引擎未实现（仅 T1 骨架期；填实后不再产出）
    Unsupported(String),
}

impl std::fmt::Display for Ext4Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ext4Error::InvalidBoot(m) => write!(f, "invalid ext4 boot: {m}"),
            Ext4Error::Io(m) => write!(f, "ext4 io: {m}"),
            Ext4Error::Unsupported(m) => write!(f, "ext4 unsupported: {m}"),
        }
    }
}
impl std::error::Error for Ext4Error {}

/// 恢复质量（值域与 M1 两引擎一致；契约串映射在 xd-core 侧）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverQuality {
    /// extent 覆盖的块全部空闲（或 live 项可读）
    Complete,
    /// 有块已被复用 / 元数据不可判定
    MaybeDamaged,
}

/// 扫描条目：T4/T5 的契约映射源（`record_id` = inode 号，契约 `recordId`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ext4Entry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    /// 首个 extent 的起始块号（无 extent/洞 ⇒ 0）
    pub first_cluster: u32,
    pub deleted: bool,
    pub is_dir: bool,
    pub quality: RecoverQuality,
    pub ext: String,
    /// inode 号（读取路径以它重定位，与 `first_cluster` 分工）。
    pub record_id: u64,
}

/// 快扫（T4/T5 填实）：从根 inode(2) 走目录树产 live 条目 + journal 覆盖层删除恢复。
pub fn scan_with_observer(
    _dev: &dyn BlockDevice,
    _observer: &mut dyn FnMut(&Ext4Entry),
) -> Result<Vec<Ext4Entry>, Ext4Error> {
    Err(unsupported())
}

/// 读取 `[offset, offset+length)`（T4 填实）：`record_id`（inode 号）→ extent（depth 0/1）
/// → 读；洞/未初始化 extent 补零，`i_size` 截断。
pub fn read_file_range(
    _dev: &dyn BlockDevice,
    _record_id: u64,
    _offset: u64,
    _length: u64,
) -> Result<Vec<u8>, Ext4Error> {
    Err(unsupported())
}

/// 空闲块 → 有序不相交字节区间（深扫输入，T4 填实；位图不可读 ⇒ Err）。
pub fn unallocated_runs(_dev: &dyn BlockDevice) -> Result<Vec<Range<u64>>, Ext4Error> {
    Err(unsupported())
}

fn unsupported() -> Ext4Error {
    Ext4Error::Unsupported("ext4 engine is not implemented yet (M2 T1 skeleton)".into())
}
