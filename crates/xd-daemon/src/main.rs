// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 小盾桌面特权进程：JSON-RPC 服务（每行一条 JSON，见 proto/v0/README.md）。
//! 传输二选一：stdio（缺省）或 TCP 回环提权会话（`--listen`+`--port-file` 成对给出，
//! 协议与威胁模型见 transport.rs 头注与 docs/security）。
//! 提权归 M4；M1e 起 Linux 支持 --device 注册物理块设备 + 启动时 sysfs 枚举供 device.list
//! （Windows 侧 --device 支持 `\\.\PhysicalDriveN` 只读打开，真机语义未验证，见 docs/security §8），
//! 并在 root（pkexec 兜底）路径做 --image 参数纵深防御（privcheck，见 docs/security/linux-privilege-model.md）。
//! M1b：扫描 worker 线程与主循环经唯一写口（`transport::write_line`）串行化；`--db` 指定任务库
//! （缺省 XDG state 路径，打开失败降级内存库并 warn）。

mod export_worker;
mod portfile;
#[cfg(target_os = "linux")]
mod privcheck;
mod transport;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use xd_core::api::PROTOCOL_VERSION;
use xd_core::export::ExportManager;
use xd_core::handlers::CoreCtx;
#[cfg(target_os = "linux")]
use xd_core::scan_task::OpenError;
use xd_core::scan_task::{DeviceOpener, NotifyFn, ScanCanceled, ScanManager};
use xd_core::store::Store;
use xd_device::BlockDevice;
use xd_device::image::ImageFileDevice;

/// `--db` 缺省库路径：`XDG_STATE_HOME/xiaodun/tasks.db` → `$HOME/.local/state/xiaodun/tasks.db`；
/// 两者都拿不到返回 None（内存库）。
fn default_db_path() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_STATE_HOME")
        && !x.is_empty()
    {
        return Some(PathBuf::from(x).join("xiaodun/tasks.db"));
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".local/state/xiaodun/tasks.db"))
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // 导出子进程模式（父 daemon `spawn 自身 --export-worker …` 拉起）：首参即分派，
    // 不进常规 arg 循环（参数集与 CLI 不重叠）；语义与权限序见 export_worker 模块头注。
    if argv.first().map(String::as_str) == Some("--export-worker") {
        match export_worker::parse_args(&argv[1..]) {
            Ok(a) => std::process::exit(export_worker::run(a)),
            Err(msg) => {
                eprintln!("error: --export-worker: {msg}");
                std::process::exit(2);
            }
        }
    }

    let mut devices: Vec<Arc<dyn BlockDevice>> = Vec::new();
    let mut db_path: Option<PathBuf> = None;
    let mut listen: Option<std::net::SocketAddr> = None;
    let mut port_file: Option<PathBuf> = None;
    // 提权兜底路径（pkexec 以 root 拉起）的准入判定，见 docs/security/linux-privilege-model.md。一次 /proc 读。
    #[cfg(target_os = "linux")]
    let euid = privcheck::effective_uid();
    let mut args = argv.into_iter();
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
                    Ok(dev) => devices.push(Arc::new(dev)),
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
                    Ok(dev) => devices.push(Arc::new(dev)),
                    Err(e) => {
                        eprintln!("error: cannot open device {path}: {e}");
                        std::process::exit(2);
                    }
                }
                // Windows：只读物理盘句柄（id `win:\\.\PhysicalDriveN`；未验证=需真机，见
                // docs/security §8）。枚举/打开同源，见 xd-device/src/windows.rs。
                #[cfg(target_os = "windows")]
                match xd_device::windows::WindowsBlockDevice::open(&path) {
                    Ok(dev) => devices.push(Arc::new(dev)),
                    Err(e) => {
                        eprintln!("error: cannot open device {path}: {e}");
                        std::process::exit(2);
                    }
                }
                #[cfg(not(any(target_os = "linux", target_os = "windows")))]
                {
                    eprintln!("error: --device 仅 Linux 支持: {path}");
                    std::process::exit(2);
                }
            }
            "--db" => {
                let Some(path) = args.next() else {
                    eprintln!("error: --db requires a path");
                    std::process::exit(2);
                };
                db_path = Some(PathBuf::from(path));
            }
            "--listen" => {
                let Some(addr) = args.next() else {
                    eprintln!("error: --listen requires host:port");
                    std::process::exit(2);
                };
                match addr.parse::<std::net::SocketAddr>() {
                    // 监听面仅回环（提权会话传输的安全前提，见 docs/security）：非回环地址拒绝。
                    Ok(a) if a.ip().is_loopback() => listen = Some(a),
                    Ok(a) => {
                        eprintln!("error: --listen 仅允许回环地址（收到 {a}）");
                        std::process::exit(2);
                    }
                    Err(e) => {
                        eprintln!("error: --listen 需为 host:port: {e}");
                        std::process::exit(2);
                    }
                }
            }
            "--port-file" => {
                let Some(path) = args.next() else {
                    eprintln!("error: --port-file requires a path");
                    std::process::exit(2);
                };
                port_file = Some(PathBuf::from(path));
            }
            other => {
                eprintln!("error: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }

    // `--listen`/`--port-file` 成对出现才进 TCP 模式；只给其一 = 参数错误（不静默退 stdio）。
    let tcp = match (listen, port_file) {
        (Some(addr), Some(port_file)) => Some(transport::TcpOptions { addr, port_file }),
        (None, None) => None,
        _ => {
            eprintln!("error: --listen 与 --port-file 须成对出现");
            std::process::exit(2);
        }
    };

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

    // 装在 spawn 任何 worker 之前：取消用 unwind 标记作控制流，静默其对 stderr 的默认输出；
    // 真 panic 照常打印。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info.payload().downcast_ref::<ScanCanceled>().is_none() {
            default_hook(info);
        }
    }));

    eprintln!(
        "小盾 xd-daemon {} · © erik.xyz erik@erik.xyz · protocol {} · {} 个设备已注册 · 枚举到 {} 个物理磁盘",
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION,
        devices.len(),
        list_only.len()
    );

    let db = db_path.or_else(default_db_path);
    // `export_db` = 导出子进程 `--db`：仅真文件库时可用（内存库降级 → None → export.start 诚实 -32603）
    let (store, export_db) = match &db {
        Some(p) => {
            // sqlite 不建父目录：没有就自己建；失败留给 Store::open 报错并降级。
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match Store::open(p) {
                Ok(s) => (s, Some(p.clone())),
                Err(e) => {
                    eprintln!("warn: 任务库打开失败（{e}），改用内存库（重启后结果不保留）");
                    (Store::open_memory().expect("sqlite in-memory"), None)
                }
            }
        }
        None => (Store::open_memory().expect("sqlite in-memory"), None),
    };

    // 通知广播器（stdio 会话 / 各 TCP 连接各注册一个写端；逐端锁 + flush 串行化整行）。
    let notifier = Arc::new(transport::Notifier::new());
    let notify: NotifyFn = {
        let notifier = notifier.clone();
        Arc::new(move |v: serde_json::Value| notifier.broadcast(&v))
    };
    // `tasks` 持设备句柄至 daemon 退出（M1c 现场续跑复用）；USB 安全弹出前的关句柄策略归 M1d/M2。
    let mgr = Arc::new(ScanManager::new(store, notify.clone()));
    if let Err(e) = mgr.recover_after_restart() {
        eprintln!("warn: 遗留任务状态修复失败: {e}");
    }
    // 导出管理器与扫描共享同一 store（父侧校验读同库）；子进程经 --db 只读打开同一文件。
    let exports = Arc::new(ExportManager::new(mgr.store_arc(), notify, export_db));

    #[cfg(target_os = "linux")]
    let opener: Arc<dyn DeviceOpener> = Arc::new(DaemonOpener);
    #[cfg(not(target_os = "linux"))]
    let opener: Arc<dyn DeviceOpener> = Arc::new(xd_core::scan_task::NoopOpener);

    let ctx = Arc::new(
        CoreCtx::new(devices)
            .with_list_only(list_only)
            .with_scan(mgr, opener)
            .with_export(exports),
    );

    // TCP 回环（提权会话）：不读 stdin、stdout 不写（诊断全 stderr）；服务循环不返回。
    if let Some(opts) = tcp {
        transport::serve_tcp(opts, ctx.clone(), notifier.clone());
    }

    // stdio 会话：注册通知写端（与响应写出共用同一把锁 ⇒ 整行不交错），进程存活期内一直在册。
    // 用 `Stdout` 而非 `StdoutLock<'static>`：现 std 的锁句柄含 `ReentrantLockGuard`（!Send），
    // 无法跨 worker 共享。
    let out: Arc<Mutex<std::io::Stdout>> = Arc::new(Mutex::new(std::io::stdout()));
    let _sink = notifier.register(Box::new(transport::SharedSink(out.clone())));
    let stdin = std::io::stdin();
    transport::serve_lines(stdin.lock(), &ctx, &out);
}

/// 懒打开物理设备（device.list 零 open 铁律的唯一出口）。`image:` 一律拒绝——镜像只能经
/// 启动参数 `--image` 注册（root 走 privcheck 准入）；否则提权 daemon 会沦为任意路径读取器。
#[cfg(target_os = "linux")]
struct DaemonOpener;

#[cfg(target_os = "linux")]
impl DeviceOpener for DaemonOpener {
    fn open(&self, id: &str) -> Result<Arc<dyn BlockDevice>, OpenError> {
        if id.starts_with("image:") {
            return Err(OpenError::Other(
                "images must be registered via --image at startup".into(),
            ));
        }
        let Some(path) = id.strip_prefix("unix:") else {
            return Err(OpenError::Other(format!("unknown device id scheme: {id}")));
        };
        match xd_device::linux::LinuxBlockDevice::open(&PathBuf::from(path)) {
            Ok(d) => Ok(Arc::new(d)),
            Err(xd_device::DeviceError::Io(e))
                if e.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                Err(OpenError::PermissionDenied)
            }
            Err(e) => {
                eprintln!("warn: 打开设备 {id} 失败: {e}");
                Err(OpenError::Other(e.to_string()))
            }
        }
    }
}
