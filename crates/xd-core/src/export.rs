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

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal};
use serde_json::{Value, json};

use crate::notify::notification;
use crate::scan_task::NotifyFn;
use crate::store::{Store, StoreError};

pub const MAX_IDXS: usize = 100_000;
pub const MAX_REPORT_ITEMS: usize = 1000;
/// progress 转发节流（同 `scan.progress` 口径：≥250ms 一条）。
const PROGRESS_THROTTLE: Duration = Duration::from_millis(250);

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
    let st = rustix::fs::stat(target).map_err(|_| ExportError::TargetNotWritable(dir.clone()))?;
    check_on_source(major_minor(st.st_dev), source_rdev, &dir)?;
    let vfs =
        rustix::fs::statvfs(target).map_err(|_| ExportError::TargetNotWritable(dir.clone()))?;
    check_space(vfs.f_bavail.saturating_mul(vfs.f_frsize), estimated)
}

/// 同盘校验（纯函数：内核事实 `st_dev(目标) == st_rdev(源块设备)`；测试注入假值）。
fn check_on_source(
    target_dev: (u64, u64),
    source_rdev: Option<(u64, u64)>,
    dir: &str,
) -> Result<(), ExportError> {
    match source_rdev {
        Some(rdev) if rdev == target_dev => Err(ExportError::TargetOnSource(dir.to_string())),
        _ => Ok(()),
    }
}

/// 余量校验（纯函数）：`f_bavail * f_frsize < estimated` → -32010（非 root 可用块，不用 f_blocks）。
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
    /// 转发线程 reap 时 take；cancel 借同一句柄发 SIGTERM。
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
        job.canceled.store(true, Ordering::SeqCst);
        if let Some(child) = job.child.lock().unwrap().as_mut()
            && let Some(pid) = Pid::from_raw(child.id() as i32)
            && let Err(e) = rustix::process::kill_process(pid, Signal::TERM)
        {
            eprintln!("warn: export {export_id} SIGTERM 失败：{e}");
        }
        Ok("canceled")
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

    #[test]
    fn on_source_is_kernel_fact_equality() {
        // 同设备 → -32006（带目标目录文案）；异设备/镜像源（None）→ 放行
        assert!(matches!(
            check_on_source((8, 0), Some((8, 0)), "/mnt/usb/Recovered"),
            Err(ExportError::TargetOnSource(d)) if d == "/mnt/usb/Recovered"
        ));
        assert!(
            check_on_source((8, 1), Some((8, 0)), "/x").is_ok(),
            "异分区放行"
        );
        assert!(
            check_on_source((0, 43), None, "/x").is_ok(),
            "镜像源不做同盘校验"
        );
    }

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
