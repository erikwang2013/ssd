// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 扫描任务与结果的 SQLite 持久化（设计 §4.4.4：流式落盘、分页查询、崩溃/重启后可查）。
//! 单进程单连接（`Mutex<Connection>`）：daemon 是唯一写者，不开 WAL。

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, params};

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

    fn init(&self) -> Result<(), StoreError> {
        self.conn.lock().unwrap().execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 device_id TEXT NOT NULL,
                 fs TEXT NOT NULL,
                 state TEXT NOT NULL,
                 read_bytes INTEGER NOT NULL DEFAULT 0,
                 found_count INTEGER NOT NULL DEFAULT 0,
                 elapsed_ms INTEGER NOT NULL DEFAULT 0,
                 total_bytes INTEGER NOT NULL
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
                 PRIMARY KEY (task_id, idx)
             );",
        )?;
        Ok(())
    }

    pub fn create_task(
        &self,
        device_id: &str,
        fs: &str,
        total_bytes: u64,
    ) -> Result<u64, StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tasks (device_id, fs, state, total_bytes) VALUES (?1, ?2, ?3, ?4)",
            params![
                device_id,
                fs,
                state_str(ScanState::Scanning),
                total_bytes as i64
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
                 (task_id, idx, name, path, ext, size_bytes, deleted, is_dir, quality, first_cluster)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                    e.first_cluster as i64
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
            "SELECT id, device_id, fs, state, read_bytes, found_count, elapsed_ms, total_bytes
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
            "SELECT idx, name, path, ext, size_bytes, deleted, is_dir, quality, first_cluster
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
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok((total as u64, out))
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
        }
    }

    #[test]
    fn create_task_defaults_to_scanning() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("image:test.img", "exfat", 4096).unwrap();
        assert_eq!(id, 1);
        let t = s.task(id).unwrap().unwrap();
        assert_eq!(t.state, ScanState::Scanning);
        assert_eq!(t.device_id, "image:test.img");
        assert_eq!(t.total_bytes, 4096);
        assert_eq!((t.read_bytes, t.found_count, t.elapsed_ms), (0, 0, 0));
        assert!(s.task(99).unwrap().is_none());
    }

    #[test]
    fn insert_and_page_entries() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", 1).unwrap();
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
    fn deleted_only_filter_counts_and_pages() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", 1).unwrap();
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
        let id = s.create_task("d", "exfat", 1).unwrap();
        let e = entry(0, "照片 ①🌸.JPG", true);
        s.insert_entries(id, std::slice::from_ref(&e)).unwrap();
        let (_, page) = s.entries(id, 0, 1, false).unwrap();
        assert_eq!(page[0], e, "UTF-8 名字一字不差（exFAT 红利不得被库层吃掉）");
    }

    #[test]
    fn reinsert_same_key_replaces_row() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", 1).unwrap();
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
        let id = s.create_task("d", "fat", 10).unwrap();
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
        let id = s.create_task("d", "fat", 1).unwrap();
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
        let a = s.create_task("d", "fat", 1).unwrap();
        let b = s.create_task("d", "fat", 1).unwrap();
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
        let id = s.create_task("d", "fat", 1).unwrap();
        s.insert_entries(id, &[entry(0, "A", false)]).unwrap();
        s.clear_entries(id).unwrap();
        assert_eq!(s.entries(id, 0, 10, false).unwrap().0, 0);
    }

    #[test]
    fn mark_interrupted_fails_active_but_keeps_paused_and_terminal() {
        let s = Store::open_memory().unwrap();
        let a = s.create_task("d", "fat", 1).unwrap(); // scanning
        let b = s.create_task("d", "fat", 1).unwrap();
        s.set_state(b, ScanState::Paused).unwrap();
        let c = s.create_task("d", "fat", 1).unwrap();
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
        let id = s.create_task("d", "fat", 1).unwrap();
        s.set_state(id, ScanState::Completed).unwrap();
        s.set_state_if_active(id, ScanState::Paused).unwrap();
        assert_eq!(
            s.task(id).unwrap().unwrap().state,
            ScanState::Completed,
            "终态不被暂停覆写"
        );
        let id2 = s.create_task("d", "fat", 1).unwrap();
        s.set_state_if_active(id2, ScanState::Canceled).unwrap();
        assert_eq!(s.task(id2).unwrap().unwrap().state, ScanState::Canceled);
    }

    #[test]
    fn file_store_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let id = {
            let s = Store::open(&path).unwrap();
            let id = s.create_task("image:x.img", "exfat", 8192).unwrap();
            s.insert_entries(id, &[entry(0, "KEEP.JPG", true)]).unwrap();
            id
        };
        let s2 = Store::open(&path).unwrap();
        assert_eq!(s2.task(id).unwrap().unwrap().total_bytes, 8192);
        let (total, page) = s2.entries(id, 0, 10, false).unwrap();
        assert_eq!((total, page[0].name.as_str()), (1, "KEEP.JPG"));
    }
}
