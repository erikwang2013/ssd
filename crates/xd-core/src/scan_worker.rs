// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 崩溃隔离边界在 worker 的 catch_unwind（取消 ≠ 故障：快扫按 `ScanCanceled` 类型区分）；
//! 对 crate 内其余模块仅暴露两条线程体：`run_worker`（快扫）/`run_carve_worker`（深扫）
//! ——公开 API 一律走 `crate::scan_task`。

use std::ops::Range;
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
        byte_offset: None,
        contiguous: None, // fat 无 NoFatChain 概念：拓扑由 deleted 决定（删除件走连续回退）
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
        byte_offset: None,
        contiguous: Some(e.contiguous), // exfat 拓扑提示：反构造读取时承重（NoFatChain/链）
    }
}

/// 断点前已尝试扫描的字节（任务级累计坐标）：续跑时 readBytes 自断点续起而非回零，
/// 收尾仍恰为 Σrun 长（= `totalBytes`，百分比 100%）。断点落于 run 内时含该 run 的前缀段。
fn scanned_base(runs: &[Range<u64>], resume_from: u64) -> u64 {
    let mut base = 0;
    for r in runs {
        if resume_from >= r.end {
            base += r.end - r.start; // 整段在断点前
        } else {
            base += resume_from.saturating_sub(r.start); // 断点在本 run 内：前缀段
            break;
        }
    }
    base
}

/// 深扫线程体：雕刻（自家循环 → 取消即回调返回 false）→ 终态置态 → 通知。
/// 与快扫的**有意不对称**：`run_worker` 的取消必须走 `ScanCanceled` unwind（引擎回调不返回
/// 控制值），而雕刻回调**返回 false 即停**——语义更直接，故 carve 路径不产生 unwind；
/// catch_unwind 在此仅兜真 panic 的崩溃隔离。
///
/// `resume_from`/`next_idx`：首跑传 0/0；断点续跑传检查点与已落库条目数（idx 续号，
/// `INSERT OR REPLACE` 使重扫区间同编号覆盖而非追加）。
#[allow(clippy::too_many_arguments)] // 与 WorkerFn 线程体同构：参数即入参，不引入结构体
pub(crate) fn run_carve_worker(
    id: u64,
    device: Arc<dyn BlockDevice>,
    runs: Vec<Range<u64>>,
    resume_from: u64,
    next_idx: u64,
    ctrl: Arc<Ctrl>,
    store: Arc<Store>,
    notify: NotifyFn,
) {
    let start = Instant::now();
    let base = scanned_base(&runs, resume_from);
    let mut cb = CarveProgress {
        task_id: id,
        store: &store,
        notify: &*notify,
        ctrl: &ctrl,
        start,
        found: next_idx,
        scanned: base,
        progress_base: base,
        last_notify_at: start,
        last_notify_bytes: base,
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        xd_carving::carve_runs_from(
            &*device,
            &runs,
            resume_from,
            xd_carving::MAX_FILE_BYTES,
            &mut |ev| cb.on(ev),
        )
    }));
    let (state, msg): (ScanState, Option<String>) = match outcome {
        Ok(_) if ctrl.canceled.load(Ordering::SeqCst) => (ScanState::Canceled, None),
        Ok(_) => (ScanState::Completed, None),
        Err(_) => (ScanState::Failed, Some("carve worker panicked".into())),
    };
    let elapsed = start.elapsed().as_millis() as u64;
    // 终局进度 = 已扫描字节（口径同 totalBytes=Σ空闲区间，百分比封顶 100%）
    let _ = store.set_progress(id, cb.scanned, cb.found, elapsed);
    let _ = store.set_state_if_active(id, state);
    (notify)(crate::notify::notification(
        "scan.finished",
        json!({
            "taskId": id, "state": state_str(state),
            "foundCount": cb.found, "elapsedMs": elapsed,
        }),
    ));
    if let Some(m) = msg {
        eprintln!("warn: carve task {id} failed: {m}");
    }
}

/// 雕刻事件回调：返回 false = 停止（取消）；暂停在回调内驻停（驻停中亦响应取消）。
/// `readBytes` 口径 = **已扫描**字节（任务级：断点前累计 + 本次；与 `totalBytes`=Σ空闲区间
/// 对齐，百分比不超 100%）。
///
/// **驻停次序与检查点不变量（T7 契约，spec-t7 阻断修复后口径）**：`Scanned` 先落**配对**
/// 检查点（`carved_offset` + `found_count` 同帧）再驻停；`Entry` 先驻停再落盘。不变量：
/// **凡 `idx < found_count` 的已落库条目，其 `byte_offset < carved_offset`**——续跑自
/// `found_count` 续号、自 `carved_offset` 起扫，同号 `INSERT OR REPLACE` 只可能覆盖本窗口
/// 重扫的等价条目（幂等）；断点前的旧行 idx 恒小于续号起点，永不被覆盖。取消检查恒在最先
/// （响应性不因次序变化）。
struct CarveProgress<'a> {
    task_id: u64,
    store: &'a Store,
    notify: &'a dyn Fn(Value),
    ctrl: &'a Ctrl,
    start: Instant,
    found: u64,
    scanned: u64,
    /// 断点前累计（任务级进度基准，见 `scanned_base`）。
    progress_base: u64,
    last_notify_at: Instant,
    last_notify_bytes: u64,
}

impl CarveProgress<'_> {
    fn on(&mut self, ev: xd_carving::CarveEvent<'_>) -> bool {
        if self.ctrl.canceled.load(Ordering::SeqCst) {
            return false;
        }
        match ev {
            xd_carving::CarveEvent::Scanned { scanned, at } => {
                self.scanned = self.progress_base + scanned;
                // 检查点是**安全机制**，不参与进度节流：每窗口一条小 UPDATE（≤4MiB 粒度）。
                // **配对写**：`found_count` 与断点同帧落盘。此刻本窗口条目尚未产出，
                // `self.found` 恰 = 「断点（at）之前已落库条目数」——两坐标一致，重启后
                // idx 自 found_count 续号才不会覆盖断点前的旧行（spec-t7 阻断的丢条根因）。
                let _ = self.store.set_carved_offset(self.task_id, at, self.found);
                let now = Instant::now();
                if now.duration_since(self.last_notify_at) >= Duration::from_millis(250)
                    || self.scanned.saturating_sub(self.last_notify_bytes) >= 1024 * 1024
                {
                    let elapsed = self.start.elapsed().as_millis() as u64;
                    let _ =
                        self.store
                            .set_progress(self.task_id, self.scanned, self.found, elapsed);
                    (self.notify)(crate::notify::notification(
                        "scan.progress",
                        json!({
                            "taskId": self.task_id, "state": "scanning",
                            "readBytes": self.scanned, "foundCount": self.found, "elapsedMs": elapsed,
                        }),
                    ));
                    self.last_notify_at = now;
                    self.last_notify_bytes = self.scanned;
                }
                if !self.park_if_paused() {
                    return false;
                }
            }
            xd_carving::CarveEvent::Entry(e) => {
                if !self.park_if_paused() {
                    return false;
                }
                let entry = ScanEntry {
                    idx: self.found,
                    name: String::new(),
                    path: String::new(),
                    ext: e.signature.ext().to_string(),
                    size_bytes: e.size,
                    deleted: true,
                    is_dir: false,
                    quality: "carved".into(),
                    first_cluster: 0,
                    byte_offset: Some(e.byte_offset),
                    contiguous: None, // 雕刻件无拓扑：读取走 read_back（byte_offset 分支）
                };
                self.found += 1; // idx 先占位后自增：条目编号与 found 计数恒一致
                let _ = self.store.insert_entries(
                    self.task_id,
                    std::slice::from_ref(&entry), // 库错不中断扫描（found/idx 照进）。注意：该编号会随配对检查点越过断点，缺失行此后不重扫（库错路径的已知局限，非检查点丢条）
                );
            }
        }
        true
    }

    /// 驻停（驻停中亦响应取消）。返回 false = 取消。
    fn park_if_paused(&self) -> bool {
        while self.ctrl.paused.load(Ordering::SeqCst) {
            if self.ctrl.canceled.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        true
    }
}

/// 线程体：快扫（崩溃隔离 + 取消 unwind）→ 终态置态（条件写，不覆写 cancel 已置态）→ 通知。
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个借用局部 store/notify 的 CarveProgress（线程内自持资源）。
    fn in_thread(
        ctrl: Arc<Ctrl>,
        ev: xd_carving::CarveEvent<'static>,
    ) -> std::thread::JoinHandle<bool> {
        std::thread::spawn(move || {
            let store = Store::open_memory().unwrap();
            let notify: NotifyFn = Arc::new(|_| {});
            let start = Instant::now();
            let mut cb = CarveProgress {
                task_id: 1,
                store: &store,
                notify: &*notify,
                ctrl: &ctrl,
                start,
                found: 0,
                scanned: 0,
                progress_base: 0,
                last_notify_at: start,
                last_notify_bytes: 0,
            };
            cb.on(ev)
        })
    }

    #[test]
    fn carve_progress_pauses_inside_callback_and_cancel_wins() {
        // 深扫的驻停/取消**在回调内**（无 unwind 通道）：paused → on() 不返回；
        // 驻停中置 canceled → on() 立即返回 false。
        let ctrl = Arc::new(Ctrl::default());
        ctrl.paused.store(true, Ordering::SeqCst);
        let h = in_thread(
            ctrl.clone(),
            xd_carving::CarveEvent::Scanned { scanned: 0, at: 0 },
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(!h.is_finished(), "paused 时 on() 必须驻停不返回");
        ctrl.canceled.store(true, Ordering::SeqCst);
        assert!(!h.join().unwrap(), "驻停中取消：on() 返回 false = 停");
        // 对照：未驻停 → 立即返回 true（扫描继续）
        let h2 = in_thread(
            Arc::new(Ctrl::default()),
            xd_carving::CarveEvent::Scanned { scanned: 0, at: 0 },
        );
        assert!(h2.join().unwrap());
    }

    #[test]
    fn scanned_base_counts_prefix_before_checkpoint() {
        // 任务级进度基准：整段在断点前的 run 全计；断点所在 run 只计前缀；断点之后不计
        let runs = [0..1000u64, 2000..3000, 5000..6000];
        assert_eq!(scanned_base(&runs, 0), 0, "首跑无基准");
        assert_eq!(scanned_base(&runs, 500), 500, "断点在首 run 内：前缀段");
        assert_eq!(scanned_base(&runs, 1000), 1000, "恰在 run 界");
        assert_eq!(scanned_base(&runs, 2500), 1000 + 500, "跨 run：整段 + 前缀");
        assert_eq!(
            scanned_base(&runs, 4500),
            2000,
            "断点在 run 间隙（Rust 半开区间）"
        );
        assert_eq!(scanned_base(&runs, 6000), 3000, "越界：全量");
        assert_eq!(scanned_base(&runs, 9999), 3000, "远超：全量");
    }
}
