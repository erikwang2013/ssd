// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! port-file（提权会话的端口与令牌交接件）与令牌生成。
//! 安全属性：unix 0600 + 同目录临时文件 rename 原子落盘（读者永远读不到半行）；
//! 令牌 16 字节 CSPRNG → 32 位十六进制，比较见 transport.rs（常数时间）。
//! **属主**：提权（root）daemon 写 UI 的 port-file 时把属主交还目标目录属主（[`adopt_owner_of_dir`]）
//! ——0600 的属主即读者，root:0600 普通用户读不到令牌、提权链断（T4 落地时发现的 T1 缺口；
//! 目录由 UI 以 `createTempSync` 0700 自建，属主恒为 UI 用户）。Windows 分支的写路径与令牌生成
//! 已由 CI windows runner **实跑**（tcp_session 全链路，见 docs/security §7「已验」）；
//! 未收紧的是 port-file ACL（NTFS 无 0600 语义，归 M2）。

use std::path::Path;

/// 写 `<path>`：一行 `<port> <token>\n`（端口取实际绑定值，见 transport::serve_tcp）。
pub(crate) fn write_port_file(path: &Path, port: u16, token: &str) -> std::io::Result<()> {
    write_port_file_impl(path, format!("{port} {token}\n").as_bytes())
}

/// unix：`<path>.tmp-<pid>` 以 0600 独占创建（`create_new`，不跟随符号链接）→ `rename` 原子替换。
/// 残留同名临时文件（pid 复用的死文件）先删再建——删除只作用于该路径本身，不跟随符号链接。
#[cfg(unix)]
fn write_port_file_impl(path: &Path, content: &[u8]) -> std::io::Result<()> {
    // `Write` 只在 unix 臂用于 write_all；置于臂内 ⇒ Windows 构建不产生 unused_imports
    // （cfg-flip 探针实证：顶层 use 在非 unix 形态下 -D warnings 红）。
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".tmp-{}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);

    let create = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
    };
    let mut f = match create() {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(&tmp)?;
            create()?
        }
        Err(e) => return Err(e),
    };
    f.write_all(content)?;
    f.flush()?;
    drop(f);
    adopt_owner_of_dir(&tmp, path)?;
    std::fs::rename(&tmp, path)
}

/// 提权写入（root daemon 写 UI 指定的 port-file）时把属主交还**目标目录的属主**：
/// 0600 的属主即读者，root:0600 的 F 普通用户读不到 ⇒ 提权会话拿不到令牌（T4 落地发现）。
/// 目录由 UI 以 `createTempSync`（0700）自建，属主恒为 UI 用户；非 root 写入时创建者本就是
/// 目录属主（或换属主失败不该拖垮会话）⇒ 只在 euid==root 且属主不同时才 chown。
/// **残留风险**（docs/security §10 明示）：能控制 `--port-file` 路径指向他人目录者，可让 root
/// daemon 把令牌交给该目录属主——根因是 pkexec 不校验参数（§3 已接受的同一缺口），M4 一并收紧。
#[cfg(unix)]
fn adopt_owner_of_dir(tmp: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    use rustix::process::{Gid, Uid, geteuid};

    if !geteuid().is_root() {
        return Ok(());
    }
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let (file, dir) = (tmp.metadata()?, dir.metadata()?);
    if file.uid() == dir.uid() {
        return Ok(());
    }
    rustix::fs::chown(
        tmp,
        Some(Uid::from_raw(dir.uid())),
        Some(Gid::from_raw(dir.gid())),
    )
    .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))
}

/// windows：直写。路径已由 CI windows runner 实跑（见模块头注）；**ACL 未收紧**——NTFS 无
/// unix 权限位，文件可读性由继承 ACL 决定（通常已限当前用户，但不保证）；显式收紧 TODO M2。
#[cfg(windows)]
fn write_port_file_impl(path: &Path, content: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, content)
}

/// 其他平台：显式拒绝（不静默退化）。[`random_token`] 同款。
#[cfg(not(any(unix, windows)))]
fn write_port_file_impl(_path: &Path, _content: &[u8]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "port-file 仅 unix/windows 支持（M1e：其他平台显式拒绝）",
    ))
}

/// 32 位十六进制会话令牌（16 字节 CSPRNG）。
pub(crate) fn random_token() -> String {
    let mut buf = [0u8; 16];
    fill_random(&mut buf);
    let mut s = String::with_capacity(32);
    for b in buf {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// unix：/dev/urandom。读失败 = 环境异常，响亮退出——令牌不可退化。
#[cfg(unix)]
fn fill_random(buf: &mut [u8]) {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buf))
        .unwrap_or_else(|e| panic!("/dev/urandom 读取失败: {e}"));
}

/// windows：BCryptGenRandom（系统首选 RNG）。CI windows runner 已实跑本路径（tcp_session
/// 全链路用真令牌）；真机 UAC 提权链归 M1 出口（Task 4）。
#[cfg(windows)]
fn fill_random(buf: &mut [u8]) {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    // SAFETY: 空算法句柄 + 系统首选 RNG 标志是 BCryptGenRandom 的文档化用法；
    // buf 为独占可变借用，长度如实传入。
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    assert!(status == 0, "BCryptGenRandom 失败: NTSTATUS {status:#x}");
}

/// 其他平台：显式拒绝（无 CSPRNG 可用时不静默凑数）。
#[cfg(not(any(unix, windows)))]
fn fill_random(_buf: &mut [u8]) {
    panic!("random_token 仅 unix/windows 支持（M1e：其他平台显式拒绝）");
}
