// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! euid==0（pkexec 兜底路径）下的参数纵深防御（第二道；第一道是 polkit 策略 auth_admin 不 keep）。

use std::io;
use std::path::Path;

/// --image 在 root 模式下的准入：必须是普通文件、属主 == PKEXEC_UID、PKEXEC_UID 必须存在。
pub fn check_image_arg(
    file_uid: u32,
    is_regular_file: bool,
    pkexec_uid: Option<u32>,
) -> Result<(), String> {
    if !is_regular_file {
        return Err("root 模式下 --image 必须是普通文件".into());
    }
    match pkexec_uid {
        Some(p) if p == file_uid => Ok(()),
        Some(p) => Err(format!(
            "root 模式下 --image 属主 {file_uid} 须为调用者 {p}（PKEXEC_UID）"
        )),
        None => Err("root 模式缺少 PKEXEC_UID（非 pkexec 启动？）——拒绝".into()),
    }
}

/// 打开 --image：O_NOFOLLOW（拒符号链接换靶）。
pub fn open_image_no_follow(p: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    const O_NOFOLLOW: i32 = 0o400000; // Linux x86_64 值；见 man 2 open
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(p)
}

/// 当前进程 effective uid（零依赖读 /proc/self/status 的 Uid 行第 2 列）。
pub fn effective_uid() -> Option<u32> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_arg_checks() {
        assert!(check_image_arg(1000, true, Some(1000)).is_ok());
        assert!(check_image_arg(1000, true, Some(0)).is_err()); // 属主不符
        assert!(check_image_arg(1000, true, None).is_err()); // 无 PKEXEC_UID
        assert!(check_image_arg(1000, false, Some(1000)).is_err()); // 非普通文件
    }

    #[test]
    fn effective_uid_matches_process() {
        // 本进程（非 root）解析值应与 `id -u` 一致；root CI 下也成立
        let uid = effective_uid().unwrap();
        let real: u32 = String::from_utf8(
            std::process::Command::new("id")
                .arg("-u")
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .parse()
        .unwrap();
        assert_eq!(uid, real);
    }
}
