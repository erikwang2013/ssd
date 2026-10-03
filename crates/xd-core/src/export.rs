// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 恢复导出（父进程侧）：三重目标校验 → 起 `--export-worker` 子进程 → 转发进度/条目/终报。
//! 目标盘校验在父侧先做（同步错误码），子在降权前复核（权威，见 xd-daemon 的 `export_worker`）。
//!
//! **降权子进程模型**（同时写入 docs/security/linux-privilege-model.md）：父（可能是 root）
//! `spawn 自身 --export-worker` → 子**先按父权限开源设备 fd、做目标盘复核** → root 且
//! `PKEXEC_UID` 存在时 `setresuid` 降到调用者 → **此后所有文件写入均以普通用户身份**。
//! 父只转发子 stdout 的 JSON 行；子崩溃/被杀 = 导出终止（已写文件保留）；取消 = SIGTERM 子进程。
//!
//! 校验前提：`estimated = Σ size_bytes` 是**上界**（降级短交付件实际更短）——余量按上界判，
//! 宁严勿松。
//! 同盘判定（-32006）两道：精快路径 `st_dev(目标) == st_rdev(源)` + 盘级祖先（sysfs 走链，
//! 封「源=整盘 / 目标=其分区」；解析不到 sysfs 节点时 fail-open + stderr 留痕，见
//! `check_on_source_at`；残窗 = 父/子校验间换靶 TOCTOU，归 M4）。
//!
//! **可移植性（T9 修复轮）**：同盘判定与取消的 SIGTERM 均为 unix 实现（`#[cfg(unix)]`）；
//! 非 unix 无 dev_t / 无信号，故「源为物理设备」或「取消运行中作业」时**显式报
//! `PlatformUnsupported`**（绝不静默放行/假装已取消）。余量预检是 UX 预检、非安全边界，其
//! `statvfs` 在非 unix 缺席时仅 **warn 后跳过**（写失败由逐件 degraded/failed 报告兜底——报错
//! 会拖垮整条导出链而无安全收益）。运行时平台层（Win32 余量/信号）归 M1e-tail。

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(unix)]
use rustix::process::{Pid, Signal};
use serde_json::{Value, json};

use crate::notify::notification;
use crate::scan_task::NotifyFn;
use crate::store::{Store, StoreError};

pub const MAX_IDXS: usize = 100_000;
pub const MAX_REPORT_ITEMS: usize = 1000;
/// progress 转发节流（同 `scan.progress` 口径：≥250ms 一条）。
const PROGRESS_THROTTLE: Duration = Duration::from_millis(250);
/// 盘级祖先判定的 sysfs 根（生产值；测试经 `check_on_source_at` 注入假根）。
#[cfg(unix)]
const SYSFS_ROOT: &str = "/sys";

#[derive(Debug)]
pub enum ExportError {
    TaskNotFound(u64),
    EntryNotFound(u64),
    /// idxs 去重后为空（handlers 已挡原始空表；此为防御）。
    NoEntries,
    /// `export.cancel` 的未知 exportId → handlers 映射 -32602（重试语义即「已完成/不存在」）。
    ExportNotFound(u64),
    TargetOnSource(String),
    TargetNotWritable(String),
    /// 携带 `need`（= estimatedBytes）供 -32010 文案。
    InsufficientSpace(u64),
    /// 平台不支持（非 unix 无 dev_t 同盘判定 / 无 SIGTERM 取消）。**显式失败，不静默放行**——
    /// 静默 Ok 会让「写回源盘」或「UI 显示已取消而 worker 仍在写盘」无声通过（平台层归 M1e-tail）。
    PlatformUnsupported(String),
    Internal(String),
}

impl From<StoreError> for ExportError {
    fn from(e: StoreError) -> Self {
        ExportError::Internal(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportStarted {
    pub export_id: u64,
    pub file_count: u64,
    pub estimated_bytes: u64,
}

/// 去重（保持首现顺序）：契约「idxs 去重后非空且 ≤100000」的承重函数（handlers 与 manager 共用）。
pub fn dedupe_idxs(idxs: &[u64]) -> Vec<u64> {
    let mut seen = HashSet::with_capacity(idxs.len());
    idxs.iter().copied().filter(|i| seen.insert(*i)).collect()
}

/// 目标三重校验（父侧先做 + 子降权前复核共用）：存在且为目录 → 非源设备 → 余量 ≥ `estimated`。
///
/// `source_rdev` = 源为物理块设备时的 `(major, minor)`；镜像源为 `None`——**镜像不做同盘校验**
/// （目标是普通文件生态，写目标不碰镜像内容；文档注明「恢复目标勿选镜像所在盘的满盘」）。
/// 同盘判定为两道：精快路径 `st_dev(目标) == st_rdev(源)` + **盘级祖先**（封「源=整盘、
/// 目标=其分区」盲区，见 `check_on_source_at`）。
pub fn check_target(
    target: &Path,
    estimated: u64,
    source_rdev: Option<(u64, u64)>,
) -> Result<(), ExportError> {
    let dir = target.display().to_string();
    let md = std::fs::metadata(target).map_err(|_| ExportError::TargetNotWritable(dir.clone()))?;
    if !md.is_dir() {
        return Err(ExportError::TargetNotWritable(dir));
    }
    // dev_t 走 std 统一访问器（Linux/macOS 同为 u64；rustix 的 `st_dev` 在 macOS 是 i32）。
    #[cfg(unix)]
    check_on_source(
        major_minor(std::os::unix::fs::MetadataExt::dev(&md)),
        source_rdev,
        &dir,
    )?;
    // 非 unix 无 dev_t ⇒ 同盘判定不可用。源为物理设备时**显式报不支持**（不得静默 Ok：那会让
    // 「写回源盘」在 Windows 上无声通过）。当前 xd-device 非 Linux 恒给 None ⇒ 此臂实际不可达，
    // 是「平台层接线后仍不得静默」的保险。
    #[cfg(not(unix))]
    {
        if source_rdev.is_some() {
            return Err(ExportError::PlatformUnsupported(
                "same-device check is not implemented on this platform".to_string(),
            ));
        }
    }
    // 余量预检是 UX 预检，**不是安全边界**（写失败由逐件 degraded/failed 报告兜底）⇒ 非 unix 平台
    // 无 statvfs 时 warn 后放行，而不是像同盘校验/取消那样报 `PlatformUnsupported`：那会让整条导出链
    // 在 Windows 上运行时必败（连带把 happy/-32007/degraded 等平台中立的用例拖成红 → 只能整组门控 =
    // 静默丢覆盖）。真实现（GetDiskFreeSpaceEx 等）归 M1e-tail 平台层。
    #[cfg(unix)]
    {
        let vfs =
            rustix::fs::statvfs(target).map_err(|_| ExportError::TargetNotWritable(dir.clone()))?;
        check_space(vfs.f_bavail.saturating_mul(vfs.f_frsize), estimated)
    }
    #[cfg(not(unix))]
    {
        eprintln!(
            "warn: 本平台无 statvfs——跳过余量预检（需 {estimated} 字节；写失败将逐件报告）：{dir}"
        );
        Ok(())
    }
}

/// 同盘校验：① 精快路径（内核事实 `st_dev(目标) == st_rdev(源)`）② 盘级祖先（sysfs 走链，
/// 封「源=整盘 / 目标=其分区」盲区）。镜像源（None）双道皆免。
#[cfg(unix)]
fn check_on_source(
    target_dev: (u64, u64),
    source_rdev: Option<(u64, u64)>,
    dir: &str,
) -> Result<(), ExportError> {
    check_on_source_at(Path::new(SYSFS_ROOT), target_dev, source_rdev, dir)
}

/// `sysfs_root` 注入版（测试用假根；生产恒 `/sys`，见 `SYSFS_ROOT`）。
#[cfg(unix)]
fn check_on_source_at(
    sysfs_root: &Path,
    target_dev: (u64, u64),
    source_rdev: Option<(u64, u64)>,
    dir: &str,
) -> Result<(), ExportError> {
    let Some(src) = source_rdev else {
        return Ok(()); // 镜像源：不做同盘校验（见 check_target 头注）
    };
    if target_dev == src {
        return Err(ExportError::TargetOnSource(dir.to_string()));
    }
    match is_descendant_at(sysfs_root, target_dev, src) {
        Some(true) => Err(ExportError::TargetOnSource(dir.to_string())),
        // fail-open + 留痕（裁定见 security 文档 §6）：sysfs 节点缺（容器/异常环境）时退回
        // 精快路径结论。铁律要害是「别写回源盘」，精快路径已拦最常见形态；盘级判定是纵深，
        // 不让环境差异挡住正常导出。对照行为：改 fail-close 则 inject 测试③翻转。
        None => {
            eprintln!(
                "warn: 同盘盘级判定不可用（sysfs 无 {target_dev:?} 或 {src:?} 节点）——退回 rdev 相等判定"
            );
            Ok(())
        }
        Some(false) => Ok(()),
    }
}

/// 盘级祖先（注入根，纯 I/O 封装便于测试）：`target_dev` 的 sysfs 节点是否位于 `source_dev`
/// 节点之下（含相等；`Path::starts_with` 按路径分量比较，故 sdb1 是 sdb 的后代、sdb10 不是）。
/// `None` = 任一节点在 sysfs 解析不到（调用方 fail-open）。
///
/// `target_dev` 取 `stat(目标).st_dev` 即**目标所在文件系统的设备号**——与
/// `/proc/self/mountinfo` 第 3 字段是同一个值（`man proc`：「the value of st_dev for files on
/// this filesystem」；本机实测 8:22/8:21 两例一致），故不另解析挂载表（最长前缀匹配只会
/// 复现已有的一次 stat）。分区在 sysfs 里是整盘目录的子路径（`.../block/sdb/sdb1`，实测），
/// 故「祖先」正是「同盘且源不更细」。
#[cfg(unix)]
fn is_descendant_at(
    sysfs_root: &Path,
    target_dev: (u64, u64),
    source_dev: (u64, u64),
) -> Option<bool> {
    let canon = |(maj, min): (u64, u64)| {
        std::fs::canonicalize(sysfs_root.join(format!("dev/block/{maj}:{min}"))).ok()
    };
    Some(canon(target_dev)?.starts_with(canon(source_dev)?))
}

/// 余量校验（纯函数）：`f_bavail * f_frsize < estimated` → -32010（非 root 可用块，不用 f_blocks）。
/// 取值源 `statvfs` 为 unix 专属（见 `check_target` 尾段）⇒ 本函数随之 unix 门控（否则非 unix 无调用方）。
#[cfg(unix)]
fn check_space(avail: u64, estimated: u64) -> Result<(), ExportError> {
    if avail < estimated {
        Err(ExportError::InsufficientSpace(estimated))
    } else {
        Ok(())
    }
}

/// glibc dev_t 解码（gnu_dev_major/minor；与 `xd_device::linux::major_minor` 同式——那份仅
/// Linux 编译，xd-core 需跨 unix 编译故本地复用表达式）。
#[cfg(unix)]
fn major_minor(dev: u64) -> (u64, u64) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major, minor)
}

/// 单个导出作业（终态**保留**在表内——`export.cancel` 对终态须幂等回其终态，见 README）。
struct Job {
    canceled: AtomicBool,
    /// `Some("canceled"|"completed")` = 终态；`None` = 运行中。
    state: Mutex<Option<&'static str>>,
    /// spawn 时定格的子进程 pid：cancel **定向 kill，不借 child 句柄**。cancel 是 RPC 路径，
    /// 不得耦合转发线程的锁纪律——旧写法要锁 `child` 取 `id()`，一旦转发线程在 reap/wait 期间
    /// 持锁（worker 卡 D 态即长挂），整个 daemon 的 RPC 随之停摆。stale-pid 窗（reap 完成～
    /// state 落定之间）见 `cancel` 注释。
    pid: i32,
    /// 转发线程 reap 时 take。
    child: Mutex<Option<Child>>,
}

pub struct ExportManager {
    store: Arc<Store>,
    notify: NotifyFn,
    /// 子进程 `--db`：内存库（None）时导出不可用——worker 无从读任务/条目（诚实 -32603）。
    db_path: Option<PathBuf>,
    // ponytail: jobs 只增不删（每次导出一条终态记录，每条约百字节）；daemon 会话内导出次数有限，
    // 需长跑清理再加终态 LRU。
    jobs: Mutex<HashMap<u64, Arc<Job>>>,
    next_id: AtomicU64,
}

impl ExportManager {
    pub fn new(store: Arc<Store>, notify: NotifyFn, db_path: Option<PathBuf>) -> Self {
        Self {
            store,
            notify,
            db_path,
            jobs: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// 校验 → 起子进程 → 起转发线程。返回时子已 spawn（请求响应与导出生死解耦：失败逐件进终报）。
    pub fn start(
        &self,
        task_id: u64,
        idxs: &[u64],
        target_dir: &str,
        source_rdev: Option<(u64, u64)>,
    ) -> Result<ExportStarted, ExportError> {
        if self.store.task(task_id)?.is_none() {
            return Err(ExportError::TaskNotFound(task_id));
        }
        let idxs = dedupe_idxs(idxs);
        if idxs.is_empty() {
            return Err(ExportError::NoEntries);
        }
        if idxs.len() > MAX_IDXS {
            // handlers 已按去重后表长挡下（-32602）；此臂为公开 API 的纵深防御。
            return Err(ExportError::Internal(format!(
                "too many idxs: {}",
                idxs.len()
            )));
        }
        let mut names: HashMap<u64, String> = HashMap::new();
        let mut estimated: u64 = 0;
        for idx in &idxs {
            let e = self
                .store
                .entry(task_id, *idx)?
                .ok_or(ExportError::EntryNotFound(*idx))?;
            estimated = estimated.saturating_add(e.size_bytes);
            names.insert(*idx, e.name.clone());
        }
        check_target(Path::new(target_dir), estimated, source_rdev)?;
        let db = self
            .db_path
            .clone()
            .ok_or_else(|| ExportError::Internal("export requires a file-backed store".into()))?;

        let export_id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let child = self.spawn_worker(&db, task_id, export_id, target_dir, &idxs)?;
        let job = Arc::new(Job {
            canceled: AtomicBool::new(false),
            state: Mutex::new(None),
            pid: child.id() as i32,
            child: Mutex::new(Some(child)),
        });
        self.jobs.lock().unwrap().insert(export_id, job.clone());
        let stdout = job
            .child
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|c| c.stdout.take())
            .expect("child stdout is piped");
        let (notify, target, total) = (
            self.notify.clone(),
            target_dir.to_string(),
            idxs.len() as u64,
        );
        std::thread::Builder::new()
            .name(format!("export-{export_id}"))
            .spawn(move || forward(stdout, &job, &notify, export_id, total, &target, &names))
            .expect("spawn export forwarder");
        Ok(ExportStarted {
            export_id,
            file_count: total,
            estimated_bytes: estimated,
        })
    }

    /// 取消：未知 id → `ExportNotFound`（handlers → -32602）；运行中 → SIGTERM → `"canceled"`；
    /// 终态 → 幂等返回其终态（`"canceled"`/`"completed"`）。
    pub fn cancel(&self, export_id: u64) -> Result<&'static str, ExportError> {
        let jobs = self.jobs.lock().unwrap();
        let Some(job) = jobs.get(&export_id) else {
            return Err(ExportError::ExportNotFound(export_id));
        };
        if let Some(state) = *job.state.lock().unwrap() {
            return Ok(state);
        }
        // 先置标记再杀：转发线程 reap 后读到的必是「已取消」（否则终报会漏掉 canceled 标志）。
        #[cfg(unix)]
        {
            job.canceled.store(true, Ordering::SeqCst);
            // pid 定向 SIGTERM（不借 child 句柄/锁，见 Job.pid 注释）。
            // ESRCH 静默：reap 已完成～state 落定之间的 stale-pid 窗内目标可能已不在。
            match Pid::from_raw(job.pid) {
                Some(pid) => {
                    if let Err(e) = rustix::process::kill_process(pid, Signal::TERM)
                        && e != rustix::io::Errno::SRCH
                    {
                        eprintln!("warn: export {export_id} SIGTERM 失败：{e}");
                    }
                }
                None => eprintln!("warn: export {export_id} 无有效子进程 pid：{}", job.pid),
            }
            Ok("canceled")
        }
        // 非 unix 无信号可发（Windows 无 SIGTERM）：**先于置标记**显式报不支持——若置了 canceled
        // 再假装成功，UI 显示「已取消」而 worker 仍在写盘（静默撒谎）。平台层归 M1e-tail。
        #[cfg(not(unix))]
        {
            Err(ExportError::PlatformUnsupported(format!(
                "export cancel is not implemented on this platform (worker pid {})",
                job.pid
            )))
        }
    }

    /// 起子进程：`current_exe --export-worker --db … --task … --export-id … --target …`。
    /// `PKEXEC_UID` 随父环境继承（子自行读，见 export_worker 的权限序）。
    fn spawn_worker(
        &self,
        db: &Path,
        task_id: u64,
        export_id: u64,
        target_dir: &str,
        idxs: &[u64],
    ) -> Result<Child, ExportError> {
        let exe = std::env::current_exe()
            .map_err(|e| ExportError::Internal(format!("cannot locate daemon binary: {e}")))?;
        let mut child = Command::new(exe)
            .arg("--export-worker")
            .arg("--db")
            .arg(db)
            .arg("--task")
            .arg(task_id.to_string())
            .arg("--export-id")
            .arg(export_id.to_string())
            .arg("--target")
            .arg(target_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // 子留痕直接落 daemon stderr（可诊断）
            .spawn()
            .map_err(|e| ExportError::Internal(format!("cannot spawn export worker: {e}")))?;
        if let Some(mut sin) = child.stdin.take() {
            // 子可能已死（如 --db 打不开）：写失败不使请求失败——转发线程会以全 failed 终报。
            let payload = serde_json::to_string(&idxs)
                .map_err(|e| ExportError::Internal(format!("encode idxs: {e}")))?;
            if let Err(e) = writeln!(sin, "{payload}") {
                eprintln!("warn: export {export_id} 写子 stdin 失败：{e}");
            }
        } // sin drop = 关 stdin（子读到 EOF 后开跑）
        Ok(child)
    }
}

/// 转发线程：读子 stdout 的 JSON 行 → 节流转发 progress / 逐条收 item / 留痕 fatal / 收 finished；
/// 子退出后 reap 并**由父构造** `export.finished`（含 canceled 标志与截断标志），落终态。
fn forward(
    stdout: ChildStdout,
    job: &Job,
    notify: &NotifyFn,
    export_id: u64,
    total: u64,
    target_dir: &str,
    names: &HashMap<u64, String>,
) {
    let mut items: Vec<Value> = Vec::new();
    let mut degraded_seen: u64 = 0;
    let mut failed_seen: u64 = 0;
    let mut done_seen: u64 = 0;
    let mut written: u64 = 0;
    let mut counts: Option<(u64, u64, u64)> = None;
    let mut last_sent: Option<Instant> = None;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v["type"].as_str() {
            Some("progress") => {
                done_seen = v["done"].as_u64().unwrap_or(done_seen);
                written = v["writtenBytes"].as_u64().unwrap_or(written);
                if last_sent.is_none_or(|t| t.elapsed() >= PROGRESS_THROTTLE) {
                    notify(notification(
                        "export.progress",
                        json!({
                            "exportId": export_id, "done": done_seen, "total": total,
                            "writtenBytes": written, "elapsedMs": v["elapsedMs"].as_u64().unwrap_or(0),
                        }),
                    ));
                    last_sent = Some(Instant::now());
                }
            }
            Some("item") => {
                let status = v["status"].as_str().unwrap_or("failed");
                if status == "degraded" {
                    degraded_seen += 1;
                } else {
                    failed_seen += 1;
                }
                if items.len() < MAX_REPORT_ITEMS {
                    let idx = v["idx"].as_u64().unwrap_or(0);
                    // 名字以子的落盘实名为准（雕刻件 `carved_*`/去重 `_N` 只有子知道），
                    // 缺省回退父侧原始名。
                    let name = v["name"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| names.get(&idx).cloned())
                        .unwrap_or_default();
                    items.push(json!({
                        "idx": idx, "name": name, "status": status,
                        "reason": v["reason"].as_str().unwrap_or(""),
                    }));
                }
            }
            Some("fatal") => {
                eprintln!(
                    "warn: export {export_id} 子进程致命错误：{}",
                    v["reason"].as_str().unwrap_or("unknown")
                );
            }
            Some("finished") => {
                counts = Some((
                    v["succeeded"].as_u64().unwrap_or(0),
                    v["degraded"].as_u64().unwrap_or(0),
                    v["failed"].as_u64().unwrap_or(0),
                ));
            }
            _ => {}
        }
    }
    if let Some(mut c) = job.child.lock().unwrap().take() {
        let _ = c.wait(); // reap（cancel 的 SIGTERM / 正常退出都在此收口）
    }
    let canceled = job.canceled.load(Ordering::SeqCst);
    // 无 finished 行（被杀/崩溃/致命早退）：按已完成的 done 反推——未跑完的计 failed。
    let (succeeded, degraded, failed) = counts.unwrap_or_else(|| {
        let succeeded = done_seen.saturating_sub(degraded_seen + failed_seen);
        (
            succeeded,
            degraded_seen,
            total.saturating_sub(succeeded + degraded_seen),
        )
    });
    *job.state.lock().unwrap() = Some(if canceled { "canceled" } else { "completed" });
    notify(notification(
        "export.finished",
        json!({
            "exportId": export_id, "succeeded": succeeded, "degraded": degraded, "failed": failed,
            "canceled": canceled, "targetDir": target_dir, "items": items,
            "itemsTruncated": (degraded_seen + failed_seen) as usize > items.len(),
        }),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_keeps_first_order() {
        assert_eq!(dedupe_idxs(&[3, 1, 3, 2, 1]), vec![3, 1, 2]);
        assert!(dedupe_idxs(&[]).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn on_source_is_kernel_fact_equality() {
        // 精快路径：同设备 → -32006（带目标目录文案）；镜像源（None）→ 放行。
        // 假根缺节点（/nonexistent）⇒ 盘级判定 fail-open，恰好把本测钉在**精快路径**上。
        let empty = Path::new("/nonexistent-sysfs-root");
        assert!(matches!(
            check_on_source_at(empty, (8, 0), Some((8, 0)), "/mnt/usb/Recovered"),
            Err(ExportError::TargetOnSource(d)) if d == "/mnt/usb/Recovered"
        ));
        assert!(
            check_on_source_at(empty, (0, 43), None, "/x").is_ok(),
            "镜像源不做同盘校验"
        );
        assert!(
            check_on_source_at(empty, (8, 1), Some((8, 0)), "/x").is_ok(),
            "sysfs 缺节点 ⇒ fail-open（退回精快路径结论）"
        );
    }

    /// 假 sysfs 根（照 `xd_device::linux::BlockEnumerator::with_root` 先例）：
    /// `8:16 → .../block/sdb`（整盘）、`8:17/8:18 → .../block/sdb/sdb{1,2}`（其分区）。
    /// 生产 `/sys` 的真形态实测同构（sdb→sdb6），此处离线复刻以免依赖真机拓扑。
    #[cfg(unix)]
    fn fake_sysfs() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let disk = root.path().join("devices/pci0/block/sdb");
        std::fs::create_dir_all(&disk).unwrap();
        for (maj, min, rel) in [
            ("8", "16", "devices/pci0/block/sdb"),
            ("8", "17", "devices/pci0/block/sdb/sdb1"),
            ("8", "18", "devices/pci0/block/sdb/sdb2"),
            ("9", "0", "devices/pci0/block/sdc"),
        ] {
            let target = root.path().join(rel);
            std::fs::create_dir_all(&target).unwrap();
            let link = root.path().join(format!("dev/block/{maj}:{min}"));
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(Path::new("../../").join(rel), &link).unwrap();
        }
        root
    }

    #[cfg(unix)]
    #[test]
    fn partition_of_source_disk_is_rejected_by_ancestor_walk() {
        // 盲区封堵的牙：源=整盘 sdb(8,16)、目标=其分区 sdb1(8,17)——rdev 不相等，旧判定放行；
        // 盘级祖先链 .../block/sdb/sdb1 ⊃ .../block/sdb ⇒ 拒。删祖先判定 → 本测必红。
        let root = fake_sysfs();
        assert!(matches!(
            check_on_source_at(root.path(), (8, 17), Some((8, 16)), "/mnt/Recovered"),
            Err(ExportError::TargetOnSource(d)) if d == "/mnt/Recovered"
        ));
        // 方向性 pin（单向包含）：源更细（sdb1）、目标=整盘节点 ⇒ 放行——现实中有分区表的整盘
        // 挂不上文件系统（「目标=整盘节点」不成立）；此断言只钉「目标是源的后代才拒」的方向。
        assert!(
            check_on_source_at(root.path(), (8, 16), Some((8, 17)), "/x").is_ok(),
            "方向单向：目标须是源的后代"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sibling_and_other_disk_targets_pass() {
        // 兄弟分区（同盘不同设备：sdb2 vs 源 sdb1）与另一块盘（sdc）都不是祖先 ⇒ 放行。
        // 语义边界：契约拦的是「写回源设备这条链」，不是「写回同一块物理盘」。
        let root = fake_sysfs();
        assert!(
            check_on_source_at(root.path(), (8, 18), Some((8, 17)), "/x").is_ok(),
            "兄弟分区放行"
        );
        assert!(
            check_on_source_at(root.path(), (9, 0), Some((8, 16)), "/x").is_ok(),
            "另一块盘放行"
        );
    }

    /// 真机 sysfs 冒烟（本机 `--image` 态即此形态：根 fs 在 /dev/sdb6）：
    /// 读真 `/sys` 与真 `stat`，断言根 fs 设备既是自身后代、又是其整盘的（分区）后代。
    /// 环境无 /sys 节点（容器/非 Linux）→ 自跳过并留痕。
    /// 生产接线冒烟：`check_target` 走真 `/sys`。这是「`SYSFS_ROOT` 接线错/被改悬空假根」的牙——
    /// 那种错会被 fail-open 静默吞掉，本测必红。目标用真临时目录，源取其所在整盘的节点号
    /// （由 sysfs 父目录的 `dev` 文件推得）；无 sysfs 节点（容器/匿名设备/整盘无分区）→ 自跳过留痕。
    #[cfg(target_os = "linux")]
    #[test]
    fn check_target_uses_real_sysfs_ancestor_gate() {
        let dir = tempfile::tempdir().unwrap();
        let dev = major_minor(std::os::unix::fs::MetadataExt::dev(
            &std::fs::metadata(dir.path()).unwrap(),
        ));
        // 测试侧独立取真值：**字面 "/sys"**（不用 SYSFS_ROOT——否则接线错会被测试侧同源盲掉，
        // 该错正是本测要咬的目标）。
        let canon = match std::fs::canonicalize(
            PathBuf::from("/sys").join(format!("dev/block/{}:{}", dev.0, dev.1)),
        ) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skip: 临时目录设备 {dev:?} 无 sysfs 节点（{e}）");
                return;
            }
        };
        let disk = canon
            .parent()
            .and_then(|d| std::fs::read_to_string(d.join("dev")).ok())
            .and_then(|s| {
                let (maj, min) = s.trim().split_once(':')?;
                Some((maj.parse().ok()?, min.parse().ok()?))
            });
        let Some(disk) = disk.filter(|d| *d != dev) else {
            eprintln!("skip: {canon:?} 无整盘父节点（根 fs 直接在整盘上？）");
            return;
        };
        assert!(
            matches!(
                check_target(dir.path(), 0, Some(disk)),
                Err(ExportError::TargetOnSource(d)) if d == dir.path().display().to_string()
            ),
            "生产接线须走真 /sys 的盘级祖先：目标 {dev:?} 位于源整盘 {disk:?} 之下"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn real_sysfs_ancestor_smoke() {
        let dev = major_minor(std::os::unix::fs::MetadataExt::dev(
            &std::fs::metadata("/").unwrap(),
        ));
        let node = PathBuf::from("/sys").join(format!("dev/block/{}:{}", dev.0, dev.1));
        let canon = match std::fs::canonicalize(&node) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "skip: {} 不可解析（{e}）——无真 sysfs 拓扑可核",
                    node.display()
                );
                return;
            }
        };
        assert_eq!(
            is_descendant_at(Path::new(SYSFS_ROOT), dev, dev),
            Some(true),
            "设备是自身的后代：{canon:?}"
        );
        // 父目录的 `dev` 文件 = 整盘节点号（分区在 sysfs 里是整盘子目录）：真机核祖先链一跳
        let parent_dev = canon.parent().and_then(|d| {
            std::fs::read_to_string(d.join("dev")).ok().and_then(|s| {
                let (maj, min) = s.trim().split_once(':')?;
                Some((maj.parse().ok()?, min.parse().ok()?))
            })
        });
        match parent_dev {
            Some(p) => assert_eq!(
                is_descendant_at(Path::new(SYSFS_ROOT), dev, p),
                Some(true),
                "根 fs 设备 {canon:?} 应为其整盘 {p:?} 的后代"
            ),
            None => eprintln!("skip: {canon:?} 无父 dev（根 fs 直接在整盘上？）——自后代已过"),
        }
    }

    /// 非 unix 的牙：源为物理设备 ⇒ 同盘判定不可用 ⇒ **显式 `PlatformUnsupported`**（静默 Ok 是
    /// 缺陷：会让「写回源盘」无声通过）。镜像源（None）不受影响，继续走余量校验。仅非 unix 编译
    /// （unix 上走真判定，此断言不成立）。
    #[cfg(not(unix))]
    #[test]
    fn same_device_check_is_explicitly_unsupported_off_unix() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            check_target(dir.path(), 0, Some((8, 0))),
            Err(ExportError::PlatformUnsupported(_))
        ));
        assert!(
            check_target(dir.path(), 0, None).is_ok(),
            "镜像源不做同盘校验，不得被平台守卫误伤"
        );
    }

    #[cfg(unix)]
    #[test]
    fn space_check_uses_estimated_upper_bound() {
        assert!(check_space(16007, 16007).is_ok(), "恰好够用放行");
        assert!(matches!(
            check_space(16006, 16007),
            Err(ExportError::InsufficientSpace(16007))
        ));
        assert!(check_space(u64::MAX, 0).is_ok(), "空导出不欠空间");
    }

    #[test]
    fn check_target_missing_dir_is_not_writable() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(matches!(
            check_target(&missing, 1, None),
            Err(ExportError::TargetNotWritable(_))
        ));
        // 存在但是文件（非目录）→ 同码
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            check_target(&file, 1, None),
            Err(ExportError::TargetNotWritable(_))
        ));
        // 目录 + 镜像源 + 余量充足 → 放行
        assert!(check_target(dir.path(), 1024, None).is_ok());
    }

    fn mgr() -> (Arc<Store>, ExportManager) {
        let store = Arc::new(Store::open_memory().unwrap());
        let m = ExportManager::new(store.clone(), Arc::new(|_| {}), None);
        (store, m)
    }

    fn entry(idx: u64, name: &str) -> crate::api::ScanEntry {
        crate::api::ScanEntry {
            idx,
            name: name.into(),
            path: "/".into(),
            ext: "jpg".into(),
            size_bytes: 10 + idx,
            deleted: false,
            is_dir: false,
            quality: "complete".into(),
            first_cluster: 6,
            byte_offset: None,
            contiguous: None,
            record_id: None,
        }
    }

    #[test]
    fn start_rejects_unknown_task_entry_and_empty_idxs() {
        let (store, m) = mgr();
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().to_str().unwrap();
        assert!(matches!(
            m.start(9, &[0], t, None),
            Err(ExportError::TaskNotFound(9))
        ));
        let id = store.create_task("d", "exfat", "quick", 4096).unwrap();
        store.insert_entries(id, &[entry(0, "A.JPG")]).unwrap();
        assert!(matches!(
            m.start(id, &[], t, None),
            Err(ExportError::NoEntries)
        ));
        assert!(matches!(
            m.start(id, &[7], t, None),
            Err(ExportError::EntryNotFound(7))
        ));
        // 校验顺序：条目先于目标（目标坏也须先报 -32008；错误码优先级是契约面）
        assert!(matches!(
            m.start(id, &[7], "/nonexistent-target", None),
            Err(ExportError::EntryNotFound(7))
        ));
    }

    #[test]
    fn start_without_db_fails_after_checks() {
        // 内存库（无 --db）：条目/目标校验通过后诚实报内部错（worker 无从读库），不静默起子进程
        let (store, m) = mgr();
        let id = store.create_task("d", "exfat", "quick", 4096).unwrap();
        store.insert_entries(id, &[entry(0, "A.JPG")]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            m.start(id, &[0], dir.path().to_str().unwrap(), None),
            Err(ExportError::Internal(_))
        ));
    }

    #[test]
    fn cancel_unknown_export_is_not_found_terminal_is_idempotent() {
        let (_, m) = mgr();
        assert!(matches!(m.cancel(42), Err(ExportError::ExportNotFound(42))));
        // 注入一条终态作业（不经子进程）：cancel 幂等原样返回其终态
        let job = Arc::new(Job {
            canceled: AtomicBool::new(true),
            state: Mutex::new(Some("canceled")),
            pid: 0,
            child: Mutex::new(None),
        });
        m.jobs.lock().unwrap().insert(1, job);
        assert_eq!(m.cancel(1).unwrap(), "canceled");
        m.jobs
            .lock()
            .unwrap()
            .get(&1)
            .unwrap()
            .state
            .lock()
            .unwrap()
            .replace("completed");
        assert_eq!(m.cancel(1).unwrap(), "completed");
    }

    #[test]
    fn cancel_running_job_kills_by_pid_without_child_handle() {
        // 运行中作业 + 无 child 句柄（pid=0 非法）：cancel 仍返回 "canceled"、置标记、不 panic
        // ——pid 定向 kill 不依赖 child mutex（本测的树里 child 恒 None，锁永不参与）。
        let (_, m) = mgr();
        let job = Arc::new(Job {
            canceled: AtomicBool::new(false),
            state: Mutex::new(None),
            pid: 0,
            child: Mutex::new(None),
        });
        m.jobs.lock().unwrap().insert(7, job.clone());
        // 非 unix 无信号：**显式报平台不支持，且不得置 canceled**——置了就是「UI 显示已取消而
        // worker 仍在写盘」的静默撒谎（本断言即该铁律的牙）。
        #[cfg(not(unix))]
        {
            assert!(matches!(
                m.cancel(7),
                Err(ExportError::PlatformUnsupported(_))
            ));
            assert!(!job.canceled.load(Ordering::SeqCst), "不得假装已取消");
            assert!(job.state.lock().unwrap().is_none(), "终态不得被改写");
        }
        #[cfg(unix)]
        {
            assert_eq!(m.cancel(7).unwrap(), "canceled");
            assert!(job.canceled.load(Ordering::SeqCst), "取消标记已置");
            assert!(job.state.lock().unwrap().is_none(), "终态由转发线程落定");
        }
    }

    #[test]
    fn target_checks_precede_spawn() {
        // 目标不存在 → -32007（条目齐备也拦在 spawn 前：单元环境不起子进程）
        let (store, m) = mgr();
        let id = store.create_task("d", "exfat", "quick", 4096).unwrap();
        store.insert_entries(id, &[entry(0, "A.JPG")]).unwrap();
        assert!(matches!(
            m.start(id, &[0], "/nonexistent-target", None),
            Err(ExportError::TargetNotWritable(_))
        ));
    }
}
