// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 扫描任务与结果的 SQLite 持久化（设计 §4.4.4：流式落盘、分页查询、崩溃/重启后可查）。
//! 单进程单连接（`Mutex<Connection>`）：daemon 是唯一写者。
//!
//! **写放大与崩溃语义（T7 阶段三量化选型 b）：** `journal_mode=WAL` + `synchronous=NORMAL`。
//! 旧配置（DELETE 日志 + FULL）每条目一次 fsync ≈ 7.2ms → ext4 实测 513 条目 **139 条/秒**
//! （3.68s，与 qual-m1b-t6 的 6.9ms 同源）；WAL+NORMAL 同调用形态 **8288-10265 条/秒**
//! （513 条目 ≈ 46-62ms，≈60-74×），优于 worker 侧小批量（64 条/事务）形态的 7075 条/秒。
//! 崩溃语义声明（限定到**已提交事务**层面）：**daemon 进程崩溃零丢失已提交事务**（在 WAL 里，
//! 重开即恢复）；**掉电/OS 崩溃可能丢最后一次 checkpoint 之后的提交**（库恒一致；扫描结果可由
//! 重扫再得——源设备只读，结果非独有数据）。
//! 内存库不受影响（pragma 落 "memory" 模式）。

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OpenFlags, params};

use crate::api::{ScanEntry, ScanState};

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "sqlite error: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub(crate) fn state_str(s: ScanState) -> &'static str {
    match s {
        ScanState::Pending => "pending",
        ScanState::Scanning => "scanning",
        ScanState::Paused => "paused",
        ScanState::Canceled => "canceled",
        ScanState::Completed => "completed",
        ScanState::Failed => "failed",
    }
}

pub(crate) fn state_from_str(s: &str) -> Option<ScanState> {
    match s {
        "pending" => Some(ScanState::Pending),
        "scanning" => Some(ScanState::Scanning),
        "paused" => Some(ScanState::Paused),
        "canceled" => Some(ScanState::Canceled),
        "completed" => Some(ScanState::Completed),
        "failed" => Some(ScanState::Failed),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: u64,
    pub device_id: String,
    pub fs: String,
    pub state: ScanState,
    pub read_bytes: u64,
    pub found_count: u64,
    pub elapsed_ms: u64,
    pub total_bytes: u64,
    /// "quick" | "deep"（T7 断点续跑按此分派 worker）。
    pub scan_mode: String,
    /// 深扫检查点（v4）：已完整处理内容的绝对右界（安全续扫点，见 `xd_carving` 头注）。
    /// `None` = 无检查点（从未写过 / 迁移前旧行）——0 是合法偏移（区间起点），不得与未知混同。
    pub carved_offset: Option<u64>,
}

/// 连接锁中毒即 fail-stop（`unwrap`）——重启后的任务态由 `mark_interrupted`/`recover_after_restart` 兜底。
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let s = Self {
            conn: Mutex::new(Connection::open(path)?),
        };
        s.init()?;
        Ok(s)
    }

    pub fn open_memory() -> Result<Self, StoreError> {
        let s = Self {
            conn: Mutex::new(Connection::open_in_memory()?),
        };
        s.init()?;
        Ok(s)
    }

    /// 只读打开（导出子进程用）：**不跑 init()/迁移**——schema 由 daemon 的常规打开负责；
    /// 子进程在降权前调用（fd/shm 均以父权限建立，降权后连接只读亦可用）。
    pub fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            conn: Mutex::new(Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?),
        })
    }

    fn init(&self) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        // 写放大（T7 阶段三量化选型 b，理由与崩溃语义见模块头注）：条目插入走 WAL 提交
        //（无逐条 fsync）——ext4 实测 139 → 8288-10265 条/秒。内存库上此 pragma 落 "memory" 模式，
        // 语义不变。
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 device_id TEXT NOT NULL,
                 fs TEXT NOT NULL,
                 state TEXT NOT NULL,
                 read_bytes INTEGER NOT NULL DEFAULT 0,
                 found_count INTEGER NOT NULL DEFAULT 0,
                 elapsed_ms INTEGER NOT NULL DEFAULT 0,
                 total_bytes INTEGER NOT NULL,
                 scan_mode TEXT NOT NULL DEFAULT 'quick'
             );
             CREATE TABLE IF NOT EXISTS entries (
                 task_id INTEGER NOT NULL,
                 idx INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 path TEXT NOT NULL,
                 ext TEXT NOT NULL,
                 size_bytes INTEGER NOT NULL,
                 deleted INTEGER NOT NULL,
                 is_dir INTEGER NOT NULL,
                 quality TEXT NOT NULL,
                 first_cluster INTEGER NOT NULL,
                 -- byte_offset 三态：NULL = 未知（迁移前旧行 / FS 条目）；0 是合法雕刻偏移
                 --（文件恰在未分配区间起点）——不得用 0 表示未知。
                 byte_offset INTEGER,
                 -- contiguous 同三态：NULL = 未知（迁移前旧行 / fat / 雕刻）；1/0 = exfat 显式拓扑。
                 contiguous INTEGER,
                 -- record_id 同三态（v1.3）：NULL = 未知（迁移前旧行 / fat / exfat / 雕刻件）；
                 -- 0 是合法记录号（NTFS = MFT 记录号、ext4 = inode 号）——不得用 0 表未知。
                 record_id INTEGER,
                 PRIMARY KEY (task_id, idx)
             );",
        )?;
        // v1 → v2 迁移（M1c）：entries.byte_offset。user_version 闸门 + 列探测保证幂等
        //（全新库建表即含列，仅置版本；v1 旧库列探测后 ALTER）。
        let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if ver < 2 {
            let has: bool = conn
                .prepare("SELECT 1 FROM pragma_table_info('entries') WHERE name = 'byte_offset'")?
                .exists([])?;
            if !has {
                conn.execute("ALTER TABLE entries ADD COLUMN byte_offset INTEGER", [])?;
            }
            conn.execute_batch("PRAGMA user_version = 2")?;
        }
        // v2 → v3 迁移（M1c T6）：tasks.scan_mode（quick/deep，断点续跑按此分派）。
        // 模版同 v2：闸门 + 列探测；旧行由 DEFAULT 'quick' 得保守值（M1c 前只有 quick）。
        if ver < 3 {
            let has: bool = conn
                .prepare("SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'scan_mode'")?
                .exists([])?;
            if !has {
                conn.execute(
                    "ALTER TABLE tasks ADD COLUMN scan_mode TEXT NOT NULL DEFAULT 'quick'",
                    [],
                )?;
            }
            conn.execute_batch("PRAGMA user_version = 3")?;
        }
        // v3 → v4 迁移（M1c T7）：tasks.carved_offset（深扫断点续跑的检查点）。
        // 模版同前；**可空且不给 DEFAULT**——旧行/未写过 = NULL（未知），0 是合法偏移。
        if ver < 4 {
            let has: bool = conn
                .prepare("SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'carved_offset'")?
                .exists([])?;
            if !has {
                conn.execute("ALTER TABLE tasks ADD COLUMN carved_offset INTEGER", [])?;
            }
            conn.execute_batch("PRAGMA user_version = 4")?;
        }
        // v4 → v5 迁移（M1d T2）：entries.contiguous（exfat 拓扑提示，M1d read.EntryRange 反构造
        // 承重）。模版同 v2；**可空且不给 DEFAULT**——旧行 NULL 反构造时按 false（只信链）。
        if ver < 5 {
            let has: bool = conn
                .prepare("SELECT 1 FROM pragma_table_info('entries') WHERE name = 'contiguous'")?
                .exists([])?;
            if !has {
                conn.execute("ALTER TABLE entries ADD COLUMN contiguous INTEGER", [])?;
            }
            conn.execute_batch("PRAGMA user_version = 5")?;
        }
        // v5 → v6 迁移（M2 T1）：entries.record_id（NTFS = MFT 记录号 / ext4 = inode 号，
        // 读取路径反构造承重）。模版同 v2；**可空且不给 DEFAULT**——旧行 NULL 反构造时按
        // 缺失处理（-32603，绝不按 first_cluster 猜读）；0 是合法记录号。
        if ver < 6 {
            let has: bool = conn
                .prepare("SELECT 1 FROM pragma_table_info('entries') WHERE name = 'record_id'")?
                .exists([])?;
            if !has {
                conn.execute("ALTER TABLE entries ADD COLUMN record_id INTEGER", [])?;
            }
            conn.execute_batch("PRAGMA user_version = 6")?;
        }
        Ok(())
    }

    pub fn create_task(
        &self,
        device_id: &str,
        fs: &str,
        mode: &str,
        total_bytes: u64,
    ) -> Result<u64, StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tasks (device_id, fs, state, total_bytes, scan_mode)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                device_id,
                fs,
                state_str(ScanState::Scanning),
                total_bytes as i64,
                mode
            ],
        )?;
        Ok(conn.last_insert_rowid() as u64)
    }

    pub fn set_state(&self, id: u64, state: ScanState) -> Result<(), StoreError> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET state = ?2 WHERE id = ?1",
            params![id as i64, state_str(state)],
        )?;
        Ok(())
    }

    /// 条件置态（只从活动态迁移）：pause/cancel 的竞态护栏——worker 已终态时不得被覆写。
    pub fn set_state_if_active(&self, id: u64, state: ScanState) -> Result<(), StoreError> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET state = ?2 WHERE id = ?1 AND state IN ('pending','scanning','paused')",
            params![id as i64, state_str(state)],
        )?;
        Ok(())
    }

    pub fn set_progress(
        &self,
        id: u64,
        read_bytes: u64,
        found_count: u64,
        elapsed_ms: u64,
    ) -> Result<(), StoreError> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET read_bytes = ?2, found_count = ?3, elapsed_ms = ?4 WHERE id = ?1",
            params![
                id as i64,
                read_bytes as i64,
                found_count as i64,
                elapsed_ms as i64
            ],
        )?;
        Ok(())
    }

    /// 深扫检查点写（worker 每窗口调用，**配对写**）：`v` = 安全续扫点绝对偏移，
    /// `found` = 同帧已落库条目数（= worker 的 `self.found`，本窗口条目尚未产出时恰为
    /// 「断点前已落库条目数」）。两坐标必须同一条 UPDATE 落盘：只写 `carved_offset` 而
    /// `found_count` 走节流进度，重启后 `next_idx` 会滞后于断点——续号重扫时
    /// `INSERT OR REPLACE` 按同号覆盖**断点之前**的旧行（永不重扫 → 静默丢条，spec-t7 阻断）。
    pub fn set_carved_offset(&self, id: u64, v: u64, found: u64) -> Result<(), StoreError> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET carved_offset = ?2, found_count = ?3 WHERE id = ?1",
            params![id as i64, v as i64, found as i64],
        )?;
        Ok(())
    }

    /// `INSERT OR REPLACE`：同 `(task_id, idx)` 重插=替换——供 M1c 断点续跑重扫区间复用。
    pub fn insert_entries(&self, task_id: u64, entries: &[ScanEntry]) -> Result<(), StoreError> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare(
                "INSERT OR REPLACE INTO entries
                 (task_id, idx, name, path, ext, size_bytes, deleted, is_dir, quality, first_cluster,
                  byte_offset, contiguous, record_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?;
            for e in entries {
                st.execute(params![
                    task_id as i64,
                    e.idx as i64,
                    e.name,
                    e.path,
                    e.ext,
                    e.size_bytes as i64,
                    e.deleted,
                    e.is_dir,
                    e.quality,
                    e.first_cluster as i64,
                    e.byte_offset.map(|v| v as i64),
                    e.contiguous,
                    e.record_id.map(|v| v as i64)
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_entries(&self, task_id: u64) -> Result<(), StoreError> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM entries WHERE task_id = ?1",
            params![task_id as i64],
        )?;
        Ok(())
    }

    pub fn task(&self, id: u64) -> Result<Option<TaskRow>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT id, device_id, fs, state, read_bytes, found_count, elapsed_ms, total_bytes,
                    scan_mode, carved_offset
             FROM tasks WHERE id = ?1",
        )?;
        let mut rows = st.query(params![id as i64])?;
        let Some(r) = rows.next()? else {
            return Ok(None);
        };
        let state_raw: String = r.get(3)?;
        Ok(Some(TaskRow {
            id: r.get::<_, i64>(0)? as u64,
            device_id: r.get(1)?,
            fs: r.get(2)?,
            // 库内字符串由本模块写出（INIT 无旧数据）；未知值视为 failed（诚实降级，不 panic）
            state: state_from_str(&state_raw).unwrap_or(ScanState::Failed),
            read_bytes: r.get::<_, i64>(4)? as u64,
            found_count: r.get::<_, i64>(5)? as u64,
            elapsed_ms: r.get::<_, i64>(6)? as u64,
            total_bytes: r.get::<_, i64>(7)? as u64,
            scan_mode: r.get(8)?,
            carved_offset: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
        }))
    }

    pub fn entries(
        &self,
        task_id: u64,
        offset: u64,
        limit: u64,
        deleted_only: bool,
    ) -> Result<(u64, Vec<ScanEntry>), StoreError> {
        let conn = self.conn.lock().unwrap();
        let filter = if deleted_only { " AND deleted = 1" } else { "" };
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM entries WHERE task_id = ?1{filter}"),
            params![task_id as i64],
            |r| r.get(0),
        )?;
        let mut st = conn.prepare(&format!(
            "SELECT idx, name, path, ext, size_bytes, deleted, is_dir, quality, first_cluster,
                    byte_offset, contiguous, record_id
             FROM entries WHERE task_id = ?1{filter} ORDER BY idx LIMIT ?2 OFFSET ?3"
        ))?;
        let rows = st.query_map(params![task_id as i64, limit as i64, offset as i64], |r| {
            Ok(ScanEntry {
                idx: r.get::<_, i64>(0)? as u64,
                name: r.get(1)?,
                path: r.get(2)?,
                ext: r.get(3)?,
                size_bytes: r.get::<_, i64>(4)? as u64,
                deleted: r.get(5)?,
                is_dir: r.get(6)?,
                quality: r.get(7)?,
                first_cluster: r.get::<_, i64>(8)? as u32,
                byte_offset: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                contiguous: r.get(10)?,
                record_id: r.get::<_, Option<i64>>(11)?.map(|v| v as u64),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok((total as u64, out))
    }

    /// 单条目点查（导出按 idx 取值）：缺 → `None`（调用侧映射 -32008）。
    pub fn entry(&self, task_id: u64, idx: u64) -> Result<Option<ScanEntry>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT idx, name, path, ext, size_bytes, deleted, is_dir, quality, first_cluster,
                    byte_offset, contiguous, record_id
             FROM entries WHERE task_id = ?1 AND idx = ?2",
        )?;
        let mut rows = st.query(params![task_id as i64, idx as i64])?;
        let Some(r) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(ScanEntry {
            idx: r.get::<_, i64>(0)? as u64,
            name: r.get(1)?,
            path: r.get(2)?,
            ext: r.get(3)?,
            size_bytes: r.get::<_, i64>(4)? as u64,
            deleted: r.get(5)?,
            is_dir: r.get(6)?,
            quality: r.get(7)?,
            first_cluster: r.get::<_, i64>(8)? as u32,
            byte_offset: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
            contiguous: r.get(10)?,
            record_id: r.get::<_, Option<i64>>(11)?.map(|v| v as u64),
        }))
    }

    /// daemon 启动时调：进程内 worker 已随上次退出消失——pending/scanning 诚实置 failed；
    /// paused 保留（resume 可重跑，隔天继续语义）。
    pub fn mark_interrupted(&self) -> Result<usize, StoreError> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE tasks SET state = 'failed' WHERE state IN ('pending','scanning')",
            [],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(idx: u64, name: &str, deleted: bool) -> ScanEntry {
        ScanEntry {
            idx,
            name: name.into(),
            path: "/".into(),
            ext: name
                .rsplit_once('.')
                .map(|(_, x)| x.to_lowercase())
                .unwrap_or_default(),
            size_bytes: 100 + idx,
            deleted,
            is_dir: false,
            quality: "complete".into(),
            first_cluster: 6 + idx as u32,
            byte_offset: None,
            contiguous: None,
            record_id: None,
        }
    }

    #[test]
    fn wal_normal_crash_semantics_declared() {
        // T7 阶段三选型 b 的可执行声明（模块头注的崩溃语义）：
        // 1) 文件库必须真在 WAL + synchronous=NORMAL——谁退回 DELETE/FULL（139 条/秒），此测先红；
        // 2) 崩溃可见性**形态演示（非掉电语义、无判别力）**：连接未干净关闭（mem::forget 模拟
        //    —— WAL 未 checkpoint 回主库）后重开，已提交条目即见（WAL 恢复）。同形状下
        //    DELETE+FULL 亦 100% 可见（qual 探针），故此半段只演示「未干净关闭可见性」形态；
        //    掉电语义由模块头注声明承担（可能丢最后一次 checkpoint 之后的提交），不由此测钉死。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.db");
        let id = {
            let s = Store::open(&path).unwrap();
            let (mode, sync): (String, i64) = s
                .conn
                .lock()
                .unwrap()
                .query_row(
                    "SELECT (SELECT * FROM pragma_journal_mode), (SELECT * FROM pragma_synchronous)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(mode, "wal", "文件库必须在 WAL（写放大修复的承重点）");
            assert_eq!(
                sync, 1,
                "synchronous=NORMAL=1；FULL=2 会把每条目 fsync 带回来"
            );
            let id = s.create_task("d", "exfat", "deep", 9).unwrap();
            s.insert_entries(id, &[entry(0, "LANDED.JPG", true)])
                .unwrap();
            std::mem::forget(s); // 崩溃模拟：不关闭连接（无 checkpoint、无 journal 清理）
            id
        };
        let s2 = Store::open(&path).unwrap();
        let (total, page) = s2.entries(id, 0, 10, false).unwrap();
        assert_eq!(
            (total, page[0].name.as_str()),
            (1, "LANDED.JPG"),
            "已提交条目必须跨未干净关闭存活（WAL 恢复）"
        );
        assert_eq!(s2.task(id).unwrap().unwrap().state, ScanState::Scanning);
    }

    #[test]
    fn create_task_defaults_to_scanning() {
        let s = Store::open_memory().unwrap();
        let id = s
            .create_task("image:test.img", "exfat", "quick", 4096)
            .unwrap();
        assert_eq!(id, 1);
        let t = s.task(id).unwrap().unwrap();
        assert_eq!(t.state, ScanState::Scanning);
        assert_eq!(t.device_id, "image:test.img");
        assert_eq!(t.total_bytes, 4096);
        assert_eq!((t.read_bytes, t.found_count, t.elapsed_ms), (0, 0, 0));
        assert_eq!(t.scan_mode, "quick");
        assert!(s.task(99).unwrap().is_none());
    }

    #[test]
    fn insert_and_page_entries() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", "quick", 1).unwrap();
        let all = vec![
            entry(0, "A.JPG", true),
            entry(1, "B.TXT", false),
            entry(2, "C.BIN", false),
        ];
        s.insert_entries(id, &all).unwrap();
        let (total, page) = s.entries(id, 1, 2, false).unwrap();
        assert_eq!(total, 3);
        assert_eq!(page, vec![all[1].clone(), all[2].clone()]);
        let (_, empty) = s.entries(id, 3, 2, false).unwrap();
        assert!(empty.is_empty(), "offset 越尾 → 空页（非错）");
    }

    #[test]
    fn entry_point_lookup_hits_misses_and_task_scope() {
        let s = Store::open_memory().unwrap();
        let a = s.create_task("d", "fat", "quick", 1).unwrap();
        let b = s.create_task("d", "fat", "quick", 1).unwrap();
        s.insert_entries(a, &[entry(0, "A.JPG", true), entry(3, "B.TXT", false)])
            .unwrap();
        assert_eq!(s.entry(a, 3).unwrap().unwrap().name, "B.TXT");
        assert!(s.entry(a, 1).unwrap().is_none(), "缺 idx → None（-32008）");
        assert!(s.entry(b, 0).unwrap().is_none(), "跨任务不串（-32008）");
        assert!(s.entry(99, 0).unwrap().is_none(), "未知任务 → None");
    }

    #[test]
    fn open_read_only_sees_committed_rows_but_rejects_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro.db");
        let id = {
            let w = Store::open(&path).unwrap();
            let id = w.create_task("d", "exfat", "quick", 4096).unwrap();
            w.insert_entries(id, &[entry(0, "A.JPG", false)]).unwrap();
            id
        }; // 写连接关闭（WAL checkpoint 回主库）
        let ro = Store::open_read_only(&path).unwrap();
        let row = ro.task(id).unwrap().unwrap();
        assert_eq!((row.fs.as_str(), row.device_id.as_str()), ("exfat", "d"));
        assert_eq!(ro.entry(id, 0).unwrap().unwrap().name, "A.JPG");
        assert!(
            matches!(
                ro.create_task("d", "exfat", "quick", 1),
                Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(e, _)))
                    if e.code == rusqlite::ErrorCode::ReadOnly
            ),
            "只读连接写 = ReadOnly 错误（子进程不写库）"
        );
    }

    #[test]
    fn deleted_only_filter_counts_and_pages() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", "quick", 1).unwrap();
        s.insert_entries(
            id,
            &[
                entry(0, "A.JPG", true),
                entry(1, "B.TXT", false),
                entry(2, "C.JPG", true),
            ],
        )
        .unwrap();
        let (total, page) = s.entries(id, 0, 10, true).unwrap();
        assert_eq!(total, 2);
        assert_eq!(
            page.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["A.JPG", "C.JPG"]
        );
    }

    #[test]
    fn unicode_names_roundtrip() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "exfat", "quick", 1).unwrap();
        let e = entry(0, "照片 ①🌸.JPG", true);
        s.insert_entries(id, std::slice::from_ref(&e)).unwrap();
        let (_, page) = s.entries(id, 0, 1, false).unwrap();
        assert_eq!(page[0], e, "UTF-8 名字一字不差（exFAT 红利不得被库层吃掉）");
    }

    #[test]
    fn reinsert_same_key_replaces_row() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", "quick", 1).unwrap();
        s.insert_entries(id, &[entry(0, "OLD.JPG", true)]).unwrap();
        let mut new = entry(0, "NEW.JPG", false);
        new.path = "/NEW.JPG".into();
        new.ext = "jpg".into();
        new.size_bytes = 999;
        new.quality = "maybeDamaged".into();
        new.first_cluster = 42;
        s.insert_entries(id, std::slice::from_ref(&new)).unwrap();
        let (total, page) = s.entries(id, 0, 10, false).unwrap();
        assert_eq!(total, 1, "同 (task_id, idx) 替换不增行");
        assert_eq!(
            page,
            vec![new],
            "全字段被新行覆盖（M1c 断点续跑重插同一 idx）"
        );
        let (total2, _) = {
            s.insert_entries(id, &[entry(1, "OTHER.JPG", false)])
                .unwrap();
            s.entries(id, 0, 10, false).unwrap()
        };
        assert_eq!(total2, 2, "不同 idx 正常追加，REPLACE 不误伤新行");
    }

    #[test]
    fn set_progress_roundtrips() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", "quick", 10).unwrap();
        s.set_progress(id, 7, 3, 250).unwrap();
        let t = s.task(id).unwrap().unwrap();
        assert_eq!(
            (t.read_bytes, t.found_count, t.elapsed_ms),
            (7, 3, 250),
            "三列各归其位"
        );
        assert_eq!(t.total_bytes, 10, "total_bytes 不被 set_progress 触碰");
    }

    #[test]
    fn unknown_state_reads_as_failed() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", "quick", 1).unwrap();
        s.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE tasks SET state = 'bogus' WHERE id = ?1",
                params![id as i64],
            )
            .unwrap();
        assert_eq!(
            s.task(id).unwrap().unwrap().state,
            ScanState::Failed,
            "未知态诚实降级"
        );
    }

    #[test]
    fn entries_and_clear_are_task_scoped() {
        let s = Store::open_memory().unwrap();
        let a = s.create_task("d", "fat", "quick", 1).unwrap();
        let b = s.create_task("d", "fat", "quick", 1).unwrap();
        s.insert_entries(a, &[entry(0, "A.JPG", false)]).unwrap();
        s.insert_entries(b, &[entry(0, "B.JPG", false)]).unwrap();
        let (ta, pa) = s.entries(a, 0, 10, false).unwrap();
        assert_eq!((ta, pa.len()), (1, 1));
        assert_eq!(pa[0].name, "A.JPG", "不泄漏他任务条目");
        s.clear_entries(a).unwrap();
        assert_eq!(s.entries(b, 0, 10, false).unwrap().0, 1, "clear 只清本任务");
    }

    #[test]
    fn clear_entries_empties_task() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", "quick", 1).unwrap();
        s.insert_entries(id, &[entry(0, "A", false)]).unwrap();
        s.clear_entries(id).unwrap();
        assert_eq!(s.entries(id, 0, 10, false).unwrap().0, 0);
    }

    #[test]
    fn mark_interrupted_fails_active_but_keeps_paused_and_terminal() {
        let s = Store::open_memory().unwrap();
        let a = s.create_task("d", "fat", "quick", 1).unwrap(); // scanning
        let b = s.create_task("d", "fat", "quick", 1).unwrap();
        s.set_state(b, ScanState::Paused).unwrap();
        let c = s.create_task("d", "fat", "quick", 1).unwrap();
        s.set_state(c, ScanState::Completed).unwrap();
        assert_eq!(s.mark_interrupted().unwrap(), 1);
        assert_eq!(s.task(a).unwrap().unwrap().state, ScanState::Failed);
        assert_eq!(
            s.task(b).unwrap().unwrap().state,
            ScanState::Paused,
            "paused 可隔天继续"
        );
        assert_eq!(s.task(c).unwrap().unwrap().state, ScanState::Completed);
    }

    #[test]
    fn set_state_if_active_guards_terminal_states() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", "quick", 1).unwrap();
        s.set_state(id, ScanState::Completed).unwrap();
        s.set_state_if_active(id, ScanState::Paused).unwrap();
        assert_eq!(
            s.task(id).unwrap().unwrap().state,
            ScanState::Completed,
            "终态不被暂停覆写"
        );
        let id2 = s.create_task("d", "fat", "quick", 1).unwrap();
        s.set_state_if_active(id2, ScanState::Canceled).unwrap();
        assert_eq!(s.task(id2).unwrap().unwrap().state, ScanState::Canceled);
    }

    #[test]
    fn byte_offset_roundtrips_and_defaults_null() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "exfat", "quick", 1).unwrap();
        let mut carved = entry(0, "", true);
        carved.quality = "carved".into();
        carved.byte_offset = Some(835584);
        s.insert_entries(id, &[carved.clone(), entry(1, "A.TXT", false)])
            .unwrap();
        let (_, page) = s.entries(id, 0, 10, false).unwrap();
        assert_eq!(page[0].byte_offset, Some(835584));
        assert_eq!(page[1].byte_offset, None);
    }

    #[test]
    fn record_id_roundtrips_and_defaults_null() {
        // 三态（None/Some(0)/Some(n)）各自往返：None 不得被压成 0（= 伪造「记录号 0」，
        // NTFS 记录 0 = $MFT 本身、ext4 inode 0 为保留号，都会把读取路径带向错误记录）。
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "ntfs", "quick", 1).unwrap();
        let mut zero = entry(1, "MFT0.BIN", false);
        zero.record_id = Some(0); // 0 是合法记录号（三态：None=未知 / 0=$MFT / n=记录号）
        let mut rec = entry(2, "IMG.JPG", true);
        rec.record_id = Some(42);
        s.insert_entries(id, &[entry(0, "OLD.TXT", false), zero, rec])
            .unwrap();
        let (_, page) = s.entries(id, 0, 10, false).unwrap();
        assert_eq!(
            page[0].record_id, None,
            "None 往返（fat/exfat/雕刻件/旧行）"
        );
        assert_eq!(
            page[1].record_id,
            Some(0),
            "0 是合法记录号，不得被当未知抹掉"
        );
        assert_eq!(page[2].record_id, Some(42), "非零记录号往返");
        assert_eq!(s.entry(id, 2).unwrap().unwrap().record_id, Some(42));
    }

    #[test]
    fn v1_database_migrates_to_v6() {
        // 手工造 v1 库（无 byte_offset/contiguous/record_id 列、user_version=1，含一条真实旧行）→ Store::open 迁移后可读写
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, device_id TEXT NOT NULL, fs TEXT NOT NULL,
                     state TEXT NOT NULL, read_bytes INTEGER NOT NULL DEFAULT 0, found_count INTEGER NOT NULL DEFAULT 0,
                     elapsed_ms INTEGER NOT NULL DEFAULT 0, total_bytes INTEGER NOT NULL);
                 CREATE TABLE entries (task_id INTEGER NOT NULL, idx INTEGER NOT NULL, name TEXT NOT NULL, path TEXT NOT NULL,
                     ext TEXT NOT NULL, size_bytes INTEGER NOT NULL, deleted INTEGER NOT NULL, is_dir INTEGER NOT NULL,
                     quality TEXT NOT NULL, first_cluster INTEGER NOT NULL, PRIMARY KEY (task_id, idx));
                 INSERT INTO tasks (id, device_id, fs, state, total_bytes)
                     VALUES (1, 'image:v1.img', 'fat', 'completed', 512);
                 INSERT INTO entries (task_id, idx, name, path, ext, size_bytes, deleted, is_dir, quality, first_cluster)
                     VALUES (1, 0, 'OLD_V1.JPG', '/', 'jpg', 111, 1, 0, 'complete', 6);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let ver: i64 = s
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, 6, "迁移后版本标记必须前进到 6（v1 连跳 v2..v6）");
        // 迁移前已存在的旧行：偏移未知，必须读回 NULL（DEFAULT 0 会把未知伪造成「偏移=0」）
        let (_, old) = s.entries(1, 0, 10, false).unwrap();
        assert_eq!(
            old[0].byte_offset, None,
            "迁移前旧行未知必须 NULL——DEFAULT 0 伪造「偏移=0」"
        );
        assert_eq!(
            old[0].contiguous, None,
            "迁移前旧行拓扑未知必须 NULL——DEFAULT 1 会伪造「连续」（读取猜连续=错报）"
        );
        assert_eq!(
            old[0].record_id, None,
            "迁移前旧行无记录号必须 NULL——DEFAULT 0 会把未知伪造成「记录号=0」（读取按 0 猜读）"
        );
        assert_eq!(
            s.task(1).unwrap().unwrap().carved_offset,
            None,
            "旧行无深扫检查点：必须 NULL（DEFAULT 0 会把未知伪造成「断点=0」→ 重启整段重扫）"
        );
        let id = s.create_task("d", "exfat", "quick", 1).unwrap();
        let mut e = entry(0, "OLD.JPG", true);
        e.byte_offset = Some(4096);
        s.insert_entries(id, &[e]).unwrap();
        assert_eq!(
            s.entries(id, 0, 10, false).unwrap().1[0].byte_offset,
            Some(4096)
        );
    }

    #[test]
    fn v2_database_migrates_to_v6_and_old_rows_default_quick() {
        // 手工造 v2 库（有 byte_offset、无 scan_mode、user_version=2，含一条真实旧行）→
        // Store::open 迁移后旧行 scan_mode == 'quick'（M1c 前只有 quick），且新任务可写 deep。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, device_id TEXT NOT NULL, fs TEXT NOT NULL,
                     state TEXT NOT NULL, read_bytes INTEGER NOT NULL DEFAULT 0, found_count INTEGER NOT NULL DEFAULT 0,
                     elapsed_ms INTEGER NOT NULL DEFAULT 0, total_bytes INTEGER NOT NULL);
                 CREATE TABLE entries (task_id INTEGER NOT NULL, idx INTEGER NOT NULL, name TEXT NOT NULL, path TEXT NOT NULL,
                     ext TEXT NOT NULL, size_bytes INTEGER NOT NULL, deleted INTEGER NOT NULL, is_dir INTEGER NOT NULL,
                     quality TEXT NOT NULL, first_cluster INTEGER NOT NULL, byte_offset INTEGER,
                     PRIMARY KEY (task_id, idx));
                 INSERT INTO tasks (id, device_id, fs, state, total_bytes)
                     VALUES (1, 'image:v2.img', 'exfat', 'completed', 512);
                 PRAGMA user_version = 2;",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let ver: i64 = s
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, 6, "v2 库必须前进到 6");
        assert_eq!(
            s.task(1).unwrap().unwrap().scan_mode,
            "quick",
            "迁移前旧行（M1c 前只有快扫）必须读回 quick——NULL/空串会把断点续跑分派错"
        );
        let id = s.create_task("d", "exfat", "deep", 1).unwrap();
        assert_eq!(s.task(id).unwrap().unwrap().scan_mode, "deep");
    }

    #[test]
    fn v3_database_migrates_to_v6_and_old_rows_have_null_checkpoint() {
        // 手工造 v3 库（有 scan_mode、无 carved_offset、user_version=3，含一条 deep 旧行）→
        // Store::open 迁移后旧行 carved_offset == None（从未写过检查点），且可写入新值。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v3.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, device_id TEXT NOT NULL, fs TEXT NOT NULL,
                     state TEXT NOT NULL, read_bytes INTEGER NOT NULL DEFAULT 0, found_count INTEGER NOT NULL DEFAULT 0,
                     elapsed_ms INTEGER NOT NULL DEFAULT 0, total_bytes INTEGER NOT NULL,
                     scan_mode TEXT NOT NULL DEFAULT 'quick');
                 CREATE TABLE entries (task_id INTEGER NOT NULL, idx INTEGER NOT NULL, name TEXT NOT NULL, path TEXT NOT NULL,
                     ext TEXT NOT NULL, size_bytes INTEGER NOT NULL, deleted INTEGER NOT NULL, is_dir INTEGER NOT NULL,
                     quality TEXT NOT NULL, first_cluster INTEGER NOT NULL, byte_offset INTEGER,
                     PRIMARY KEY (task_id, idx));
                 INSERT INTO tasks (id, device_id, fs, state, total_bytes, scan_mode)
                     VALUES (1, 'image:v3.img', 'exfat', 'paused', 4096, 'deep');
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let ver: i64 = s
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, 6, "v3 库必须前进到 6");
        assert_eq!(
            s.task(1).unwrap().unwrap().carved_offset,
            None,
            "旧行无检查点必须 NULL（0 会被当成「断点=区间起点」→ 静默整段重扫）"
        );
        s.set_carved_offset(1, 2048, 0).unwrap();
        assert_eq!(s.task(1).unwrap().unwrap().carved_offset, Some(2048));
    }

    #[test]
    fn v4_database_migrates_to_v6_and_old_rows_have_null_contiguous() {
        // 手工造 v4 库（有 byte_offset/carved_offset、无 contiguous、user_version=4，含一条旧行）→
        // 迁移后旧行 contiguous == None（未知拓扑）；新写 Some(true)/Some(false) 可往返。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v4.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, device_id TEXT NOT NULL, fs TEXT NOT NULL,
                     state TEXT NOT NULL, read_bytes INTEGER NOT NULL DEFAULT 0, found_count INTEGER NOT NULL DEFAULT 0,
                     elapsed_ms INTEGER NOT NULL DEFAULT 0, total_bytes INTEGER NOT NULL,
                     scan_mode TEXT NOT NULL DEFAULT 'quick', carved_offset INTEGER);
                 CREATE TABLE entries (task_id INTEGER NOT NULL, idx INTEGER NOT NULL, name TEXT NOT NULL, path TEXT NOT NULL,
                     ext TEXT NOT NULL, size_bytes INTEGER NOT NULL, deleted INTEGER NOT NULL, is_dir INTEGER NOT NULL,
                     quality TEXT NOT NULL, first_cluster INTEGER NOT NULL, byte_offset INTEGER,
                     PRIMARY KEY (task_id, idx));
                 INSERT INTO tasks (id, device_id, fs, state, total_bytes, scan_mode)
                     VALUES (1, 'image:v4.img', 'exfat', 'completed', 8192, 'quick');
                 INSERT INTO entries (task_id, idx, name, path, ext, size_bytes, deleted, is_dir, quality, first_cluster, byte_offset)
                     VALUES (1, 0, 'OLD_V4.BIN', '/', 'bin', 4096, 0, 0, 'complete', 5, NULL);
                 PRAGMA user_version = 4;",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let ver: i64 = s
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, 6, "v4 库必须前进到 6");
        let (_, old) = s.entries(1, 0, 10, false).unwrap();
        assert_eq!(
            old[0].contiguous, None,
            "旧行无 contiguous 必须 NULL——反构造时按 false（只信链）而非猜连续"
        );
        let id = s.create_task("d", "exfat", "quick", 1).unwrap();
        let mut a = entry(0, "A.BIN", false);
        a.contiguous = Some(true);
        let mut b = entry(1, "B.BIN", false);
        b.contiguous = Some(false);
        s.insert_entries(id, &[a, b]).unwrap();
        let (_, new) = s.entries(id, 0, 10, false).unwrap();
        assert_eq!(new[0].contiguous, Some(true));
        assert_eq!(new[1].contiguous, Some(false));
    }

    #[test]
    fn v5_database_migrates_to_v6() {
        // 手工造 v5 库（有 byte_offset/contiguous/carved_offset、无 record_id、user_version=5，
        // 含一条真实旧行）→ 迁移后旧行 record_id == None（未知记录号），新写可往返含 0。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v5.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, device_id TEXT NOT NULL, fs TEXT NOT NULL,
                     state TEXT NOT NULL, read_bytes INTEGER NOT NULL DEFAULT 0, found_count INTEGER NOT NULL DEFAULT 0,
                     elapsed_ms INTEGER NOT NULL DEFAULT 0, total_bytes INTEGER NOT NULL,
                     scan_mode TEXT NOT NULL DEFAULT 'quick', carved_offset INTEGER);
                 CREATE TABLE entries (task_id INTEGER NOT NULL, idx INTEGER NOT NULL, name TEXT NOT NULL, path TEXT NOT NULL,
                     ext TEXT NOT NULL, size_bytes INTEGER NOT NULL, deleted INTEGER NOT NULL, is_dir INTEGER NOT NULL,
                     quality TEXT NOT NULL, first_cluster INTEGER NOT NULL, byte_offset INTEGER, contiguous INTEGER,
                     PRIMARY KEY (task_id, idx));
                 INSERT INTO tasks (id, device_id, fs, state, total_bytes, scan_mode)
                     VALUES (1, 'image:v5.img', 'exfat', 'completed', 8192, 'quick');
                 INSERT INTO entries (task_id, idx, name, path, ext, size_bytes, deleted, is_dir, quality, first_cluster, byte_offset, contiguous)
                     VALUES (1, 0, 'OLD_V5.BIN', '/', 'bin', 4096, 0, 0, 'complete', 5, NULL, 1);
                 PRAGMA user_version = 5;",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let ver: i64 = s
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, 6, "v5 库必须前进到 6");
        let (_, old) = s.entries(1, 0, 10, false).unwrap();
        assert_eq!(
            old[0].record_id, None,
            "旧行无记录号必须 NULL——DEFAULT 0 会把未知伪造成「记录号=0」"
        );
        assert_eq!(
            old[0].contiguous,
            Some(true),
            "v6 迁移不得扰动既有列（v5 行拓扑原样读回）"
        );
        let id = s.create_task("d", "ext4", "quick", 1).unwrap();
        let mut a = entry(0, "A.BIN", false);
        a.record_id = Some(0);
        s.insert_entries(id, &[a]).unwrap();
        assert_eq!(
            s.entries(id, 0, 10, false).unwrap().1[0].record_id,
            Some(0),
            "迁移后的库新写 0 往返（0 是合法记录号）"
        );
    }

    #[test]
    fn contiguous_roundtrips_three_states() {
        // 三态（None/Some(true)/Some(false)）各自往返：None 不得被压成 false（= 伪造「非连续」
        // 会白白退化读取路径）或 true（= 伪造「连续」→ 错报）
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "exfat", "quick", 4096).unwrap();
        s.insert_entries(
            id,
            &[
                entry(0, "CARVED.JPG", true),
                {
                    let mut e = entry(1, "CONTIG.BIN", false);
                    e.contiguous = Some(true);
                    e
                },
                {
                    let mut e = entry(2, "CHAINED.BIN", false);
                    e.contiguous = Some(false);
                    e
                },
            ],
        )
        .unwrap();
        let (_, page) = s.entries(id, 0, 10, false).unwrap();
        assert_eq!(page[0].contiguous, None, "None 往返（雕刻/迁移前旧行）");
        assert_eq!(page[1].contiguous, Some(true), "NoFatChain 往返");
        assert_eq!(page[2].contiguous, Some(false), "FAT 链往返");
    }

    #[test]
    fn carved_offset_roundtrips_and_defaults_null() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "exfat", "deep", 4096).unwrap();
        assert_eq!(
            s.task(id).unwrap().unwrap().carved_offset,
            None,
            "新建任务 = 无检查点（不是 0）"
        );
        s.set_carved_offset(id, 0, 0).unwrap();
        assert_eq!(
            s.task(id).unwrap().unwrap().carved_offset,
            Some(0),
            "0 是合法偏移（区间起点）——三态语义：None=未知 / 0=起点 / n=断点"
        );
        // 配对写：同一 UPDATE 落 found_count——重启续跑的 idx 起点与断点必须同帧
        // （分开写会造出「断点在前、计数滞后」的库态 → 续号覆盖断点前旧行 = 静默丢条）
        s.set_carved_offset(id, 835584, 7).unwrap();
        let row = s.task(id).unwrap().unwrap();
        assert_eq!(
            (row.carved_offset, row.found_count),
            (Some(835584), 7),
            "检查点两坐标同帧落盘"
        );
    }

    #[test]
    fn file_store_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let id = {
            let s = Store::open(&path).unwrap();
            let id = s
                .create_task("image:x.img", "exfat", "quick", 8192)
                .unwrap();
            s.insert_entries(id, &[entry(0, "KEEP.JPG", true)]).unwrap();
            id
        };
        let s2 = Store::open(&path).unwrap();
        assert_eq!(s2.task(id).unwrap().unwrap().total_bytes, 8192);
        let (total, page) = s2.entries(id, 0, 10, false).unwrap();
        assert_eq!((total, page[0].name.as_str()), (1, "KEEP.JPG"));
    }
}
