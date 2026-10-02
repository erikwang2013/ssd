// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! `--export-worker`：恢复导出执行体（由父 daemon `spawn 自身` 拉起，见 xd-core::export）。
//!
//! **权限序**（计划条款）：①按父权限打开源设备 fd → ②目标三重校验复核（权威）→ ③root 且
//! `PKEXEC_UID` 存在时降权到调用者 → ④此后所有文件写入均以普通用户身份。
//!
//! stdout 只出 JSON 行（`progress`/`item`/`fatal`/`finished`，逐行 flush）；stderr 留痕；
//! 退出码 0 = 正常（含逐件失败与被取消），2 = 致命（库/设备/校验失败，附 `fatal` 行）。
//! **EPIPE（父没了）→ 静默退出 0**：导出随父进程生命期结束，已写文件保留。
//!
//! 单线程进程：降权用 `rustix::thread::set_thread_res_*`（内核 per-thread 语义），在**未建任何
//! 线程前**调用，故与进程级降权等价（security 文档 §4「必须在建任何线程之前」在此成立）。
//! **降权路径未验证（需真机 root/pkexec）**：本机非 root 走不到降权臂；单测只覆盖纯决策函数。

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use xd_core::api::ScanEntry;
use xd_core::export::check_target;
use xd_core::fs_read::read_entry_range;
use xd_core::scan_task::FsKind;
use xd_core::store::Store;
use xd_device::BlockDevice;

/// 流式片大小（计划条款：导出内部片 4MiB；`fs.read` 契约上限 1MiB 是另一层，互不相关）。
const CHUNK: u64 = 4 * 1024 * 1024;
/// stdin idxs 上限（与 `xd_core::export::MAX_IDXS` 同值；超出 = 父侧校验被绕，致命）。
const MAX_IDXS: usize = 100_000;

#[derive(Debug, PartialEq)]
pub struct ExportArgs {
    pub db: PathBuf,
    pub task_id: u64,
    pub export_id: u64,
    pub target: PathBuf,
}

/// 解析 `--export-worker` 之后的参数（父进程按固定顺序传；未知/缺值一律拒绝）。
pub fn parse_args(args: &[String]) -> Result<ExportArgs, String> {
    let mut db = None;
    let mut task_id = None;
    let mut export_id = None;
    let mut target = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{a} requires a value"));
        match a.as_str() {
            "--db" => db = Some(PathBuf::from(val()?)),
            "--task" => task_id = Some(parse_num("--task", val()?)?),
            "--export-id" => export_id = Some(parse_num("--export-id", val()?)?),
            "--target" => target = Some(PathBuf::from(val()?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(ExportArgs {
        db: db.ok_or("--db is required")?,
        task_id: task_id.ok_or("--task is required")?,
        export_id: export_id.ok_or("--export-id is required")?,
        target: target.ok_or("--target is required")?,
    })
}

fn parse_num(flag: &str, s: &str) -> Result<u64, String> {
    s.parse().map_err(|_| format!("{flag}: bad number {s}"))
}

/// 入口：致命错误也走 stdout 的 `fatal` 行（父留痕 stderr），退出码 2。
pub fn run(args: ExportArgs) -> i32 {
    match run_inner(&args) {
        Ok(()) => 0,
        Err(reason) => {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", json!({"type": "fatal", "reason": reason}));
            let _ = out.flush();
            eprintln!("export worker {} fatal: {}", args.export_id, reason);
            2
        }
    }
}

fn run_inner(args: &ExportArgs) -> Result<(), String> {
    // 只读打开（不跑迁移；schema 由 daemon 的常规打开负责）；全部库读在降权前完成。
    let store = Store::open_read_only(&args.db)
        .map_err(|e| format!("open db {}: {e}", args.db.display()))?;
    let row = store
        .task(args.task_id)
        .map_err(|e| format!("read task: {e}"))?
        .ok_or_else(|| format!("task {} not found", args.task_id))?;
    let fs: FsKind = row
        .fs
        .parse()
        .map_err(|_| format!("unsupported fs: {}", row.fs))?;
    let euid = effective_uid();
    let pkexec_uid = pkexec_uid();
    // ① 父权限打开源设备（降权前唯一 open 出口；降权后不再 open 任何东西）
    let dev = open_device_by_id(&row.device_id, euid, pkexec_uid)?;
    let entries = read_entries(&store, args.task_id)?;
    let estimated = entries
        .iter()
        .fold(0u64, |acc, e| acc.saturating_add(e.size_bytes));
    // ② 目标三重校验复核（权威；父侧同类校验只为同步错误码，见 check_target 头注）
    check_target(&args.target, estimated, dev.source_rdev())
        .map_err(|e| format!("target check: {e:?}"))?;
    // ③ 降权（未验证（需真机 root/pkexec）——本机非 root 恒走不降权臂）
    maybe_drop_privileges(euid, pkexec_uid)?;
    // ④ 逐件导出：单件失败继续跑（终报逐件），stdout 断开（父没了）即退出 0
    let mut out = std::io::stdout().lock();
    let start = Instant::now();
    let total = entries.len() as u64;
    let (mut ok, mut degraded, mut failed, mut written) = (0u64, 0u64, 0u64, 0u64);
    let mut used: HashSet<String> = HashSet::new();
    for (i, e) in entries.iter().enumerate() {
        let name = unique_name(&mut used, file_name_for(e));
        match export_one(&*dev, fs, e, &args.target.join(&name)) {
            Ok(w) if w == e.size_bytes => {
                ok += 1;
                written += w;
            }
            Ok(w) => {
                degraded += 1;
                written += w;
                let line = json!({
                    "type": "item", "idx": e.idx, "name": name,
                    "status": "degraded", "reason": "short read",
                });
                if writeln!(out, "{line}").is_err() {
                    return Ok(()); // EPIPE：父没了
                }
            }
            Err(reason) => {
                failed += 1;
                let line = json!({
                    "type": "item", "idx": e.idx, "name": name,
                    "status": "failed", "reason": reason,
                });
                if writeln!(out, "{line}").is_err() {
                    return Ok(());
                }
            }
        }
        let line = json!({
            "type": "progress", "done": i as u64 + 1, "total": total,
            "writtenBytes": written, "elapsedMs": start.elapsed().as_millis() as u64,
        });
        if writeln!(out, "{line}").is_err() {
            return Ok(()); // EPIPE：父没了
        }
    }
    let line = json!({
        "type": "finished", "succeeded": ok, "degraded": degraded, "failed": failed,
    });
    let _ = writeln!(out, "{line}"); // 终报写不出去（父没了）也无妨：退出 0
    Ok(())
}

/// stdin 读 idxs（父在 spawn 后立即写入并关管道 → 读到 EOF 即一行 JSON 数组）。
fn read_entries(store: &Store, task_id: u64) -> Result<Vec<ScanEntry>, String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("read idxs: {e}"))?;
    let idxs: Vec<u64> = serde_json::from_str(line.trim()).map_err(|e| format!("bad idxs: {e}"))?;
    if idxs.is_empty() || idxs.len() > MAX_IDXS {
        return Err(format!("idxs size {} out of range", idxs.len()));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for idx in idxs {
        if !seen.insert(idx) {
            continue; // 防御性去重（父已去重；保持首现顺序 = 进度口径）
        }
        let e = store
            .entry(task_id, idx)
            .map_err(|e| format!("read entry {idx}: {e}"))?
            .ok_or_else(|| format!("entry {idx} not found"))?;
        out.push(e);
    }
    Ok(out)
}

/// 单件导出：`create_new`（名字已去重；重跑同目录同会 EEIST → 该件 failed，不覆盖既有文件）→
/// 4MiB 片循环写盘。短交付（`eof` 且已写 < `sizeBytes`）→ `Ok(已写)`（父侧记 degraded）；
/// 读/写错误 → `Err`（**删除半成品后**返回——半成品与完好文件不可区分，「宁可漏报不可错报」）。
fn export_one(
    dev: &dyn BlockDevice,
    fs: FsKind,
    e: &ScanEntry,
    path: &Path,
) -> Result<u64, String> {
    let cleanup = |f: std::fs::File| {
        drop(f);
        let _ = std::fs::remove_file(path);
    };
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| format!("create {}: {err}", path.display()))?;
    let mut off = 0u64;
    while off < e.size_bytes {
        let want = CHUNK.min(e.size_bytes - off);
        let (bytes, eof) = match read_entry_range(dev, fs, e, off, want) {
            Ok(v) => v,
            Err(err) => {
                cleanup(f);
                return Err(format!("read: {err:?}"));
            }
        };
        // bytes 空 ⇒ eof（read_entry_range 裁定的短交付）；先写后判，空写无副作用
        if let Err(err) = f.write_all(&bytes) {
            cleanup(f);
            return Err(format!("write: {err}"));
        }
        off += bytes.len() as u64;
        if eof {
            break;
        }
    }
    Ok(off)
}

/// 落盘名：空名（雕刻件）→ `carved_{idx:06}.{ext}`；否则 sanitize 原文件名。
fn file_name_for(e: &ScanEntry) -> String {
    if e.name.is_empty() {
        let ext = if e.ext.is_empty() { "bin" } else { &e.ext };
        format!("carved_{:06}.{ext}", e.idx)
    } else {
        sanitize(&e.name)
    }
}

/// 文件名净化：`..` 子串与 `/`、`\`、控制字符 → `_`（防路径穿越/怪字符）；
/// 净化后为空或恰为 `.` → `_`（`.` 会指向目录本身）。
fn sanitize(name: &str) -> String {
    let s: String = name
        .replace("..", "_")
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    if s.is_empty() || s == "." {
        "_".into()
    } else {
        s
    }
}

/// 重名去重（计划：`_2`/`_3` 计数后缀，插在扩展名前）：首个原名原样，其后 `{stem}_{n}.{ext}`；
/// 计数名若也被占（如真有 `A_2.JPG`）继续递增——**最终名以 `used` 集合唯一为准**。
fn unique_name(used: &mut HashSet<String>, name: String) -> String {
    if used.insert(name.clone()) {
        return name;
    }
    for n in 2u32.. {
        let cand = with_counter(&name, n);
        if used.insert(cand.clone()) {
            return cand;
        }
    }
    unreachable!("u32 计数域内必有空位")
}

fn with_counter(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}_{n}.{ext}"),
        _ => format!("{name}_{n}"),
    }
}

/// 按 store 内 device_id 打开源设备（**先按父权限**；降权前唯一 open 出口）。
///
/// 安全注：设备 id 来自**自家 store**（daemon 写的任务库），不经 RPC 参数——RPC 侧镜像只能经
/// `--image` 注册（`DaemonOpener` 拒 `image:`）。但库文件在常规路径下**属主即调用者用户**，
/// 故 `image:` 分支在 root 模式复用 `--image` 同闸（`privcheck`：O_NOFOLLOW + 属主须为
/// `PKEXEC_UID`）——不设此闸，伪造库行 `image:/etc/shadow` 会让提权 worker 沦为任意 root 可读
/// 文件的读取器（security 文档 §3 同类面的另一扇门）。非 root 模式与 `--image` 常规路径
/// 同语义（不做属主校验）。TOCTOU 残窗与 M4 `from_file` 闭合见 security 文档 §3/§4。
fn open_device_by_id(
    id: &str,
    euid: Option<u32>,
    pkexec_uid: Option<u32>,
) -> Result<Arc<dyn BlockDevice>, String> {
    if let Some(path) = id.strip_prefix("image:") {
        let p = Path::new(path);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            if crate::privcheck::root_mode(euid) {
                let file = crate::privcheck::open_image_no_follow(p)
                    .map_err(|e| format!("open image {path}: {e}"))?;
                let md = file
                    .metadata()
                    .map_err(|e| format!("stat image {path}: {e}"))?;
                crate::privcheck::check_image_arg(md.uid(), md.is_file(), pkexec_uid)?;
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (euid, pkexec_uid);
        let dev = xd_device::image::ImageFileDevice::open(p)
            .map_err(|e| format!("open image {path}: {e}"))?;
        return Ok(Arc::new(dev));
    }
    #[cfg(target_os = "linux")]
    if let Some(path) = id.strip_prefix("unix:") {
        let dev = xd_device::linux::LinuxBlockDevice::open(Path::new(path))
            .map_err(|e| format!("open device {path}: {e}"))?;
        return Ok(Arc::new(dev));
    }
    Err(format!("unknown device id scheme: {id}"))
}

/// euid 读取（与 `--image` 准入同源；读不到 → None = 按 root 处理，失败关闭）。
fn effective_uid() -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        crate::privcheck::effective_uid()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// pkexec 传入的调用者 uid（无 = 非 pkexec 启动）。
fn pkexec_uid() -> Option<u32> {
    std::env::var("PKEXEC_UID").ok()?.parse().ok()
}

/// 纯决策：降权起点（单测覆盖；真降权在 `drop_to_user`）。root 且有 PKEXEC_UID → 降到该 uid；
/// root 无 PKEXEC_UID → `None`（不降，stderr 警告）；非 root → `None`（常态，无特权可降）。
fn drop_plan(euid: Option<u32>, pkexec_uid: Option<u32>) -> Option<u32> {
    match euid {
        Some(0) => pkexec_uid,
        _ => None,
    }
}

fn maybe_drop_privileges(euid: Option<u32>, pkexec_uid: Option<u32>) -> Result<(), String> {
    match drop_plan(euid, pkexec_uid) {
        Some(uid) => drop_to_user(uid),
        None => {
            if euid == Some(0) && pkexec_uid.is_none() {
                eprintln!(
                    "warn: root 且无 PKEXEC_UID（非 pkexec 启动？）——不降权，导出文件将以 root 属主写出"
                );
            }
            Ok(())
        }
    }
}

/// **未验证（需真机 root/pkexec）**：本机（非 root）不可测。顺序：附加组清空 → gid → uid
/// （security 文档 §4；反序则 setresgid 已无权限）。gid 从 /etc/passwd 尽力解析，
/// 解析不到只降 uid + stderr 警告（计划条款——文件组属主将保留 root，文案如实说明）。
#[cfg(target_os = "linux")]
fn drop_to_user(uid: u32) -> Result<(), String> {
    let u = rustix::process::Uid::from_raw(uid);
    match passwd_gid(
        &std::fs::read_to_string("/etc/passwd").unwrap_or_default(),
        uid,
    ) {
        Some(gid) => {
            let g = rustix::process::Gid::from_raw(gid);
            rustix::thread::set_thread_groups(&[]).map_err(|e| format!("setgroups(0): {e}"))?;
            rustix::thread::set_thread_res_gid(g, g, g)
                .map_err(|e| format!("setresgid({gid}): {e}"))?;
        }
        None => eprintln!("warn: /etc/passwd 无 uid {uid} 的 gid——只降 uid（组属主保留 root）"),
    }
    rustix::thread::set_thread_res_uid(u, u, u).map_err(|e| format!("setresuid({uid}): {e}"))
}

#[cfg(not(target_os = "linux"))]
fn drop_to_user(_uid: u32) -> Result<(), String> {
    Err("privilege drop is Linux-only".into())
}

/// /etc/passwd 里该 uid 的 gid（纯函数，单测覆盖）：行格式 `name:pw:uid:gid:…`。
#[cfg(target_os = "linux")]
fn passwd_gid(passwd: &str, uid: u32) -> Option<u32> {
    for l in passwd.lines() {
        let mut f = l.split(':');
        let (u, g) = (f.nth(2), f.next());
        if let (Some(u), Some(g)) = (u, g)
            && u.parse::<u32>().ok() == Some(uid)
        {
            return g.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_parent_order_and_rejects_damage() {
        let argv: Vec<String> = [
            "--db",
            "/tmp/x.db",
            "--task",
            "7",
            "--export-id",
            "3",
            "--target",
            "/tmp/out",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            parse_args(&argv).unwrap(),
            ExportArgs {
                db: PathBuf::from("/tmp/x.db"),
                task_id: 7,
                export_id: 3,
                target: PathBuf::from("/tmp/out"),
            }
        );
        let bad = |v: &[&str]| {
            let v: Vec<String> = v.iter().map(|s| s.to_string()).collect();
            assert!(parse_args(&v).is_err(), "{v:?}");
        };
        bad(&[]); // 全缺
        bad(&["--db", "/tmp/x.db"]); // 缺 task/export-id/target
        bad(&[
            "--db",
            "/tmp/x.db",
            "--task",
            "x",
            "--export-id",
            "1",
            "--target",
            "/",
        ]);
        bad(&["--db"]); // 缺值
        bad(&[
            "--db",
            "/tmp/x.db",
            "--task",
            "1",
            "--export-id",
            "1",
            "--target",
            "/",
            "--wat",
            "1",
        ]);
    }

    #[test]
    fn drop_plan_only_when_root_with_pkexec_uid() {
        assert_eq!(drop_plan(Some(0), Some(1000)), Some(1000));
        assert_eq!(
            drop_plan(Some(0), None),
            None,
            "root 无 PKEXEC_UID：不降权（警告）"
        );
        assert_eq!(
            drop_plan(Some(1000), Some(1000)),
            None,
            "非 root：无特权可降"
        );
        assert_eq!(
            drop_plan(None, Some(1000)),
            None,
            "euid 读不到：非 root 处理（root_mode 只用于校验分支）"
        );
    }

    #[test]
    fn sanitize_rejects_traversal_and_control_chars() {
        assert_eq!(sanitize("IMG_0001.JPG"), "IMG_0001.JPG");
        assert_eq!(sanitize("../../etc/passwd"), "____etc_passwd");
        assert_eq!(sanitize("a/b\\c"), "a_b_c");
        assert_eq!(sanitize("x\u{0}y\nz"), "x_y_z");
        assert_eq!(sanitize(".."), "_");
        assert_eq!(sanitize("."), "_");
        assert_eq!(sanitize("..."), "_.");
    }

    #[test]
    fn unique_name_appends_counter_before_ext_and_claims_names() {
        let mut used = HashSet::new();
        assert_eq!(unique_name(&mut used, "A.JPG".into()), "A.JPG");
        assert_eq!(unique_name(&mut used, "A.JPG".into()), "A_2.JPG");
        assert_eq!(unique_name(&mut used, "A.JPG".into()), "A_3.JPG");
        // 计数名被真占（后到的原件恰叫 A_2.JPG）不妨碍唯一性
        assert_eq!(unique_name(&mut used, "A_2.JPG".into()), "A_2_2.JPG");
        // 无扩展名
        assert_eq!(unique_name(&mut used, "NOEXT".into()), "NOEXT");
        assert_eq!(unique_name(&mut used, "NOEXT".into()), "NOEXT_2");
        // 点开头（隐藏文件）不当作扩展名切分（stem 为空）
        assert_eq!(unique_name(&mut used, ".hidden".into()), ".hidden");
        assert_eq!(unique_name(&mut used, ".hidden".into()), ".hidden_2");
    }

    #[test]
    fn file_name_for_carved_and_sanitized() {
        let mk = |name: &str, ext: &str, idx: u64| ScanEntry {
            idx,
            name: name.into(),
            path: "/".into(),
            ext: ext.into(),
            size_bytes: 10,
            deleted: false,
            is_dir: false,
            quality: "complete".into(),
            first_cluster: 0,
            byte_offset: None,
            contiguous: None,
        };
        assert_eq!(file_name_for(&mk("", "jpg", 42)), "carved_000042.jpg");
        assert_eq!(
            file_name_for(&mk("", "", 7)),
            "carved_000007.bin",
            "ext 缺失回退 bin"
        );
        assert_eq!(file_name_for(&mk("a/../b.JPG", "jpg", 0)), "a___b.JPG");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn passwd_gid_parses_entries() {
        let pw = "root:x:0:0:root:/root:/bin/bash\nerik:x:1000:1000:Erik:/home/e:\\na\nerik/x:x:1001:33:x\ngone:x:1002:nope:g\n";
        assert_eq!(passwd_gid(pw, 0), Some(0));
        assert_eq!(passwd_gid(pw, 1000), Some(1000));
        assert_eq!(passwd_gid(pw, 1001), Some(33));
        assert_eq!(passwd_gid(pw, 1002), None, "gid 非数字 → 无解");
        assert_eq!(passwd_gid(pw, 999), None);
        assert_eq!(passwd_gid("", 0), None);
    }
}
