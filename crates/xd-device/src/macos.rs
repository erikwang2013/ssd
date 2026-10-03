// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! macOS 物理块设备：只读整盘句柄（`/dev/diskN`）+ 枚举层。
//!
//! **未验证（需真机）**：本模块全部平台路径——CI macOS runner 只证「可编译 + 枚举/打开冒烟」
//! （`tests/macos_smoke.rs`：枚举 ≥1 硬断言；打开/读 512B 弱断言，PermissionDenied 接受）；
//! 真机 root + 完全磁盘访问（FDA/TCC）下的打开/读取与导出同盘校验归 M1 出口手测（见 docs/security §9）。
//!
//! 与 `linux.rs` 的对应与差异（计划 Task 3）：
//! - 枚举 `/dev/disk[0-9]+`（`read_dir` + 形态解析；**只要整盘**：分区 `diskXsY`、裸盘 `rdiskX` 不列）。
//! - 容量必须经 fd：`libc::ioctl` 的 `DKIOCGETBLOCKSIZE × DKIOCGETBLOCKCOUNT`（libc 未导出这两个
//!   常量 ⇒ 按 Darwin `_IOC` 编码常量求值，值钉单测）⇒ 每盘一次只读 open（同 Windows 口径，与
//!   Linux「零 open() 只读 sysfs」不同）。无权限时该盘**仍列入**、size 记 0 + stderr 留痕
//!   （`/dev/diskN` 为 `brw-r----- root:operator`：普通用户 EACCES；跳盘会让普通用户上下文
//!   `device.list` 全空，CI 冒烟的枚举硬断言也随之不可达）。
//! - transport 恒 `None`、removable 恒 `false`：M1 不引 IOKit/`diskutil`（计划裁定），M2 补。
//! - 打开经 `rustix::fs::open`（`O_RDONLY|O_CLOEXEC|O_NONBLOCK`；无任何写位）。FDA 缺失（EPERM）/
//!   非 operator 组普通用户（EACCES）→ `ErrorKind::PermissionDenied`（daemon 侧提示
//!   「需在系统设置授权完全磁盘访问」）。
//! - `source_rdev` 复用 `crate::source_rdev_of`（unix 共用 glibc 式解码）：与 `xd-core::export`
//!   的目标侧解码同式 ⇒ 精快路径 `st_dev(目标) == st_rdev(源)` 的**相等语义**成立（解码出的数字
//!   非 Darwin major/minor 语义，但确定性解码不改变相等判定；换 Darwin 解码而 xd-core 侧不改，
//!   会让 -32006 在 macOS 静默失效）。盘级祖先第二道走 sysfs，macOS 无 `/sys` ⇒ 恒不可用
//!   → fail-open + 留痕（§6 语义），只剩精快路径。
//! - 只读铁律落点：`O_RDONLY`（无写位）+ 类型系统无写方法（同 Linux）。

use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};

use crate::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};

// ---- ioctl 与容量 ----

/// Darwin `_IOC` 编码（`<sys/ioccom.h>`）：`IOC_OUT | (len & 0x1fff) << 16 | group << 8 | num`，
/// IOC_OUT = 0x4000_0000。
const fn ior(group: u8, num: u8, len: libc::c_ulong) -> libc::c_ulong {
    0x4000_0000 | ((len & 0x1fff) << 16) | ((group as libc::c_ulong) << 8) | (num as libc::c_ulong)
}

/// `<sys/disk.h>`（XNU）：`DKIOCGETBLOCKSIZE = _IOR('d', 24, u_int32_t)`、
/// `DKIOCGETBLOCKCOUNT = _IOR('d', 25, u_int64_t)`。libc 未导出这两个常量 ⇒ 按公式常量求值；
/// 头文件值（0x4004_6418 / 0x4008_6419）钉在单测，防公式或数字手滑。
const DKIOCGETBLOCKSIZE: libc::c_ulong = ior(b'd', 24, 4);
const DKIOCGETBLOCKCOUNT: libc::c_ulong = ior(b'd', 25, 8);

/// 经 fd 读一个 ioctl 出参（`_IOR` 约定：第三参为出参指针）。未验证（需真机）。
fn ioctl_read<T: Default>(fd: RawFd, request: libc::c_ulong) -> Result<T, DeviceError> {
    // 宽度配对护栏：request 的 _IOC 长度字段必须 == T 字节宽（调用点互换/手滑在 debug/CI 立即红；
    // 内核按声明宽度写，配错即栈越界）。
    debug_assert_eq!(
        ((request >> 16) & 0x1fff) as usize,
        std::mem::size_of::<T>(),
        "ioctl 请求宽度与出参类型不匹配"
    );
    let mut v = T::default();
    // SAFETY: request 为 DKIOC* 常量，内核按 _IOR 声明的宽度全量写入 v（u32/u64 均为 Copy 类型，
    // 写满后读取无未初始化字节）；fd 由调用方保证为有效只读 fd。
    let rc = unsafe { libc::ioctl(fd, request, &mut v as *mut T) };
    if rc == -1 {
        return Err(DeviceError::Io(std::io::Error::last_os_error()));
    }
    Ok(v)
}

/// 整盘容量 = `DKIOCGETBLOCKSIZE`(u32) × `DKIOCGETBLOCKCOUNT`(u64)。未验证（需真机）。
fn disk_size_bytes(file: &File) -> Result<u64, DeviceError> {
    let fd = file.as_raw_fd();
    let block_size: u32 = ioctl_read(fd, DKIOCGETBLOCKSIZE)?;
    let count: u64 = ioctl_read(fd, DKIOCGETBLOCKCOUNT)?;
    Ok((block_size as u64).saturating_mul(count))
}

/// 只读打开节点：`O_RDONLY`（rustix 默认带 `O_CLOEXEC`，这里显式重申）+ `O_NONBLOCK`
/// （防 `--device <fifo/异常节点>` 卡死在 open；块设备忽略该位——同 linux.rs 口径）。
/// FDA 缺失 → EPERM；无 operator 组（普通用户）→ EACCES；两者均为 `PermissionDenied`。未验证（需真机）。
fn open_readonly(node: &Path) -> Result<File, DeviceError> {
    let fd = rustix::fs::open(
        node,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|e| DeviceError::Io(e.into()))?;
    Ok(File::from(fd))
}

// ---- 形态解析（信任边界）----

/// 整盘名形态 `disk<纯数字>`（正则式解析，不引 regex crate）。
/// 分区（`disk0s1`/`disk0s1s2`）、裸盘（`rdisk0`）、非数字（`diskX`）一律不是——M1 只要整盘。
pub(crate) fn is_whole_disk_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("disk") else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

/// `--device` 信任边界：仅接受整盘节点 `/dev/disk<N>`（父目录必须恰为 `/dev`）——
/// 分区/裸盘/任意路径拒绝，与 Linux「必须是块设备节点」、Windows `parse_physical_drive_path` 同口径。
pub(crate) fn parse_disk_node(path: &Path) -> Option<u32> {
    if path.parent()? != Path::new("/dev") {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    if !is_whole_disk_name(name) {
        return None;
    }
    name.strip_prefix("disk")?.parse().ok()
}

// ---- 枚举 ----

/// 枚举到的整盘：`node` = `/dev/diskN`（`info.id` 为同源 `unix:` 前缀 id，与 Linux 语法一致）。
#[derive(Debug, Clone)]
pub struct MacosDisk {
    pub info: DeviceInfo,
    pub node: PathBuf,
}

/// 枚举整盘（`/dev/diskN`，盘号升序）。**列表不依赖权限**：容量查询需 open（见模块头注），
/// 无权限（EACCES/EPERM）时该盘仍列入、`size_bytes` 记 0 并在 stderr 留痕——容量**成功读到 0**
/// 的盘跳过（无媒体/幻影盘，同 Linux `parse_entry_zero_size_is_none` 与 Windows 零盘跳过口径）。
/// 未验证（需真机）；CI macOS runner 实跑 `tests/macos_smoke.rs`。
pub fn enumerate() -> Result<Vec<MacosDisk>, DeviceError> {
    enumerate_in(Path::new("/dev"))
}

/// 注入根版（单测用假 /dev；生产恒 `/dev`）。
pub(crate) fn enumerate_in(dev_dir: &Path) -> Result<Vec<MacosDisk>, DeviceError> {
    enumerate_in_with(dev_dir, &|node| {
        open_readonly(node).and_then(|f| disk_size_bytes(&f))
    })
}

/// 探针注入版：`Ok(0)`=无媒体/幻影盘跳过；`Ok(n)` 直通；`Err`=仍列入、size 记 0 + 留痕
/// （容量策略由此单测钉死，不依赖载体文件类型；生产探针 = 只读 open + DKIOC ioctl）。
pub(crate) fn enumerate_in_with(
    dev_dir: &Path,
    probe_size: &dyn Fn(&Path) -> Result<u64, DeviceError>,
) -> Result<Vec<MacosDisk>, DeviceError> {
    let entries = std::fs::read_dir(dev_dir).map_err(DeviceError::Io)?;
    let mut found: Vec<(u32, MacosDisk)> = Vec::new();
    for e in entries.flatten() {
        let Some(name) = e.file_name().to_str().map(String::from) else {
            continue; // 非 UTF-8 名不可能是 disk<数字>
        };
        if !is_whole_disk_name(&name) {
            continue; // 分区/裸盘/其他一律不列
        }
        let node = dev_dir.join(&name);
        let size_bytes = match probe_size(&node) {
            Ok(0) => continue, // 无媒体/幻影盘（零盘跳过口径）
            Ok(size) => size,
            Err(err) => {
                eprintln!(
                    "warn: 无法读取 {node:?} 容量（无权限或无媒体；该盘仍列入，size=0）: {err}"
                );
                0
            }
        };
        // is_whole_disk_name 已保证纯数字；溢出错（disk99999999999）在此兜底跳过
        let Some(number) = name.strip_prefix("disk").and_then(|d| d.parse().ok()) else {
            continue;
        };
        found.push((
            number,
            MacosDisk {
                info: DeviceInfo {
                    id: format!("unix:{}", node.display()),
                    name: name.clone(),
                    kind: DeviceKind::Physical,
                    size_bytes,
                    removable: false, // M1 无 IOKit（M2 补 kIOMediaRemovable）
                    fs_guess: None,   // 同 Linux/Windows：枚举不做 FS 探测
                    transport: None,  // M1 恒 None（计划 Task 3：不引 diskutil/IOKit；M2 用 IOKit）
                },
                node,
            },
        ));
    }
    found.sort_by_key(|(number, _)| *number); // 盘号序（disk2 < disk10；字符串序会错）
    Ok(found.into_iter().map(|(_, d)| d).collect())
}

// ---- 只读整盘句柄 ----

/// 只读整盘句柄（`O_RDONLY`；类型系统无写方法）。未验证（需真机）。
pub struct MacosBlockDevice {
    info: DeviceInfo,
    file: File,
}

impl MacosBlockDevice {
    /// 打开整盘 `/dev/diskN`（信任边界：分区/裸盘/任意路径拒绝）。未验证（需真机）。
    /// FDA 缺失（EPERM）/非 operator 组普通用户（EACCES）→ `Io(PermissionDenied)`。
    pub fn open(node: &Path) -> Result<Self, DeviceError> {
        if parse_disk_node(node).is_none() {
            return Err(DeviceError::NotAFile(format!(
                "macOS 仅支持整盘节点 /dev/diskN: {}",
                node.display()
            )));
        }
        let file = open_readonly(node)?;
        let size_bytes = disk_size_bytes(&file)?;
        // 规范路径（与 linux.rs 同口径）：id 与枚举项必须逐字一致（T4 去重依赖）
        let canon = std::fs::canonicalize(node).unwrap_or_else(|_| node.to_path_buf());
        let name = canon
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| canon.display().to_string());
        Ok(Self {
            info: DeviceInfo {
                id: format!("unix:{}", canon.display()),
                name,
                kind: DeviceKind::Physical,
                size_bytes,
                removable: false, // M1 无 IOKit（M2 补）
                fs_guess: None,
                transport: None, // M1 恒 None（M2 用 IOKit）
            },
            file,
        })
    }
}

impl BlockDevice for MacosBlockDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        // 同 Linux 口径：读取直到填满 buf 或到达 EOF；块设备短读常见，需循环补齐（pread 无游标）。
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

    /// 覆写：fstat **已打开的 fd**（不重开路径、无写路径——只读铁律不破），取 `st_rdev`。
    /// 与 Linux 共用 `crate::source_rdev_of`（同式解码——相等语义见模块头注）。
    fn source_rdev(&self) -> Option<(u64, u64)> {
        crate::source_rdev_of(&self.file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dkioc_request_codes_match_xnu_header_values() {
        // <sys/disk.h>：_IOR('d',24,u32)/_IOR('d',25,u64)；头文件值如下（防公式/数字手滑）
        assert_eq!(DKIOCGETBLOCKSIZE, 0x4004_6418);
        assert_eq!(DKIOCGETBLOCKCOUNT, 0x4008_6419);
    }

    #[test]
    fn whole_disk_name_filter() {
        assert!(is_whole_disk_name("disk0"));
        assert!(is_whole_disk_name("disk12"));
        for bad in ["disk", "diskX", "disk0s1", "disk0s1s2", "rdisk0", "sd0", ""] {
            assert!(!is_whole_disk_name(bad), "应拒绝: {bad}");
        }
    }

    #[test]
    fn parse_disk_node_accepts_only_dev_whole_disks() {
        assert_eq!(parse_disk_node(Path::new("/dev/disk0")), Some(0));
        assert_eq!(parse_disk_node(Path::new("/dev/disk15")), Some(15));
        for bad in [
            "/dev/disk0s1",         // 分区
            "/dev/rdisk0",          // 裸盘
            "/dev/disk",            // 无盘号
            "/dev/diskX",           // 非数字
            "/tmp/disk0",           // 非 /dev（信任边界）
            "disk0",                // 相对路径
            "/dev/disk99999999999", // 超出 u32
            "/dev/sub/disk0",       // /dev 子目录（父目录必须恰为 /dev，非 starts_with）
            "/dev/../dev/disk0",    // 含 .. 段（拒绝即可，无需归一）
        ] {
            assert_eq!(parse_disk_node(Path::new(bad)), None, "应拒绝: {bad}");
        }
    }

    #[test]
    fn open_rejects_non_disk_node_paths() {
        for bad in ["/tmp/x", "/dev/disk0s1", "/dev/rdisk0", "/dev/disk"] {
            assert!(
                matches!(
                    MacosBlockDevice::open(Path::new(bad)),
                    Err(DeviceError::NotAFile(_))
                ),
                "应拒绝: {bad}"
            );
        }
    }

    #[test]
    fn read_at_fills_buf_stops_at_eof_and_errors() {
        // 载体常规文件（macOS CI 真跑）：偏移推进/EOF 短读/纯 EOF/错误四态覆盖 pread 循环
        let info = DeviceInfo {
            id: "unix:/dev/disk0".into(),
            name: "disk0".into(),
            kind: DeviceKind::Physical,
            size_bytes: 600,
            removable: false,
            fs_guess: None,
            transport: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, vec![7u8; 600]).unwrap();
        let dev = MacosBlockDevice {
            info: info.clone(),
            file: File::open(&path).unwrap(),
        };
        let mut buf = [0u8; 512];
        assert_eq!(dev.read_at(0, &mut buf).unwrap(), 512); // 读满
        assert!(buf.iter().all(|b| *b == 7));
        let mut tail = [0u8; 512];
        assert_eq!(dev.read_at(512, &mut tail).unwrap(), 88); // 偏移推进 + EOF 前短读
        let mut none = [0u8; 16];
        assert_eq!(dev.read_at(600, &mut none).unwrap(), 0); // 纯 EOF → 0，不挂死
        let dirdev = MacosBlockDevice {
            info,
            file: File::open(dir.path()).unwrap(),
        };
        assert!(dirdev.read_at(0, &mut none).is_err()); // 目录 fd 读报错 → Err（吞错变异在此被杀）
    }

    #[test]
    fn disk_size_on_non_device_is_err_not_panic() {
        // 常规文件上 DKIOC ioctl 必失败（ENOTTY）→ Err（不 panic、不假造容量）
        let f = tempfile::NamedTempFile::new().unwrap();
        let file = File::open(f.path()).unwrap();
        assert!(matches!(disk_size_bytes(&file), Err(DeviceError::Io(_))));
    }

    #[test]
    fn enumerate_in_lists_whole_disks_only_and_survives_unreadable_size() {
        let root = tempfile::tempdir().unwrap();
        for name in ["disk0", "disk10", "disk2", "disk0s1", "rdisk0", "diskX"] {
            std::fs::write(root.path().join(name), b"").unwrap();
        }
        let disks = enumerate_in_with(root.path(), &|_| {
            Err(DeviceError::Io(std::io::Error::other("ENOTTY")))
        })
        .unwrap();
        let names: Vec<&str> = disks
            .iter()
            .map(|d| d.node.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["disk0", "disk2", "disk10"]); // 盘号序，非字符串序
        // 容量探针必失败 → size 0 且仍列入（列表不依赖权限/可读性）
        assert!(disks.iter().all(|d| d.info.size_bytes == 0));
        assert!(
            disks
                .iter()
                .all(|d| d.info.id == format!("unix:{}", d.node.display()))
        );
        assert!(disks.iter().all(|d| d.info.kind == DeviceKind::Physical));
        assert!(
            disks
                .iter()
                .all(|d| d.info.transport.is_none() && !d.info.removable)
        );
    }

    #[test]
    fn zero_size_disk_is_skipped_and_size_passes_through() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("disk0"), b"").unwrap();
        std::fs::write(root.path().join("disk1"), b"").unwrap();
        let disks = enumerate_in_with(root.path(), &|p| {
            if p.ends_with("disk0") {
                Ok(0)
            } else {
                Ok(1024)
            }
        })
        .unwrap();
        assert_eq!(disks.len(), 1, "容量读到 0 的盘跳过");
        assert!(disks[0].node.ends_with("disk1"));
        assert_eq!(disks[0].info.size_bytes, 1024, "容量直通不折半");
    }
}
