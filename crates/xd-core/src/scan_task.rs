// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 扫描任务编排：状态机（暂停/取消）、worker 线程、崩溃隔离（catch_unwind）、进度回调与
//! 流式落盘（store）。daemon 只做传输与线程托管（设计 §4.4）。
//! 取消用 `panic_any(ScanCanceled)` 在条目边界 unwind——worker 的 catch_unwind 按类型区分
//! （取消 ≠ 故障）；daemon 侧 panic hook 对该标记静默。
//! worker 内部（计数设备 / 进度节流 / 引擎条目适配 / 线程体）见 `crate::scan_worker`。

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use xd_device::BlockDevice;

use crate::api::{ScanEntry, ScanState};
use crate::store::{Store, StoreError, TaskRow};

/// 取消用的 unwind 标记（见模块头注）。
#[derive(Debug)]
pub struct ScanCanceled;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Fat,
    Exfat,
}

impl FsKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FsKind::Fat => "fat",
            FsKind::Exfat => "exfat",
        }
    }
}

/// 反解析（导出子进程按 task 行的 `fs` 字符串重建；未知值 → `ProbeError::Unsupported`
/// ——载体是既有「不支持的卷」错误，调用侧一律当致命处理）。
impl std::str::FromStr for FsKind {
    type Err = ProbeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fat" => Ok(FsKind::Fat),
            "exfat" => Ok(FsKind::Exfat),
            _ => Err(ProbeError::Unsupported),
        }
    }
}

#[derive(Debug)]
pub enum ProbeError {
    Unsupported,
}

/// 引导扇区签名粗筛（唯一 1 个扇区读）。exFAT 签名定死；FAT 做 0x55AA + BPB 字段合理性
/// 粗筛，完整校验留给引擎（worker 内 parse 失败 → 任务 failed）。
pub fn probe(dev: &dyn BlockDevice) -> Result<FsKind, ProbeError> {
    let mut buf = [0u8; 512];
    let n = dev
        .read_at(0, &mut buf)
        .map_err(|_| ProbeError::Unsupported)?;
    if n >= 11 && &buf[3..11] == b"EXFAT   " {
        return Ok(FsKind::Exfat);
    }
    if n >= 512 {
        let bps = u16::from_le_bytes([buf[11], buf[12]]);
        let spc = buf[13];
        let fats = buf[16];
        let sig_ok = buf[510] == 0x55 && buf[511] == 0xAA;
        if sig_ok
            && matches!(bps, 512 | 1024 | 2048 | 4096)
            && spc.is_power_of_two()
            && (1..=2).contains(&fats)
        {
            return Ok(FsKind::Fat);
        }
    }
    Err(ProbeError::Unsupported)
}

#[derive(Debug)]
pub enum ScanError {
    TaskNotFound(u64),
    TaskNotActive(u64),
    UnsupportedFs,
    /// 空闲空间不可判定（深扫前置：exfat 位图 / FAT 表不可读）→ -32005。
    /// 与「无空闲」必须区分：后者是合法空扫（total_bytes=0），前者是拒绝。
    UnallocatedUnavailable,
    Store(StoreError),
    Internal(String),
}

impl From<StoreError> for ScanError {
    fn from(e: StoreError) -> Self {
        ScanError::Store(e)
    }
}

/// 按 id 懒打开设备——device.list 零 open 铁律的出口：只有 scan.start/resume 重开才 open。
#[derive(Debug)]
pub enum OpenError {
    PermissionDenied,
    Other(String),
}

pub trait DeviceOpener: Send + Sync {
    fn open(&self, id: &str) -> Result<Arc<dyn BlockDevice>, OpenError>;
}

/// 默认 opener：不懒打开任何设备（测试与非 Linux daemon 用）。
pub struct NoopOpener;

impl DeviceOpener for NoopOpener {
    fn open(&self, id: &str) -> Result<Arc<dyn BlockDevice>, OpenError> {
        Err(OpenError::Other(format!("no opener configured: {id}")))
    }
}

/// worker 线程体：入参恒为 (id, device, ctrl, store, notify)；deep 的额外状态（runs）由闭包携带。
type WorkerFn = Box<dyn FnOnce(u64, Arc<dyn BlockDevice>, Arc<Ctrl>, Arc<Store>, NotifyFn) + Send>;

/// 空闲区间枚举分派（两引擎错误一律收敛为 UnallocatedUnavailable——上层只回 -32005，
/// 错误细节只进日志）。空 Vec 是合法结果（无空闲 = 空扫），不得与 Err 混同。
fn unallocated_runs_of(dev: &dyn BlockDevice, fs: FsKind) -> Result<Vec<Range<u64>>, ScanError> {
    // 两引擎错误类型不同（FatError/ExfatError）：此处抹成 () 收敛（细节进日志，不回客户端）
    let r = match fs {
        FsKind::Fat => xd_fs_fat::freespace::unallocated_runs(dev).map_err(|e| {
            eprintln!("warn: fat freespace unavailable: {e:?}");
        }),
        FsKind::Exfat => xd_fs_exfat::freespace::unallocated_runs(dev).map_err(|e| {
            eprintln!("warn: exfat freespace unavailable: {e:?}");
        }),
    };
    r.map_err(|()| ScanError::UnallocatedUnavailable)
}

pub struct ScanStarted {
    pub task_id: u64,
    pub fs: FsKind,
    pub total_bytes: u64,
}

/// resume 的两种走向：InPlace=worker 在场或已目标态；NeedsDevice=重启后需 handlers 重新打开设备。
pub enum Resume {
    InPlace,
    NeedsDevice { device_id: String },
}

pub type NotifyFn = Arc<dyn Fn(Value) + Send + Sync>;

/// 任务控制位（worker 在条目边界轮询；见 `crate::scan_worker::Progress::checkpoint`）。
/// 窄窗注记：pause/cancel/resume 的「worker 在场」分支在 worker 置终局 ↔ `running=false`
/// 窄窗内可能瞬时返回乐观值；store 地面真相一致（`set_state_if_active` 护栏），刻意不改
/// （不可确定性测试）。
#[derive(Default)]
pub(crate) struct Ctrl {
    pub(crate) paused: AtomicBool,
    pub(crate) canceled: AtomicBool,
}

struct Active {
    device: Arc<dyn BlockDevice>,
    ctrl: Arc<Ctrl>,
    running: Arc<AtomicBool>,
}

/// `active_of` 返回句柄：控制位 + 运行标志 + 设备（设备留给 M1c 现场复用；此处一并取出）。
type ActiveHandle = (Arc<Ctrl>, Arc<AtomicBool>, Arc<dyn BlockDevice>);

pub struct ScanManager {
    store: Arc<Store>,
    notify: NotifyFn,
    tasks: Mutex<HashMap<u64, Active>>,
}

impl ScanManager {
    pub fn new(store: Store, notify: NotifyFn) -> Self {
        Self {
            store: Arc::new(store),
            notify,
            tasks: Mutex::new(HashMap::new()),
        }
    }

    /// daemon 启动时调用一次：上次退出遗留的 pending/scanning → failed（paused 保留可续）。
    pub fn recover_after_restart(&self) -> Result<usize, StoreError> {
        self.store.mark_interrupted()
    }

    pub fn start(&self, device: Arc<dyn BlockDevice>) -> Result<ScanStarted, ScanError> {
        let fs = probe(&*device).map_err(|_| ScanError::UnsupportedFs)?;
        let total = device.size_bytes();
        let id = self
            .store
            .create_task(&device.info().id, fs.as_str(), "quick", total)?;
        self.spawn(id, device, Box::new(crate::scan_worker::run_worker));
        Ok(ScanStarted {
            task_id: id,
            fs,
            total_bytes: total,
        })
    }

    /// 深扫启动（mode:"deep"）：**先解未分配区间**（失败 → 拒绝，绝不入册空跑），
    /// `total_bytes` = Σ空闲区间长（进度百分比口径）。区间在启动时解一次并随 worker 闭包固定
    /// ——扫描期内不再重解（分配表可能变化，重解会与已落库偏移脱节）；重启续跑**必须**重解
    /// （见 `restart`：闭包随进程消失，runs 无法从库恢复）。
    pub fn start_deep(&self, device: Arc<dyn BlockDevice>) -> Result<ScanStarted, ScanError> {
        let fs = probe(&*device).map_err(|_| ScanError::UnsupportedFs)?;
        let runs = unallocated_runs_of(&*device, fs)?;
        let total: u64 = runs.iter().map(|r| r.end - r.start).sum();
        let id = self
            .store
            .create_task(&device.info().id, fs.as_str(), "deep", total)?;
        self.spawn_carve(id, device, runs, 0, 0);
        Ok(ScanStarted {
            task_id: id,
            fs,
            total_bytes: total,
        })
    }

    /// 入册深扫 worker：`resume_from` = 检查点绝对偏移（首跑 0），`next_idx` = 已落库条目数
    /// （首跑 0；续跑续号，配合 `INSERT OR REPLACE` 让重扫区间覆盖而非追加）。
    fn spawn_carve(
        &self,
        id: u64,
        device: Arc<dyn BlockDevice>,
        runs: Vec<Range<u64>>,
        resume_from: u64,
        next_idx: u64,
    ) {
        self.spawn(
            id,
            device,
            Box::new(move |id, dev, ctrl, store, notify| {
                crate::scan_worker::run_carve_worker(
                    id,
                    dev,
                    runs,
                    resume_from,
                    next_idx,
                    ctrl,
                    store,
                    notify,
                )
            }),
        );
    }

    fn spawn(&self, id: u64, device: Arc<dyn BlockDevice>, work: WorkerFn) {
        let ctrl = Arc::new(Ctrl::default());
        let running = Arc::new(AtomicBool::new(true));
        self.tasks.lock().unwrap().insert(
            id,
            Active {
                device: device.clone(),
                ctrl: ctrl.clone(),
                running: running.clone(),
            },
        );
        let store = self.store.clone();
        let notify = self.notify.clone();
        std::thread::Builder::new()
            .name(format!("scan-{id}"))
            .spawn(move || {
                work(id, device, ctrl, store, notify);
                running.store(false, Ordering::SeqCst);
            })
            .expect("spawn scan worker");
    }

    fn active_of(&self, id: u64) -> Option<ActiveHandle> {
        self.tasks
            .lock()
            .unwrap()
            .get(&id)
            .map(|a| (a.ctrl.clone(), a.running.clone(), a.device.clone()))
    }

    pub fn status(&self, id: u64) -> Result<TaskRow, ScanError> {
        self.store.task(id)?.ok_or(ScanError::TaskNotFound(id))
    }

    /// 与 manager 共享同一 `Arc<Store>`（daemon 侧把 store 交给 `ExportManager` 用；
    /// 子进程经 `--db` 另开只读连接，见 `store::open_read_only`）。
    pub fn store_arc(&self) -> Arc<Store> {
        self.store.clone()
    }

    /// 单条目点查（`fs.read` 用）：缺任务/缺 idx 一律 `None`——调用侧先 `status` 以区分
    /// -32003 与 -32008。
    pub fn entry(&self, id: u64, idx: u64) -> Result<Option<ScanEntry>, StoreError> {
        self.store.entry(id, idx)
    }

    pub fn results(
        &self,
        id: u64,
        offset: u64,
        limit: u64,
        deleted_only: bool,
    ) -> Result<(u64, Vec<ScanEntry>), ScanError> {
        if self.store.task(id)?.is_none() {
            return Err(ScanError::TaskNotFound(id));
        }
        Ok(self.store.entries(id, offset, limit, deleted_only)?)
    }

    /// 暂停：worker 在下一个条目边界驻停。幂等（已 paused → Ok）。
    pub fn pause(&self, id: u64) -> Result<(), ScanError> {
        if let Some((ctrl, running, _)) = self.active_of(id)
            && running.load(Ordering::SeqCst)
        {
            ctrl.paused.store(true, Ordering::SeqCst);
            self.store.set_state_if_active(id, ScanState::Paused)?;
            return Ok(());
        }
        match self.status(id)?.state {
            ScanState::Paused => Ok(()),
            _ => Err(ScanError::TaskNotActive(id)),
        }
    }

    /// 恢复：worker 在场 → 解除驻停；不在场（daemon 重启）→ NeedsDevice 交由 handlers 重开设备。
    /// 窄窗乐观值见 `Ctrl` 注（cancel 置态后、worker unwind 前可能瞬时返回 InPlace/scanning）。
    pub fn resume(&self, id: u64) -> Result<Resume, ScanError> {
        if let Some((ctrl, running, _)) = self.active_of(id)
            && running.load(Ordering::SeqCst)
        {
            ctrl.paused.store(false, Ordering::SeqCst);
            self.store.set_state_if_active(id, ScanState::Scanning)?;
            return Ok(Resume::InPlace);
        }
        let row = self.status(id)?;
        match row.state {
            ScanState::Scanning => Ok(Resume::InPlace), // 幂等
            ScanState::Paused => Ok(Resume::NeedsDevice {
                device_id: row.device_id,
            }),
            _ => Err(ScanError::TaskNotActive(id)),
        }
    }

    /// 重启后按 mode 分派：quick → 清旧结果重跑（成本低）；deep → 从检查点续扫，
    /// **不清 results**（已雕条目保留）、`idx` 自 `found_count` 续号。
    /// 仅 handlers 的 `Resume::NeedsDevice` 路径调用；守卫防公开 API 误用（worker 在场所的
    /// paused 应走 resume，否则同 id 双 worker、旧线程被遗弃）。
    ///
    /// 深扫续跑语义（spec-t7 修复后）：`carved_offset`（续扫点）与 `found_count`（续号起点）
    /// 由 worker **同帧配对落盘**（见 `Store::set_carved_offset`）——断点所在窗口内的条目重扫时
    /// 以同编号 `INSERT OR REPLACE` 覆盖为等价记录（幂等）。**已落库条目零丢失、无重复**；
    /// 两次检查点写之间被杀 → 库内仍是上一自洽帧（多回退一个窗口重扫，同样幂等）。
    pub fn restart(&self, id: u64, device: Arc<dyn BlockDevice>) -> Result<(), ScanError> {
        if let Some((_, running, _)) = self.active_of(id)
            && running.load(Ordering::SeqCst)
        {
            return Err(ScanError::TaskNotActive(id));
        }
        let row = self.status(id)?;
        if row.state != ScanState::Paused {
            return Err(ScanError::TaskNotActive(id));
        }
        let fs = probe(&*device).map_err(|_| ScanError::UnsupportedFs)?;
        if row.scan_mode == "deep" {
            // runs 随进程消失、无法从库恢复：重解一次（失败 → 拒绝，与首跑同契约）
            let runs = unallocated_runs_of(&*device, fs)?;
            self.store.set_state(id, ScanState::Scanning)?;
            self.spawn_carve(
                id,
                device,
                runs,
                row.carved_offset.unwrap_or(0),
                row.found_count,
            );
        } else {
            self.store.clear_entries(id)?;
            self.store.set_state(id, ScanState::Scanning)?;
            self.spawn(id, device, Box::new(crate::scan_worker::run_worker));
        }
        Ok(())
    }

    /// 取消：worker 在场 → 置标记（条目边界 unwind）；已 canceled → 幂等；终态 → TaskNotActive。
    pub fn cancel(&self, id: u64) -> Result<(), ScanError> {
        if let Some((ctrl, running, _)) = self.active_of(id)
            && running.load(Ordering::SeqCst)
        {
            ctrl.canceled.store(true, Ordering::SeqCst);
            ctrl.paused.store(false, Ordering::SeqCst); // 驻停中也要能取消
            self.store.set_state_if_active(id, ScanState::Canceled)?;
            return Ok(());
        }
        match self.status(id)?.state {
            ScanState::Canceled => Ok(()),
            _ => Err(ScanError::TaskNotActive(id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    use xd_device::{DeviceError, DeviceInfo};

    use crate::testutil::{exfat_fixture, wait_for_state};

    fn mgr() -> ScanManager {
        ScanManager::new(Store::open_memory().unwrap(), Arc::new(|_| {}))
    }

    #[test]
    fn start_scans_and_streams_to_store() {
        let m = mgr();
        let (_f, dev) = exfat_fixture();
        let s = m.start(dev).unwrap();
        assert_eq!(s.fs, FsKind::Exfat);
        let row = wait_for_state(&m, s.task_id, ScanState::Completed, Duration::from_secs(10));
        assert_eq!(row.found_count, 3);
        let (total, page) = m.results(s.task_id, 0, 10, false).unwrap();
        assert_eq!(total, 3);
        let del = page.iter().find(|e| e.deleted).unwrap();
        assert_eq!(
            del.name, "DEL_ME.JPG",
            "exFAT 删除名一字不差（流式落盘不丢）"
        );
        assert_eq!(del.quality, "complete");
        let (dtotal, dpage) = m.results(s.task_id, 0, 10, true).unwrap();
        assert_eq!((dtotal, dpage.len()), (1, 1));
    }

    #[test]
    fn fat_path_smoke_run_worker_and_fat_to_entry_mapping() {
        // FsKind::Fat 生产路径的唯一覆盖：probe 判型 → run_worker Fat 臂 → fat_to_entry 映射 → store 回读。
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "FAT_A.TXT", b"hello")
            .build();
        let (_f, dev) = crate::testutil::dev_from_bytes(&image);
        assert_eq!(probe(&*dev).unwrap(), FsKind::Fat);
        let m = mgr();
        let s = m.start(dev).unwrap();
        assert_eq!(s.fs, FsKind::Fat);
        let row = wait_for_state(&m, s.task_id, ScanState::Completed, Duration::from_secs(10));
        assert_eq!(row.found_count, 1);
        let (total, page) = m.results(s.task_id, 0, 10, false).unwrap();
        assert_eq!((total, page.len()), (1, 1));
        let e = &page[0];
        assert_eq!(e.name, "FAT_A.TXT");
        assert_eq!(e.path, "/");
        assert_eq!(e.ext, "txt", "fat_to_entry: ext 小写");
        assert_eq!(e.size_bytes, 5, "size_bytes 透传夹具内容长度");
        assert_eq!(
            e.first_cluster, 2,
            "夹具首簇（FAT16 固定根目录区之外的最低空闲簇）"
        );
        assert!(!e.deleted);
        assert!(!e.is_dir);
        assert_eq!(e.quality, "complete");
        assert_eq!(e.idx, 0);
    }

    /// exfat 夹具 + 在最大空闲区间的绝对偏移处埋 mini_jpeg（深扫可见件），
    /// 返回 (镜像, 埋件, 区间起点)。
    fn deep_fixture() -> (Vec<u8>, Vec<u8>, u64) {
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "LIVE_A.TXT", b"aaaa")
            .add_file("/", "DEL_ME.JPG", &[7u8; 9000])
            .delete("/", "DEL_ME.JPG")
            .build();
        let (_f0, dev0) = crate::testutil::dev_from_bytes(&image);
        let runs = xd_fs_exfat::freespace::unallocated_runs(&*dev0).unwrap();
        let run = runs
            .iter()
            .max_by_key(|r| r.end - r.start)
            .expect("夹具必有空闲区间")
            .clone();
        let j = xd_fixtures::mini_jpeg(20000);
        xd_fixtures::plant_in_run(&mut image, run.start, &j);
        (image, j, run.start)
    }

    #[test]
    fn deep_scan_completes_and_lands_carved_entries() {
        let (image, j, off) = deep_fixture();
        let (_f, dev) = crate::testutil::dev_from_bytes(&image);
        let m = mgr();
        let s = m.start_deep(dev).unwrap();
        assert_eq!(s.fs, FsKind::Exfat);
        let row = wait_for_state(&m, s.task_id, ScanState::Completed, Duration::from_secs(20));
        assert_eq!(row.found_count, 1);
        assert_eq!(
            row.read_bytes, s.total_bytes,
            "进度收尾 100%（口径=Σ空闲区间）"
        );
        assert_eq!(
            m.status(s.task_id).unwrap().scan_mode,
            "deep",
            "入册即记 mode"
        );
        let (total, page) = m.results(s.task_id, 0, 10, false).unwrap();
        assert_eq!((total, page.len()), (1, 1));
        assert_eq!(page[0].quality, "carved");
        assert_eq!(page[0].byte_offset, Some(off));
        assert_eq!(page[0].size_bytes, j.len() as u64);
        assert_eq!(page[0].ext, "jpg");
    }

    #[test]
    fn deep_cancel_stops_via_callback_not_unwind() {
        // 深扫取消 = 回调返回 false（雕刻循环自家代码）→ 终态 canceled，**不得**落 failed
        //（failed 说明走了 panic 通道，即机制错位）。
        let (image, _, _) = deep_fixture();
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let m = ScanManager::new(
            Store::open_memory().unwrap(),
            Arc::new(move |v| ev.lock().unwrap().push(v)),
        );
        let (_f, dev) = crate::testutil::dev_from_bytes(&image);
        let slow = crate::testutil::SlowDev::wrap(dev, Duration::from_millis(30));
        let s = m.start_deep(slow).unwrap();
        m.cancel(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Canceled, Duration::from_secs(10));
        let deadline = Instant::now() + Duration::from_secs(10);
        let done = loop {
            if let Some(v) = events
                .lock()
                .unwrap()
                .iter()
                .find(|v| v["method"] == "scan.finished")
            {
                break v.clone();
            }
            assert!(
                Instant::now() < deadline,
                "carve worker 未发出 scan.finished"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(done["params"]["state"], "canceled", "{done}");
    }

    #[test]
    fn pause_freezes_reads_and_resume_completes() {
        let m = mgr();
        let (_f, dev) = exfat_fixture();
        let slow = crate::testutil::SlowDev::wrap(dev, Duration::from_millis(30));
        let s = m.start(slow).unwrap();
        m.pause(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Paused, Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(250)); // 驻停稳定窗口
        let a = m.status(s.task_id).unwrap().read_bytes;
        std::thread::sleep(Duration::from_millis(200));
        let b = m.status(s.task_id).unwrap().read_bytes;
        assert_eq!(a, b, "驻停后读量冻结（worker 在条目边界不再前进）");
        m.resume(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Completed, Duration::from_secs(20));
        let (total, _) = m.results(s.task_id, 0, 10, false).unwrap();
        assert_eq!(total, 3);
    }

    #[test]
    fn pause_before_first_entry_still_freezes_and_completes() {
        // 与上测试同构但覆盖「暂停先于任何条目」：暂停后 found 可为 0，resume 后必须 3 条全到
        let m = mgr();
        let (_f, dev) = exfat_fixture();
        let slow = crate::testutil::SlowDev::wrap(dev, Duration::from_millis(20));
        let s = m.start(slow).unwrap();
        m.pause(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Paused, Duration::from_secs(5));
        m.resume(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Completed, Duration::from_secs(20));
        assert_eq!(m.results(s.task_id, 0, 10, false).unwrap().0, 3);
    }

    #[test]
    fn cancel_marks_canceled_keeps_partial_and_notifies() {
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let m = ScanManager::new(
            Store::open_memory().unwrap(),
            Arc::new(move |v| ev.lock().unwrap().push(v)),
        );
        let (_f, dev) = exfat_fixture();
        let slow = crate::testutil::SlowDev::wrap(dev, Duration::from_millis(30));
        let s = m.start(slow).unwrap();
        m.cancel(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Canceled, Duration::from_secs(10));
        // 置态是同步的、通知是异步的（worker 需先到条目边界 unwind）——必须轮询事件本身，
        // 不能只等状态（否则 CI 负载下会读到尚未送达的 scan.finished）。
        let deadline = Instant::now() + Duration::from_secs(10);
        let done = loop {
            if let Some(v) = events
                .lock()
                .unwrap()
                .iter()
                .find(|v| v["method"] == "scan.finished")
            {
                break v.clone();
            }
            assert!(Instant::now() < deadline, "worker 未发出 scan.finished");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(done["params"]["state"], "canceled");
        assert!(
            m.results(s.task_id, 0, 10, false).is_ok(),
            "部分结果保留可查"
        );
    }

    #[test]
    fn worker_panic_is_isolated_as_failed() {
        struct PanicDev(Arc<dyn BlockDevice>);
        impl BlockDevice for PanicDev {
            fn info(&self) -> &DeviceInfo {
                self.0.info()
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
                if offset > 0 {
                    panic!("boom");
                }
                self.0.read_at(offset, buf)
            }
        }
        let m = mgr();
        let (_f, dev) = exfat_fixture();
        let s = m.start(Arc::new(PanicDev(dev))).unwrap();
        let row = wait_for_state(&m, s.task_id, ScanState::Failed, Duration::from_secs(10));
        assert_eq!(row.found_count, 0);
        // 管理器仍可用（隔离：一个 worker 崩，不影响后续任务）
        let (_f2, dev2) = exfat_fixture();
        let s2 = m.start(dev2).unwrap();
        wait_for_state(
            &m,
            s2.task_id,
            ScanState::Completed,
            Duration::from_secs(10),
        );
    }

    #[test]
    fn unsupported_fs_and_unknown_task_errors() {
        let m = mgr();
        let (_f, dev) = crate::testutil::dev_from_bytes(&[0u8; 4096]);
        assert!(matches!(m.start(dev), Err(ScanError::UnsupportedFs)));
        assert!(matches!(m.status(999), Err(ScanError::TaskNotFound(999))));
        assert!(matches!(m.pause(999), Err(ScanError::TaskNotFound(999))));
        assert!(matches!(m.cancel(999), Err(ScanError::TaskNotFound(999))));
        assert!(matches!(m.resume(999), Err(ScanError::TaskNotFound(999))));
    }

    #[test]
    fn resume_after_restart_needs_device_then_reruns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let id;
        {
            let m = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
            let (_f, dev) = exfat_fixture();
            let slow = crate::testutil::SlowDev::wrap(dev, Duration::from_millis(30));
            let s = m.start(slow).unwrap();
            id = s.task_id;
            m.pause(id).unwrap();
            wait_for_state(&m, id, ScanState::Paused, Duration::from_secs(5));
        } // 模拟 daemon 退出（worker 随进程死；此处 m drop，worker 线程仍在跑——它已驻停）
        let m2 = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
        m2.recover_after_restart().unwrap();
        assert_eq!(
            m2.status(id).unwrap().state,
            ScanState::Paused,
            "paused 跨重启保留"
        );
        match m2.resume(id).unwrap() {
            Resume::NeedsDevice { device_id } => assert!(device_id.starts_with("image:")),
            Resume::InPlace => panic!("worker 已不在场，必须 NeedsDevice"),
        }
        let (_f2, dev2) = exfat_fixture();
        m2.restart(id, dev2).unwrap();
        wait_for_state(&m2, id, ScanState::Completed, Duration::from_secs(10));
        assert_eq!(
            m2.results(id, 0, 10, false).unwrap().0,
            3,
            "清后重跑结果完整"
        );
    }

    /// 续跑夹具：**两个空闲 run**（删除的 BIG.TMP 簇段 + 卷尾 LIVE 之后的簇段），各埋一枚
    /// mini_png。两 run 均 >128KiB → `SlowDev::wrap_big_reads` 下窗口读是唯一慢点。
    struct DeepResume {
        image: Vec<u8>,
        a: (u64, Vec<u8>),
        b: (u64, Vec<u8>),
    }

    fn deep_resume_fixture() -> DeepResume {
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "BIG.TMP", &[0u8; 300 * 1024])
            .add_file("/", "LIVE.TXT", b"live")
            .delete("/", "BIG.TMP")
            .build();
        let (_f0, dev0) = crate::testutil::dev_from_bytes(&image);
        let runs = xd_fs_exfat::freespace::unallocated_runs(&*dev0).unwrap();
        assert!(runs.len() >= 2, "夹具须有两个空闲区间: {runs:?}");
        let a = xd_fixtures::mini_png(b"first-segment");
        let b = xd_fixtures::mini_png(b"second-segment");
        let a_off = runs[0].start + 4096;
        let b_off = runs[1].start + 4096;
        xd_fixtures::plant_in_run(&mut image, a_off, &a);
        xd_fixtures::plant_in_run(&mut image, b_off, &b);
        DeepResume {
            image,
            a: (a_off, a),
            b: (b_off, b),
        }
    }

    /// 深扫 carved 条目的形状（与 scan_worker 的映射同构）。
    fn carved_entry(idx: u64, byte_offset: u64, size: u64) -> ScanEntry {
        ScanEntry {
            idx,
            name: String::new(),
            path: String::new(),
            ext: "png".into(),
            size_bytes: size,
            deleted: true,
            is_dir: false,
            quality: "carved".into(),
            first_cluster: 0,
            byte_offset: Some(byte_offset),
            contiguous: None,
        }
    }

    #[test]
    fn deep_pause_restart_resumes_from_checkpoint() {
        // 真流程：慢速深扫 → 暂停（检查点已落盘）→ 丢 manager（模拟进程被杀）→ 同库新 manager
        // → recover_after_restart → resume → NeedsDevice → restart → 续跑至 completed。
        let fx = deep_resume_fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let id;
        {
            let m = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
            let (_f, dev) = crate::testutil::dev_from_bytes(&fx.image);
            let slow = crate::testutil::SlowDev::wrap_big_reads(dev, Duration::from_millis(120));
            let s = m.start_deep(slow).unwrap();
            id = s.task_id;
            m.pause(id).unwrap();
            wait_for_state(&m, id, ScanState::Paused, Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(300)); // 驻停稳定窗口
            let row = m.status(id).unwrap();
            assert!(
                row.carved_offset.is_some(),
                "深扫暂停时检查点必须已落盘（None → 重启从 0 整段重扫）"
            );
        } // 模拟 daemon 退出：worker 随进程死（此处已驻停）
        let m2 = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
        m2.recover_after_restart().unwrap();
        assert_eq!(m2.status(id).unwrap().state, ScanState::Paused);
        match m2.resume(id).unwrap() {
            Resume::NeedsDevice { .. } => {}
            Resume::InPlace => panic!("worker 已不在场，必须 NeedsDevice"),
        }
        let (_f2, dev2) = crate::testutil::dev_from_bytes(&fx.image);
        m2.restart(id, dev2).unwrap();
        wait_for_state(&m2, id, ScanState::Completed, Duration::from_secs(20));
        let (total, page) = m2.results(id, 0, 10, false).unwrap();
        for (off, png) in [&fx.a, &fx.b] {
            let hits: Vec<_> = page
                .iter()
                .filter(|e| e.byte_offset == Some(*off))
                .collect();
            assert_eq!(hits.len(), 1, "偏移 {off} 恰一条（无丢条）: {page:?}");
            assert_eq!(hits[0].size_bytes, png.len() as u64);
            assert_eq!(hits[0].quality, "carved");
        }
        assert_eq!(
            total, 2,
            "本场景暂停在窗口界：配对检查点下恒 2 条、零残余: {page:?}"
        );
        assert_eq!(
            m2.status(id).unwrap().read_bytes,
            m2.status(id).unwrap().total_bytes,
            "续跑收尾 100%（进度含断点前缀）"
        );
    }

    /// spec-t7 阻断形状夹具：run0 埋 8 枚、run1 埋 3 枚 mini_png（4KiB 间距，同 [`deep_resume_fixture`]
    /// 的 1MiB exFAT 卷）。暂停落在 **run1** 时 run0 全数已落库、而 250ms 节流进度从未写过
    /// `found_count`——单写检查点的旧实现以滞后计数续号，重扫 run1 的同号 REPLACE 覆盖 run0 旧行。
    struct DeepTwoRun {
        image: Vec<u8>,
        offsets: Vec<u64>,
        k: usize,
        run1_start: u64,
    }

    fn deep_two_run_fixture() -> DeepTwoRun {
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "BIG.TMP", &[0u8; 300 * 1024])
            .add_file("/", "LIVE.TXT", b"live")
            .delete("/", "BIG.TMP")
            .build();
        let (_f0, dev0) = crate::testutil::dev_from_bytes(&image);
        let runs = xd_fs_exfat::freespace::unallocated_runs(&*dev0).unwrap();
        assert!(runs.len() >= 2, "夹具须有两个空闲区间: {runs:?}");
        assert!(
            runs[1].start.saturating_sub(runs[0].end) >= 7,
            "两区间间隙须 ≥7 字节（续跑点收 7 字节不得落进前区间）: {runs:?}"
        );
        let png = xd_fixtures::mini_png(b"loss-probe");
        let mut offsets = Vec::new();
        for (run, n) in [(runs[0].clone(), 8u64), (runs[1].clone(), 3u64)] {
            for i in 0..n {
                let off = run.start + 4096 + i * 4096;
                assert!(
                    off + png.len() as u64 <= run.end,
                    "埋点 {off} 越区间 {}..{}",
                    run.start,
                    run.end
                );
                xd_fixtures::plant_in_run(&mut image, off, &png);
                offsets.push(off);
            }
        }
        DeepTwoRun {
            image,
            offsets,
            k: 8,
            run1_start: runs[1].start,
        }
    }

    #[test]
    fn deep_pause_in_second_run_restart_loses_nothing() {
        // spec-t7 阻断的真回归（旧形状失明的根因：暂停落在窗口界/首 run，配对与否无差）：
        // 暂停驻留在**第二个 run**，重启续跑后条目总数与偏移集合必须与全量逐点相等。
        let fx = deep_two_run_fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let id;
        {
            let m = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
            let (_f, dev) = crate::testutil::dev_from_bytes(&fx.image);
            // 每条目 ≈ 一次 64KiB 预读重填（`wrap` 对一切读睡 10ms）：条目边界可观测、可驻停，
            // 且 run1 检查点时刻 ≪ 250ms → 节流进度此时恒未写过（阻断形状成立的前提）。
            let slow = crate::testutil::SlowDev::wrap(dev, Duration::from_millis(10));
            let s = m.start_deep(slow).unwrap();
            id = s.task_id;
            crate::testutil::wait_for_entries(&m, id, fx.k as u64, Duration::from_secs(30));
            m.pause(id).unwrap();
            wait_for_state(&m, id, ScanState::Paused, Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(50)); // 驻停稳定窗口
            let row = m.status(id).unwrap();
            assert_eq!(
                row.carved_offset,
                Some(fx.run1_start),
                "暂停须驻留在 run1（检查点已越到 run1 起点）——停在窗口界时本形状对配对写失明: {row:?}"
            );
            assert_eq!(
                row.found_count, fx.k as u64,
                "配对写：found_count 与 carved_offset 同帧 = run0 条目数；单写旧实现此处 0（续号覆盖 run0 旧行 = 丢条）"
            );
        } // 模拟进程被杀：worker 已驻停、manager 丢弃，库态留存
        let m2 = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
        m2.recover_after_restart().unwrap();
        match m2.resume(id).unwrap() {
            Resume::NeedsDevice { .. } => {}
            Resume::InPlace => panic!("worker 已不在场，必须 NeedsDevice"),
        }
        let (_f2, dev2) = crate::testutil::dev_from_bytes(&fx.image);
        m2.restart(id, dev2).unwrap();
        wait_for_state(&m2, id, ScanState::Completed, Duration::from_secs(30));
        let (total, page) = m2.results(id, 0, 100, false).unwrap();
        assert_eq!(
            total,
            fx.offsets.len() as u64,
            "续跑后总数必须等于全量（旧实现续号覆盖 run0 旧行 → 此处少）: {page:?}"
        );
        let mut got: Vec<u64> = page.iter().map(|e| e.byte_offset.unwrap()).collect();
        got.sort_unstable();
        let mut want = fx.offsets.clone();
        want.sort_unstable();
        assert_eq!(got, want, "偏移集合逐点相等（丢条/重报都现形）");
        let mut idxs: Vec<u64> = page.iter().map(|e| e.idx).collect();
        idxs.sort_unstable();
        assert_eq!(
            idxs,
            (0..total).collect::<Vec<u64>>(),
            "idx 恰 0..total（同号覆盖会留重号空洞）"
        );
    }

    #[test]
    fn deep_restart_keeps_landed_entries_and_resumes_from_checkpoint() {
        // 手工布置「首段已扫、进程被杀」的库态（条目 + 检查点 + paused）→ restart 必须
        // **不清 results**、idx 自 found_count 续号、只扫断点之后（首段条目原地保留、不重扫）。
        let fx = deep_resume_fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let (_f0, dev0) = crate::testutil::dev_from_bytes(&fx.image);
        let runs = xd_fs_exfat::freespace::unallocated_runs(&*dev0).unwrap();
        let total: u64 = runs.iter().map(|r| r.end - r.start).sum();
        let breakpoint = runs[1].start; // 断点 = 第二个 run 起点：首段（run 1）整段在断点前
        let id;
        {
            let s = Store::open(&path).unwrap();
            id = s
                .create_task(&dev0.info().id, "exfat", "deep", total)
                .unwrap();
            s.insert_entries(id, &[carved_entry(0, fx.a.0, fx.a.1.len() as u64)])
                .unwrap();
            s.set_progress(id, 0, 1, 0).unwrap();
            s.set_carved_offset(id, breakpoint, 1).unwrap();
            s.set_state(id, ScanState::Paused).unwrap();
        }
        let m = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
        m.recover_after_restart().unwrap();
        let (_f2, dev) = crate::testutil::dev_from_bytes(&fx.image);
        m.restart(id, dev).unwrap();
        wait_for_state(&m, id, ScanState::Completed, Duration::from_secs(20));
        let (n, page) = m.results(id, 0, 10, false).unwrap();
        assert_eq!(
            n, 2,
            "首段条目保留 + 续段新增（清表会把首段丢掉）: {page:?}"
        );
        assert_eq!(
            (page[0].idx, page[0].byte_offset),
            (0, Some(fx.a.0)),
            "首段条目原地保留（断点之前不重扫）"
        );
        assert_eq!(
            (page[1].idx, page[1].byte_offset),
            (1, Some(fx.b.0)),
            "idx 自 found_count 续号（从 0 重号会覆盖首段条目）"
        );
        assert_eq!(
            m.status(id).unwrap().read_bytes,
            total,
            "续跑进度 = 断点前缀 + 本次扫描（基准漏加则收尾 < 100%）"
        );
    }

    #[test]
    fn carved_offset_persisted_during_scan() {
        // 深扫进行中（首段条目已落库后暂停）：检查点已写且 ≥ 全部已落库条目的 byte_offset
        // ——「已落库条目恒在 carved_offset 之前」是续跑不重扫已落库内容的前提。
        let fx = deep_resume_fixture();
        let m = mgr();
        let (_f, dev) = crate::testutil::dev_from_bytes(&fx.image);
        let slow = crate::testutil::SlowDev::wrap_big_reads(dev, Duration::from_millis(120));
        let s = m.start_deep(slow).unwrap();
        crate::testutil::wait_for_entries(&m, s.task_id, 1, Duration::from_secs(10));
        m.pause(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Paused, Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(250)); // 驻停稳定窗口
        let row = m.status(s.task_id).unwrap();
        let off = row
            .carved_offset
            .expect("深扫进行中检查点必须已写（None → 重启从 0 整段重扫）");
        assert!(off > 0, "检查点须在首个空闲区间起点之后");
        let (total, page) = m.results(s.task_id, 0, 10, false).unwrap();
        assert!(total > 0, "前提：首段条目已落库");
        for e in &page {
            assert!(
                e.byte_offset.unwrap() <= off,
                "已落库条目必须 ≤ 检查点: {e:?} vs {off}"
            );
        }
    }

    #[test]
    fn quick_restart_still_clears_stale_entries() {
        // quick 语义不回归：重启清旧结果重跑（深扫的「保留」不得泄漏到 quick 分支）
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let id;
        {
            let m = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
            let (_f, dev) = exfat_fixture();
            let slow = crate::testutil::SlowDev::wrap(dev, Duration::from_millis(30));
            let s = m.start(slow).unwrap();
            id = s.task_id;
            m.pause(id).unwrap();
            wait_for_state(&m, id, ScanState::Paused, Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(250));
        }
        {
            // 注入陈旧条目（idx 99）：重启后必须消失
            let s = Store::open(&path).unwrap();
            s.insert_entries(id, &[carved_entry(99, 4242, 7)]).unwrap();
        }
        let m2 = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
        m2.recover_after_restart().unwrap();
        let (_f2, dev2) = exfat_fixture();
        m2.restart(id, dev2).unwrap();
        wait_for_state(&m2, id, ScanState::Completed, Duration::from_secs(10));
        let (total, page) = m2.results(id, 0, 10, false).unwrap();
        assert_eq!(total, 3, "陈旧条目必须被清（quick 重跑语义）: {page:?}");
    }

    #[test]
    fn restart_rejects_in_place_worker() {
        // 防呆：worker 在场且 running 的 paused（该走 resume）必须拒绝，否则同 id 双 worker。
        let m = mgr();
        let (_f, dev) = exfat_fixture();
        let slow = crate::testutil::SlowDev::wrap(dev, Duration::from_millis(30));
        let s = m.start(slow).unwrap();
        m.pause(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Paused, Duration::from_secs(5));
        let (_f2, dev2) = exfat_fixture();
        assert!(matches!(
            m.restart(s.task_id, dev2),
            Err(ScanError::TaskNotActive(_))
        ));
        // 拒绝必须无副作用：原 worker 仍可 resume 走完
        m.resume(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Completed, Duration::from_secs(20));
        assert_eq!(m.results(s.task_id, 0, 10, false).unwrap().0, 3);
    }

    #[test]
    fn recover_after_restart_fails_interrupted_keeps_paused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let (scanning_id, paused_id) = {
            let s = Store::open(&path).unwrap();
            let a = s.create_task("image:a.img", "exfat", "quick", 1).unwrap();
            let b = s.create_task("image:b.img", "exfat", "quick", 1).unwrap();
            s.set_state(b, ScanState::Paused).unwrap();
            (a, b)
        };
        let m = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
        assert_eq!(m.recover_after_restart().unwrap(), 1);
        assert_eq!(m.status(scanning_id).unwrap().state, ScanState::Failed);
        assert_eq!(m.status(paused_id).unwrap().state, ScanState::Paused);
    }

    #[test]
    fn probe_detects_exfat_and_rejects_garbage() {
        let (_f, dev) = exfat_fixture();
        assert_eq!(probe(&*dev).unwrap(), FsKind::Exfat);
        let (_f2, zeros) = crate::testutil::dev_from_bytes(&[0u8; 4096]);
        assert!(matches!(probe(&*zeros), Err(ProbeError::Unsupported)));
    }
}
