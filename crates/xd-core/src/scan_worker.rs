// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 崩溃隔离边界在 `run_worker` 的 catch_unwind（取消 ≠ 故障：按 `ScanCanceled` 类型区分）；
//! 对 crate 内其余模块仅暴露 `run_worker`（公开 API 一律走 `crate::scan_task`）。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use xd_device::{BlockDevice, DeviceError, DeviceInfo};

use crate::api::{ScanEntry, ScanState};
use crate::scan_task::{Ctrl, FsKind, NotifyFn, ScanCanceled, probe};
use crate::store::{Store, state_str};

/// 读字节计数包装（观察者进度 = 真实读量，零引擎改动）。
struct CountingDev {
    inner: Arc<dyn BlockDevice>,
    bytes: AtomicU64,
}

impl BlockDevice for CountingDev {
    fn info(&self) -> &DeviceInfo {
        self.inner.info()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        let n = self.inner.read_at(offset, buf)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

struct Progress<'a> {
    task_id: u64,
    store: &'a Store,
    notify: &'a dyn Fn(Value),
    ctrl: &'a Ctrl,
    bytes: &'a AtomicU64,
    start: Instant,
    found: u64,
    last_notify_at: Instant,
    last_notify_bytes: u64,
}

impl Progress<'_> {
    /// 条目边界：取消 → unwind 标记；暂停 → 驻停轮询（驻停中亦响应取消）。
    fn checkpoint(&self) {
        if self.ctrl.canceled.load(Ordering::SeqCst) {
            std::panic::panic_any(ScanCanceled);
        }
        while self.ctrl.paused.load(Ordering::SeqCst) {
            if self.ctrl.canceled.load(Ordering::SeqCst) {
                std::panic::panic_any(ScanCanceled);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// 一条目：检查点 → 编号落盘 → 节流进度（≥250ms 或读量增量 ≥1MiB）。
    fn on_entry(&mut self, mut entry: ScanEntry) {
        self.checkpoint();
        entry.idx = self.found;
        self.found += 1;
        let _ = self
            .store
            .insert_entries(self.task_id, std::slice::from_ref(&entry)); // 库错不中断扫描
        let now = Instant::now();
        let read = self.bytes.load(Ordering::Relaxed);
        if now.duration_since(self.last_notify_at) >= Duration::from_millis(250)
            || read.saturating_sub(self.last_notify_bytes) >= 1024 * 1024
        {
            let elapsed = self.start.elapsed().as_millis() as u64;
            let _ = self
                .store
                .set_progress(self.task_id, read, self.found, elapsed);
            (self.notify)(crate::notify::notification(
                "scan.progress",
                json!({
                    "taskId": self.task_id, "state": "scanning",
                    "readBytes": read, "foundCount": self.found, "elapsedMs": elapsed,
                }),
            ));
            self.last_notify_at = now;
            self.last_notify_bytes = read;
        }
    }
}

fn fat_to_entry(e: &xd_fs_fat::scan::FatEntry) -> ScanEntry {
    ScanEntry {
        idx: 0,
        name: e.name.clone(),
        path: e.path.clone(),
        ext: e.ext.clone(),
        size_bytes: e.size_bytes,
        deleted: e.deleted,
        is_dir: e.is_dir,
        quality: match e.quality {
            xd_fs_fat::scan::RecoverQuality::Complete => "complete".into(),
            xd_fs_fat::scan::RecoverQuality::MaybeDamaged => "maybeDamaged".into(),
        },
        first_cluster: e.first_cluster,
    }
}

fn exfat_to_entry(e: &xd_fs_exfat::scan::ExfatEntry) -> ScanEntry {
    ScanEntry {
        idx: 0,
        name: e.name.clone(),
        path: e.path.clone(),
        ext: e.ext.clone(),
        size_bytes: e.size_bytes,
        deleted: e.deleted,
        is_dir: e.is_dir,
        quality: match e.quality {
            xd_fs_exfat::scan::RecoverQuality::Complete => "complete".into(),
            xd_fs_exfat::scan::RecoverQuality::MaybeDamaged => "maybeDamaged".into(),
        },
        first_cluster: e.first_cluster,
    }
}

/// 线程体：扫描（崩溃隔离 + 取消 unwind）→ 终态置态（条件写，不覆写 cancel 已置态）→ 通知。
pub(crate) fn run_worker(
    id: u64,
    device: Arc<dyn BlockDevice>,
    ctrl: Arc<Ctrl>,
    store: Arc<Store>,
    notify: NotifyFn,
) {
    let start = Instant::now();
    let counting = CountingDev {
        inner: device,
        bytes: AtomicU64::new(0),
    };
    let mut progress = Progress {
        task_id: id,
        store: &store,
        notify: &*notify,
        ctrl: &ctrl,
        bytes: &counting.bytes,
        start,
        found: 0,
        last_notify_at: start,
        last_notify_bytes: 0,
    };
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
            match probe(&counting).map_err(|_| "unsupported fs".to_string())? {
                FsKind::Fat => xd_fs_fat::scan::scan_with_observer(&counting, &mut |e| {
                    progress.on_entry(fat_to_entry(e));
                })
                .map(|_| ())
                .map_err(|e| e.to_string()),
                FsKind::Exfat => xd_fs_exfat::scan::scan_with_observer(&counting, &mut |e| {
                    progress.on_entry(exfat_to_entry(e));
                })
                .map(|_| ())
                .map_err(|e| e.to_string()),
            }
        }));
    let (state, msg) = match outcome {
        Ok(Ok(())) => (ScanState::Completed, None),
        Ok(Err(e)) => (ScanState::Failed, Some(e)),
        Err(p) if p.downcast_ref::<ScanCanceled>().is_some() => (ScanState::Canceled, None),
        Err(_) => (ScanState::Failed, Some("scan worker panicked".into())),
    };
    let elapsed = start.elapsed().as_millis() as u64;
    let read = counting.bytes.load(Ordering::Relaxed);
    let _ = store.set_progress(id, read, progress.found, elapsed);
    // 条件置态：cancel() 已先置 Canceled 时不覆写（终态竞态护栏）
    let _ = store.set_state_if_active(id, state);
    (notify)(crate::notify::notification(
        "scan.finished",
        json!({
            "taskId": id, "state": state_str(state),
            "foundCount": progress.found, "elapsedMs": elapsed,
        }),
    ));
    if let Some(m) = msg {
        eprintln!("warn: scan task {id} failed: {m}");
    }
}
