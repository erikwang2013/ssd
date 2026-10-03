// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! Windows 物理块设备：SetupAPI 枚举（`GUID_DEVINTERFACE_DISK`）+ 只读物理盘句柄
//! （`\\.\PhysicalDriveN`）。
//!
//! **未验证（需真机）**：本文件全部平台路径——CI windows runner 只证「可编译 + 枚举/读冒烟」
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

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    BusType1394, BusTypeNvme, BusTypeSata, BusTypeSd, BusTypeUsb, CreateFileW, FILE_BEGIN,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile, SetFilePointerEx,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    GET_LENGTH_INFORMATION, GUID_DEVINTERFACE_DISK, IOCTL_DISK_GET_LENGTH_INFO,
    IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
    STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER, STORAGE_PROPERTY_QUERY,
    StorageDeviceProperty,
};

use crate::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};

/// `STORAGE_DEVICE_DESCRIPTOR` 是变长结构（定长头 + vendor/product ASCII）；
/// 1024 字节覆盖定长头与全部常见字符串字段。
const DESCRIPTOR_BUF_BYTES: usize = 1024;

/// 枚举到的整盘：`path` = Win32 设备路径 `\\.\PhysicalDriveN`（`info.id` 为同源 `win:` 前缀 id）。
#[derive(Debug, Clone)]
pub struct WindowsDisk {
    pub info: DeviceInfo,
    pub path: String,
}

/// Owned 句柄（`Drop` 即关）。枚举与 [`WindowsBlockDevice`] 共用。
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄唯一所有权在此；Drop 只发生一次。
        unsafe { CloseHandle(self.0) };
    }
}

/// `HDEVINFO` 枚举器句柄（`Drop` 即 destroy；所有返回路径都收口）。
struct DevInfoSet(HDEVINFO);

impl Drop for DevInfoSet {
    fn drop(&mut self) {
        // SAFETY: 枚举器句柄唯一所有权在此；Drop 只发生一次。
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// UTF-16 + NUL 终止（`CreateFileW`/SetupAPI 的 `PCWSTR` 约定）。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `\\.\PhysicalDriveN`（大小写不敏感）→ N。信任边界校验：拒绝其他路径形态
/// （`--device` 输入；与 Linux 侧「必须是块设备节点」同口径，见 `LinuxBlockDevice::open`）。
fn parse_physical_drive_path(path: &str) -> Option<u32> {
    const PREFIX: &str = r"\\.\PhysicalDrive";
    if !path.get(..PREFIX.len())?.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let digits = &path[PREFIX.len()..]; // 前缀已按 char 边界切出 ⇒ 此处必为边界
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
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

/// 描述符内 NUL 结尾 ASCII 字段（偏移相对缓冲起点）；偏移 0（= 无此字段）或越界 → 空串。
fn descriptor_str(buf: &[u8], offset: u32) -> String {
    if offset == 0 {
        return String::new();
    }
    let Some(rest) = buf.get(offset as usize..) else {
        return String::new();
    };
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..end]).trim().to_string()
}

/// `STORAGE_BUS_TYPE` → (契约 transport 值, removable)。removable 口径 = 计划 Task 2
/// （Usb/1394/Sd）；值域对齐 `proto/v1`（usb/sata/nvme/other，未知归 other）。
fn bus_transport(bus: i32) -> (&'static str, bool) {
    if bus == BusTypeUsb {
        ("usb", true)
    } else if bus == BusTypeSata {
        ("sata", false)
    } else if bus == BusTypeNvme {
        ("nvme", false)
    } else if bus == BusType1394 || bus == BusTypeSd {
        ("other", true)
    } else {
        ("other", false)
    }
}

/// 展示名合成：vendor + product（去重后不丢型号）；product 空回退物理盘路径。
/// 与 `linux::enumerate::compose_disk_name` 同语义（跨 cfg 不复用：linux 模块在 Windows 不编译）。
fn compose_name(vendor: &str, product: &str, fallback: &str) -> String {
    let (v, p) = (vendor.trim(), product.trim());
    if p.is_empty() {
        fallback.to_string()
    } else if !v.is_empty() && !p.starts_with(v) {
        format!("{v} {p}")
    } else {
        p.to_string()
    }
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

/// SetupAPI 枚举整盘：接口路径 → 只读句柄 → 盘号/大小/总线 → `\\.\PhysicalDriveN`。
/// 逐盘容错（单盘任一查询失败 → 跳过该盘，不中止整体——同 linux 枚举口径）。
///
/// 未验证（需真机）；CI windows runner 实跑 `tests/windows_smoke.rs`。需管理员权限
/// （Vista+ 物理盘 `GENERIC_READ` 打开即需提权；见 docs/security §8 已知限制）。
pub fn enumerate() -> Result<Vec<WindowsDisk>, DeviceError> {
    let devs = DevInfoSet(unsafe {
        // SAFETY: 全 null 参数 + DIGCF_PRESENT|DIGCF_DEVICEINTERFACE 是文档化用法。
        SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_DISK,
            std::ptr::null(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    });
    if devs.0 == INVALID_HANDLE_VALUE as HDEVINFO {
        return Err(DeviceError::Io(std::io::Error::last_os_error()));
    }

    let mut found: Vec<(u32, WindowsDisk)> = Vec::new();
    let mut index = 0u32;
    loop {
        let mut iface = SP_DEVICE_INTERFACE_DATA {
            cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: devs 为存活的有效枚举器；iface 已按 API 要求置 cbSize。
        let ok = unsafe {
            SetupDiEnumDeviceInterfaces(
                devs.0,
                std::ptr::null(),
                &GUID_DEVINTERFACE_DISK,
                index,
                &mut iface,
            )
        };
        if ok == 0 {
            break; // ERROR_NO_MORE_ITEMS（穷尽）或环境异常：都停（不猜盘）
        }
        index += 1;
        let Some(path) = interface_path(devs.0, &iface) else {
            continue;
        };
        let Ok(handle) = open_readonly(&path) else {
            continue;
        };
        let Ok(number) = device_number(handle.0) else {
            continue;
        };
        let Ok(info) = info_from_handle(handle.0, number) else {
            continue;
        };
        found.push((
            number,
            WindowsDisk {
                path: format!(r"\\.\PhysicalDrive{number}"),
                info,
            },
        ));
    }
    found.sort_by_key(|(number, _)| *number); // 盘号序（稳定展示；同 Linux 枚举排序口径）
    Ok(found.into_iter().map(|(_, d)| d).collect())
}

/// 接口路径两段式取回（先问长度再取数据）；失败 → None（跳过该盘）。
fn interface_path(devs: HDEVINFO, iface: &SP_DEVICE_INTERFACE_DATA) -> Option<String> {
    let mut required = 0u32;
    // SAFETY: 首次调用传 null 缓冲只为问长度（返回值必 ERROR_INSUFFICIENT_BUFFER）。
    unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            devs,
            iface,
            std::ptr::null_mut(),
            0,
            &mut required,
            std::ptr::null_mut(),
        )
    };
    if required == 0 {
        return None;
    }
    // 8 字节对齐缓冲（detail 结构含指针型字段；Vec<u8> 无对齐保证，用 u64 视图）。
    let mut buf = vec![0u64; (required as usize).div_ceil(8)];
    let detail = buf.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    // cbSize 语义：固定部分大小（x64=8；windows-sys 的 repr(C)/packed 定义 = size_of）。
    // SAFETY: detail 指向 ≥ required 字节的可写缓冲，写入 cbSize 在结构内。
    unsafe { (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 };
    // SAFETY: 缓冲 ≥ required 字节；iface 由调用方保证有效。
    let ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            devs,
            iface,
            detail,
            required,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    // DevicePath 是缓冲内 NUL 结尾宽串（起点 = cbSize 字段之后）。
    // SAFETY: path 指针在存活缓冲内；上限按缓冲长度收敛（API 保证 NUL，仍设界纵深）。
    let path_ptr = unsafe { (*detail).DevicePath.as_ptr() };
    let max_units = (required as usize).saturating_sub(std::mem::size_of::<u32>()) / 2;
    unsafe { wide_nul_to_string(path_ptr, max_units) }
}

/// NUL 结尾宽串 → String；`max_units` 内无 NUL → None（防越界读的界）。
///
/// SAFETY（调用方义务）：`p` 指向至少 `max_units` 个 u16 的有效只读内存。
unsafe fn wide_nul_to_string(mut p: *const u16, max_units: usize) -> Option<String> {
    let mut units = Vec::new();
    for _ in 0..max_units {
        // SAFETY: 由调用方保证 p 在上限内有效；逐元素前进。
        let u = unsafe { *p };
        if u == 0 {
            return Some(String::from_utf16_lossy(&units));
        }
        units.push(u);
        p = unsafe { p.add(1) };
    }
    None
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

#[cfg(test)]
mod tests {
    //! 纯函数单测（随 CI windows runner 执行；硬件路径归 tests/windows_smoke.rs）。
    use super::*;

    #[test]
    fn parse_physical_drive_path_accepts_canonical_forms() {
        assert_eq!(parse_physical_drive_path(r"\\.\PhysicalDrive0"), Some(0));
        assert_eq!(parse_physical_drive_path(r"\\.\PhysicalDrive15"), Some(15));
        // 设备命名空间大小写不敏感
        assert_eq!(parse_physical_drive_path(r"\\.\physicaldrive3"), Some(3));
    }

    #[test]
    fn parse_physical_drive_path_rejects_other_shapes() {
        for bad in [
            "",
            r"\\.\PhysicalDrive",
            r"\\.\PhysicalDriveX",
            r"\\.\PhysicalDrive1a",
            r"\\.\C:",
            r"\\?\PhysicalDrive0",
            r"C:\PhysicalDrive0",
            r"\\.\PhysicalDrive99999999999", // 超出 u32
            "\\.\\Physic\u{e0}lDrive0",      // 多字节字符：前缀切片不得 panic
        ] {
            assert_eq!(parse_physical_drive_path(bad), None, "应拒绝: {bad:?}");
        }
    }

    #[test]
    fn compose_name_mirrors_linux_semantics() {
        assert_eq!(
            compose_name("ATA", "ST2000LM015", r"\\.\PhysicalDrive1"),
            "ATA ST2000LM015"
        );
        // product 空 → 回退物理盘路径；product 已含 vendor 前缀不重复拼
        assert_eq!(
            compose_name("", "", r"\\.\PhysicalDrive0"),
            r"\\.\PhysicalDrive0"
        );
        assert_eq!(
            compose_name("ATA", "ATA ST2000LM015", "x"),
            "ATA ST2000LM015"
        );
        // 描述符字段是空格填充定长串
        assert_eq!(
            compose_name(" Samsung ", " SSD 860 ", "x"),
            "Samsung SSD 860"
        );
        assert_eq!(compose_name("  ", " X  ", "x"), "X");
    }

    #[test]
    fn bus_transport_maps_contract_values_and_removable() {
        assert_eq!(bus_transport(BusTypeUsb), ("usb", true));
        assert_eq!(bus_transport(BusTypeSata), ("sata", false));
        assert_eq!(bus_transport(BusTypeNvme), ("nvme", false));
        assert_eq!(bus_transport(BusType1394), ("other", true));
        assert_eq!(bus_transport(BusTypeSd), ("other", true));
        assert_eq!(bus_transport(0), ("other", false)); // BusTypeUnknown
    }

    #[test]
    fn descriptor_str_reads_nul_terminated_fields() {
        let buf = b"head\x00VENDOR  \x00PROD\x00";
        assert_eq!(descriptor_str(buf, 5), "VENDOR"); // 空格填充去尾
        assert_eq!(descriptor_str(buf, 14), "PROD");
        assert_eq!(descriptor_str(buf, 0), ""); // 偏移 0 = 无此字段
        assert_eq!(descriptor_str(buf, 999), ""); // 越界
        assert_eq!(descriptor_str(b"xyz", 1), "yz"); // 无 NUL → 取到结尾
    }

    #[test]
    fn wide_appends_nul_terminator() {
        assert_eq!(wide("A"), vec![0x41, 0]);
        assert_eq!(wide(""), vec![0]);
    }
}
