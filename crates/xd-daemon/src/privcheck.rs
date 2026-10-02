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

/// euid 是否按 root 处理：**失败关闭**——/proc 读不到（None）时视同 root 走校验分支。
pub fn root_mode(euid: Option<u32>) -> bool {
    matches!(euid, None | Some(0))
}

/// 打开 --image：O_NOFOLLOW（拒符号链接换靶）+ O_NONBLOCK（防 --image <fifo> 卡死在 open）。
pub fn open_image_no_follow(p: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // asm-generic 值，Linux 各架构（x86_64/aarch64/arm/riscv）同值；见 man 2 open。
    const O_NOFOLLOW: i32 = 0o400000;
    const O_NONBLOCK: i32 = 0o4000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
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
        assert!(check_image_arg(0, true, Some(0)).is_ok()); // root/root 放行（pkexec 以 root 跑 root 属主镜像）
    }

    #[test]
    fn root_mode_is_fail_closed() {
        assert!(root_mode(Some(0)));
        assert!(root_mode(None)); // /proc 读不到 → 视同 root，不静默跳过校验
        assert!(!root_mode(Some(1000)));
    }

    #[test]
    fn open_image_no_follow_accepts_regular_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.img");
        std::fs::write(&target, b"x").unwrap();
        assert!(open_image_no_follow(&target).is_ok());
        let link = dir.path().join("link.img");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = open_image_no_follow(&link).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(40)); // ELOOP
    }

    #[test]
    fn effective_uid_matches_process() {
        // 本进程（非 root）解析值应与 `id -u` 一致；root CI 下也成立
        let uid = effective_uid().unwrap();
        let Ok(out) = std::process::Command::new("id").arg("-u").output() else {
            eprintln!("skip: `id` 不可用");
            return;
        };
        let real: u32 = String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(uid, real);
    }
}
