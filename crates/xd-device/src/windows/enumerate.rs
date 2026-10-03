// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! SetupAPI 枚举/归类层：`enumerate`、`WindowsDisk` 与全部纯函数（+ 单测）。
//! 自 `windows.rs` 机械迁出（T2 收口，纯重构零行为变化）；公开路径经 `windows.rs` 的
//! `pub use` 重导出，`xd_device::windows::*` 不变。
//!
//! 枚举需每盘一次只读 open（盘号/大小/BusType 只能经句柄查询）⇒ 需管理员权限；
//! 未验证（需真机）——CI windows runner 只证枚举/读冒烟（`tests/windows_smoke.rs`）。

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    BusType1394, BusTypeNvme, BusTypeSata, BusTypeSd, BusTypeUsb,
};
use windows_sys::Win32::System::Ioctl::GUID_DEVINTERFACE_DISK;

use super::{device_number, info_from_handle, open_readonly};
use crate::{DeviceError, DeviceInfo};

/// 枚举到的整盘：`path` = Win32 设备路径 `\\.\PhysicalDriveN`（`info.id` 为同源 `win:` 前缀 id）。
#[derive(Debug, Clone)]
pub struct WindowsDisk {
    pub info: DeviceInfo,
    pub path: String,
}

/// `HDEVINFO` 枚举器句柄（`Drop` 即 destroy；所有返回路径都收口）。
struct DevInfoSet(HDEVINFO);

impl Drop for DevInfoSet {
    fn drop(&mut self) {
        // SAFETY: 枚举器句柄唯一所有权在此；Drop 只发生一次。
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

// ---- 纯函数层（无 OS 调用，随本文件迁出；`windows.rs` 句柄层亦用 ⇒ `pub(crate)`，不扩公开面）----

/// UTF-16 + NUL 终止（`CreateFileW`/SetupAPI 的 `PCWSTR` 约定）。
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `\\.\PhysicalDriveN`（大小写不敏感）→ N。信任边界校验：拒绝其他路径形态
/// （`--device` 输入；与 Linux 侧「必须是块设备节点」同口径，见 `LinuxBlockDevice::open`）。
pub(crate) fn parse_physical_drive_path(path: &str) -> Option<u32> {
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

/// 描述符内 NUL 结尾 ASCII 字段（偏移相对缓冲起点）；偏移 0（= 无此字段）或越界 → 空串。
pub(crate) fn descriptor_str(buf: &[u8], offset: u32) -> String {
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
pub(crate) fn bus_transport(bus: i32) -> (&'static str, bool) {
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
/// 与 `linux::enumerate::compose_disk_name` **逐字同构**（含 `starts_with` 去重分支；
/// 跨 cfg 不复用：linux 模块在 Windows 不编译）。两平台测试表同缺「product 含 vendor
/// 但非前缀」形态（如 vendor `ATA` / product `ST2000 ATA`）——该形态的同步补测留 M2 一并
/// （qual-m1e-t2 P4：仅注释记录，行为零改动）。
pub(crate) fn compose_name(vendor: &str, product: &str, fallback: &str) -> String {
    let (v, p) = (vendor.trim(), product.trim());
    if p.is_empty() {
        fallback.to_string()
    } else if !v.is_empty() && !p.starts_with(v) {
        format!("{v} {p}")
    } else {
        p.to_string()
    }
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
        if info.size_bytes == 0 {
            continue; // 口径对齐 Linux `parse_entry_zero_size_is_none`：无媒体/幻影盘不列
        }
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
            r"\\.\PhysicalDrive+7", // 数字位校验：`+` 非 ASCII 数字（qual-m1e-t2 P2）
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

    #[test]
    fn wide_nul_to_string_reads_nul_terminated_and_tail_unit() {
        let s: Vec<u16> = "AB\0".encode_utf16().collect();
        // NUL 在界内 → 读至 NUL
        assert_eq!(
            unsafe { wide_nul_to_string(s.as_ptr(), s.len()) },
            Some("AB".to_string())
        );
        // 尾 unit 恰为 NUL（上限正好覆盖到它）→ 仍读到
        assert_eq!(
            unsafe { wide_nul_to_string(s.as_ptr(), 3) },
            Some("AB".to_string())
        );
        // 前导 NUL = 空串
        let just_nul = [0u16];
        assert_eq!(
            unsafe { wide_nul_to_string(just_nul.as_ptr(), 1) },
            Some(String::new())
        );
        // 非 BMP 一半之外的多字节（CJK）走 from_utf16_lossy 正常往返
        let cjk: Vec<u16> = "盘0\0".encode_utf16().collect();
        assert_eq!(
            unsafe { wide_nul_to_string(cjk.as_ptr(), cjk.len()) },
            Some("盘0".to_string())
        );
    }

    #[test]
    fn wide_nul_to_string_bounded_never_overreads() {
        let no_nul: Vec<u16> = "XY".encode_utf16().collect();
        // 界内无 NUL → None（穷尽上限即停，不越界找 NUL）
        assert_eq!(
            unsafe { wide_nul_to_string(no_nul.as_ptr(), no_nul.len()) },
            None
        );
        // 上限 0 → None（界为空）
        assert_eq!(unsafe { wide_nul_to_string(no_nul.as_ptr(), 0) }, None);
        // NUL 恰在界外一个 unit → 不得越界读，判 None
        let s: Vec<u16> = "AB\0".encode_utf16().collect();
        assert_eq!(unsafe { wide_nul_to_string(s.as_ptr(), 2) }, None);
    }
}
