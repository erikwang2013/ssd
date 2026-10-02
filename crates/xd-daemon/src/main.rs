// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 小盾桌面特权进程：stdio JSON-RPC 服务（每行一条 JSON，见 proto/v0/README.md）。
//! 提权归 M4；M1e 起 Linux 支持 --device 注册物理块设备 + 启动时 sysfs 枚举供 device.list，
//! 并在 root（pkexec 兜底）路径做 --image 参数纵深防御（privcheck，见 docs/security/linux-privilege-model.md）。

#[cfg(target_os = "linux")]
mod privcheck;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use xd_core::api::{PROTOCOL_VERSION, Request, Response, RpcErr, RpcError};
use xd_core::handlers::{CoreCtx, handle_request};
use xd_device::BlockDevice;
use xd_device::image::ImageFileDevice;

fn main() {
    let mut devices: Vec<Box<dyn BlockDevice>> = Vec::new();
    // 提权兜底路径（pkexec 以 root 拉起）的准入判定，见 docs/security/linux-privilege-model.md。一次 /proc 读。
    #[cfg(target_os = "linux")]
    let euid = privcheck::effective_uid();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--image" => {
                let Some(path) = args.next() else {
                    eprintln!("error: --image requires a path");
                    std::process::exit(2);
                };
                // root 模式（pkexec 兜底）纵深防御：O_NOFOLLOW 打开 + 属主须为 PKEXEC_UID。
                // 常规路径（uaccess，euid != 0）不做此校验：权限由 udev ACL 与文件属主决定。
                // root_mode 对 euid 读不到（None）失败关闭——不静默跳过校验。
                #[cfg(target_os = "linux")]
                if privcheck::root_mode(euid) {
                    use std::os::unix::fs::MetadataExt;
                    let file = match privcheck::open_image_no_follow(&PathBuf::from(&path)) {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("error: cannot open image {path}: {e}");
                            std::process::exit(2);
                        }
                    };
                    let md = match file.metadata() {
                        Ok(m) => m,
                        Err(e) => {
                            eprintln!("error: cannot stat image {path}: {e}");
                            std::process::exit(2);
                        }
                    };
                    let pkexec_uid = std::env::var("PKEXEC_UID")
                        .ok()
                        .and_then(|s| s.parse().ok());
                    if let Err(reason) =
                        privcheck::check_image_arg(md.uid(), md.is_file(), pkexec_uid)
                    {
                        eprintln!("error: {reason}");
                        std::process::exit(2);
                    }
                }
                match ImageFileDevice::open(&PathBuf::from(&path)) {
                    Ok(dev) => devices.push(Box::new(dev)),
                    Err(e) => {
                        eprintln!("error: cannot open image {path}: {e}");
                        std::process::exit(2);
                    }
                }
            }
            "--device" => {
                let Some(path) = args.next() else {
                    eprintln!("error: --device requires a node path");
                    std::process::exit(2);
                };
                #[cfg(target_os = "linux")]
                match xd_device::linux::LinuxBlockDevice::open(&PathBuf::from(&path)) {
                    Ok(dev) => devices.push(Box::new(dev)),
                    Err(e) => {
                        eprintln!("error: cannot open device {path}: {e}");
                        std::process::exit(2);
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    eprintln!("error: --device 仅 Linux 支持: {path}");
                    std::process::exit(2);
                }
            }
            other => {
                eprintln!("error: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }

    // 启动枚举（Linux，零 open()）：失败不阻塞 daemon 启动，但留痕（UI 侧收集 stderr 可诊断）。
    #[cfg(target_os = "linux")]
    let list_only: Vec<xd_device::DeviceInfo> =
        match xd_device::linux::BlockEnumerator::new().list() {
            Ok(disks) => disks.iter().map(|d| d.device_info()).collect(),
            Err(e) => {
                eprintln!("warn: 物理磁盘枚举失败（listing 可能不完整）: {e}");
                Vec::new()
            }
        };
    #[cfg(not(target_os = "linux"))]
    let list_only: Vec<xd_device::DeviceInfo> = Vec::new();

    eprintln!(
        "小盾 xd-daemon {} · © erik.xyz erik@erik.xyz · protocol {} · {} 个设备已注册 · 枚举到 {} 个物理磁盘",
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION,
        devices.len(),
        list_only.len()
    );

    let ctx = CoreCtx::new(devices).with_list_only(list_only);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                eprintln!("error: read failed: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle_request(&ctx, &req),
            Err(_) => Response::Err(RpcErr {
                jsonrpc: "2.0".into(),
                id: serde_json::Value::Null,
                error: RpcError::parse_error(),
            }),
        };
        if writeln!(stdout, "{}", serde_json::to_string(&response).unwrap()).is_err() {
            break;
        }
        let _ = stdout.flush();
    }
}
