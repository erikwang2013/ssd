// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! Windows 物理块设备：只读物理盘句柄（`\\.\PhysicalDriveN`）+ 句柄查询层。
//! 枚举/归类层（`enumerate`、`WindowsDisk` 及全部纯函数与单测）在 `windows/enumerate.rs`
//! （T2 收口机械拆分：纯重构零行为变化；公开路径 `xd_device::windows::*` 经本文件重导出，不变）。
//!
//! **未验证（需真机）**：本模块全部平台路径——CI windows runner 只证「可编译 + 枚举/读冒烟」
//! （`tests/windows_smoke.rs`），真机 UAC 提权链/换盘热插拔归 M1 出口手测（见 docs/security §8）。
//!
//! 与 `linux.rs` 的对应与差异：
//! - 枚举需**每盘一次只读 open**（盘号/大小/BusType 只能经句柄查询：`IOCTL_DISK_GET_LENGTH_INFO`
//!   的 CTL_CODE 带 `FILE_READ_ACCESS`）⇒ Windows 枚举需管理员权限，与 Linux「零 open() 只读
//!   sysfs」不同；已知限制与 M2 方向见 docs/security §8。
//! - 只读铁律落点：desired access 仅 `GENERIC_READ`（全文件无任何写 API）；类型系统同款无写方法。
//! - `source_rdev` 不覆写（默认 `None`）：Windows 无 `st_rdev` ⇒ 导出同盘校验（-32006）在
//!   Windows 上跳过——**已知缺口**，M2 用卷句柄卷号比较补（docs/security §8）。
//! - transport 取 `STORAGE_DEVICE_DESCRIPTOR.BusType`（usb/sata/nvme/other）；removable 口径
//!   = BusType Usb/1394/Sd（计划 Task 2）。

use std::sync::Mutex;

use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_BEGIN, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    SetFilePointerEx,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    GET_LENGTH_INFORMATION, IOCTL_DISK_GET_LENGTH_INFO, IOCTL_STORAGE_GET_DEVICE_NUMBER,
    IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR,
    STORAGE_DEVICE_NUMBER, STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
};

use crate::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};

mod enumerate;
pub use self::enumerate::{WindowsDisk, enumerate};
use self::enumerate::{
    bus_transport, compose_name, descriptor_str, parse_physical_drive_path, wide,
};

/// `STORAGE_DEVICE_DESCRIPTOR` 是变长结构（定长头 + vendor/product ASCII）；
/// 1024 字节覆盖定长头与全部常见字符串字段。
const DESCRIPTOR_BUF_BYTES: usize = 1024;

/// Owned 句柄（`Drop` 即关）。枚举与 [`WindowsBlockDevice`] 共用。
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄唯一所有权在此；Drop 只发生一次。
        unsafe { CloseHandle(self.0) };
    }
}

/// 只读打开（desired access 仅 `GENERIC_READ` + `FILE_SHARE_READ|WRITE` + `OPEN_EXISTING`；
/// **不加** `FILE_FLAG_NO_BUFFERING`——M1 带缓冲读，见计划 Task 2）。未验证（需真机）。
fn open_readonly(path: &str) -> Result<OwnedHandle, DeviceError> {
    let name = wide(path);
    // SAFETY: name 以 NUL 结尾且存活至调用结束；属性/模板句柄按文档化用法传 null。
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(DeviceError::Io(std::io::Error::last_os_error()));
    }
    Ok(OwnedHandle(h))
}

/// `DeviceIoControl` 薄包装（只读查询；入缓冲可空）。
///
/// SAFETY（调用方义务）：`out` 可写且至少 `out_len` 字节；`input` 在 `input_len > 0` 时指向
/// 至少 `input_len` 字节的有效内存（为 0 时可传 null）；`handle` 存活。
fn ioctl(
    handle: HANDLE,
    code: u32,
    input: *const std::ffi::c_void,
    input_len: u32,
    out: *mut u8,
    out_len: u32,
) -> Result<(), DeviceError> {
    let mut returned = 0u32;
    // SAFETY: 由调用方保证指针/长度有效；overlapped 传 null = 同步查询。
    let ok = unsafe {
        DeviceIoControl(
            handle,
            code,
            input,
            input_len,
            out.cast(),
            out_len,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(DeviceError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// 盘号：`IOCTL_STORAGE_GET_DEVICE_NUMBER`（`FILE_ANY_ACCESS`，无需读权限即可查询）。
fn device_number(handle: HANDLE) -> Result<u32, DeviceError> {
    let mut n = STORAGE_DEVICE_NUMBER {
        DeviceType: 0,
        DeviceNumber: 0,
        PartitionNumber: 0,
    };
    ioctl(
        handle,
        IOCTL_STORAGE_GET_DEVICE_NUMBER,
        std::ptr::null(),
        0,
        (&mut n as *mut STORAGE_DEVICE_NUMBER).cast(),
        std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
    )?;
    Ok(n.DeviceNumber)
}

/// 盘容量：`IOCTL_DISK_GET_LENGTH_INFO`（CTL_CODE 带 `FILE_READ_ACCESS` ⇒ 需只读句柄）。
fn query_length(handle: HANDLE) -> Result<u64, DeviceError> {
    let mut info = GET_LENGTH_INFORMATION { Length: 0 };
    ioctl(
        handle,
        IOCTL_DISK_GET_LENGTH_INFO,
        std::ptr::null(),
        0,
        (&mut info as *mut GET_LENGTH_INFORMATION).cast(),
        std::mem::size_of::<GET_LENGTH_INFORMATION>() as u32,
    )?;
    Ok(info.Length.max(0) as u64)
}

/// 总线与型号（一条 `STORAGE_QUERY_PROPERTY(StorageDeviceProperty)` 全取）。
struct BusDescriptor {
    /// `STORAGE_BUS_TYPE`（i32，见 `Win32::Storage::FileSystem` 的 `BusType*` 常量）。
    bus: i32,
    vendor: String,
    product: String,
}

fn query_descriptor(handle: HANDLE) -> Result<BusDescriptor, DeviceError> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    // 8 字节对齐缓冲（设备驱动按字节写回；`Vec<u8>` 无对齐保证，用 u64 视图）。
    let mut buf = vec![0u64; DESCRIPTOR_BUF_BYTES / 8];
    ioctl(
        handle,
        IOCTL_STORAGE_QUERY_PROPERTY,
        (&query as *const STORAGE_PROPERTY_QUERY).cast(),
        std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
        buf.as_mut_ptr().cast(),
        DESCRIPTOR_BUF_BYTES as u32,
    )?;
    // SAFETY: 驱动已按字节填满 buf；字节视图长度与分配一致（len*8 == DESCRIPTOR_BUF_BYTES）。
    let bytes =
        unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), DESCRIPTOR_BUF_BYTES) };
    // SAFETY: 定长头 ≤ 缓冲；read_unaligned 免对齐假设（缓冲本已 8 对齐，留纵深）。
    let desc =
        unsafe { std::ptr::read_unaligned(buf.as_ptr().cast::<STORAGE_DEVICE_DESCRIPTOR>()) };
    Ok(BusDescriptor {
        bus: desc.BusType,
        vendor: descriptor_str(bytes, desc.VendorIdOffset),
        product: descriptor_str(bytes, desc.ProductIdOffset),
    })
}

/// 句柄 → `DeviceInfo`（size 必得；transport/型号查询失败降级，不失败打开）。
/// `number` 为 `IOCTL_STORAGE_GET_DEVICE_NUMBER` 盘号。
fn info_from_handle(handle: HANDLE, number: u32) -> Result<DeviceInfo, DeviceError> {
    let size_bytes = query_length(handle)?;
    let path = format!(r"\\.\PhysicalDrive{number}");
    let (transport, removable, name) = match query_descriptor(handle) {
        Ok(d) => {
            let (t, r) = bus_transport(d.bus);
            (
                Some(t.to_string()),
                r,
                compose_name(&d.vendor, &d.product, &path),
            )
        }
        Err(_) => (None, false, path.clone()), // 运输/型号仅提示性：查询失败不丢盘
    };
    Ok(DeviceInfo {
        id: format!("win:{path}"),
        name,
        kind: DeviceKind::Physical,
        size_bytes,
        removable,
        fs_guess: None, // 同 Linux：枚举不做 FS 探测（归扫描期）
        transport,
    })
}

/// 只读物理盘句柄（desired access 仅 `GENERIC_READ`；打开后 size/transport 取自句柄）。
pub struct WindowsBlockDevice {
    info: DeviceInfo,
    handle: OwnedHandle,
    /// 内核文件游标是跨线程共享的可变状态（`SetFilePointerEx` 与 `ReadFile` 非原子对）；
    /// `read_at` 持锁串行化——「unsafe Send/Sync 成立」的关键一环。
    io_lock: Mutex<()>,
}

impl WindowsBlockDevice {
    /// 打开 `\\.\PhysicalDriveN`（大小写不敏感，内部规范化）。未验证（需真机）。
    pub fn open(path: &str) -> Result<Self, DeviceError> {
        let number = parse_physical_drive_path(path).ok_or_else(|| {
            DeviceError::NotAFile(format!(
                "Windows 仅支持物理盘路径 \\\\.\\PhysicalDriveN: {path}"
            ))
        })?;
        let handle = open_readonly(&format!(r"\\.\PhysicalDrive{number}"))?;
        let info = info_from_handle(handle.0, number)?; // 失败 → handle Drop 关闭
        Ok(Self {
            info,
            handle,
            io_lock: Mutex::new(()),
        })
    }
}

// SAFETY: 句柄仅用于只读 IO（`CreateFileW` 只请求 `GENERIC_READ`，全文件无写 API）；唯一的
// 跨线程可变共享状态是内核文件游标，由 `io_lock` 串行化；`info` 只读。⇒ Send/Sync 成立。
unsafe impl Send for WindowsBlockDevice {}
unsafe impl Sync for WindowsBlockDevice {}

impl BlockDevice for WindowsBlockDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// 只读（`SetFilePointerEx`+`ReadFile`；`io_lock` 串行化文件游标）。读越尾按设备大小
    /// **钳位为短读**（与 Linux 块设备 pread 语义一致）；`offset` 越界/空 buf 恒 `Ok(0)`。
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        let remaining = self.info.size_bytes.saturating_sub(offset);
        let want = buf.len().min(remaining.min(usize::MAX as u64) as usize);
        let _serial = self.io_lock.lock().unwrap();
        let mut done = 0usize;
        while done < want {
            // SAFETY: 句柄由 self.handle 持有、此间存活；SetFilePointerEx 只移动文件游标。
            let ok = unsafe {
                SetFilePointerEx(
                    self.handle.0,
                    (offset + done as u64) as i64,
                    std::ptr::null_mut(),
                    FILE_BEGIN,
                )
            };
            if ok == 0 {
                return Err(DeviceError::Io(std::io::Error::last_os_error()));
            }
            let chunk = (want - done).min(u32::MAX as usize) as u32;
            let mut n = 0u32;
            // SAFETY: buf[done..] 可写 chunk（≥1）字节；overlapped 传 null = 同步读。
            let ok = unsafe {
                ReadFile(
                    self.handle.0,
                    buf[done..].as_mut_ptr(),
                    chunk,
                    &mut n,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(DeviceError::Io(std::io::Error::last_os_error()));
            }
            if n == 0 {
                break; // 设备端短读（上面已钳位，正常不达）
            }
            done += n as usize;
        }
        Ok(done)
    }

    // source_rdev 不覆写（默认 None）：Windows 无 st_rdev ⇒ 导出同盘校验缺口，
    // 见模块头注与 docs/security §8（M2 补：卷句柄卷号比较）。
}
