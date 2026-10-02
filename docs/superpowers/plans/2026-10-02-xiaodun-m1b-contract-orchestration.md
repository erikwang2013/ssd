# 小盾 M1b：契约 v1 + 扫描编排实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把两个引擎接到真实产品链路上：契约 v1（`scan.start/status/results/pause/resume/cancel` 请求 + `scan.progress/finished` 通知）、SQLite 结果持久化（流式、分页）、任务状态机与暂停/取消、daemon 并发（扫描线程 + stdout 串行化）、两引擎进度回调与既定的安全裁定 (a)。

**Architecture:** 沿用方案 B：`xd-core` 纯库编排（`scan_task` 状态机 + `store` 持久化），daemon 只做传输与线程托管。**证据分级铁律延续**：只读（任务持 `Box<dyn BlockDevice>`，类型系统无写路径）；崩溃隔离（worker 线程 + `catch_unwind`，引擎 panic → 任务 Failed，daemon 不崩）；诚实降级（(a) 裁定：删除项沿 stale 链走到首个失效簇止，不做连续猜读）。

**Tech Stack:** rusqlite（`bundled`，唯一的经设计文档 §4.4.4 授权的新依赖）、两引擎进度回调（向后兼容的 `scan_with_observer` 形态）、stdio JSON-RPC 通知。

**来源与前置:**
- 设计文档 §4.1（扫描链路）/§4.4（五条硬约束）/§4.5（导出校验，M1d 消费本切片契约）
- M1a2 计划「M1b 有序清单」（qual-t6）：测试内移 `#[path]`、缺口测试、**读分级同步决策 (a)**、位图缓存（延后）、read reason 枚举（归 M1d 前）、助手去重（fixtures re-export）
- M1a 计划移交注 11：`xd-fs-fat` 的 `read_file` → `read.rs` 机械拆分
- M1e 研究：`DeviceInfo` 需 `transport`；EACCES 走专门错误码（**不引入 `accessible` 字段**——device.list 零 open 铁律下无法廉价计算；改为 scan.start 返回 `-32001` 由 UI 引导 pkexec，M1d 消费）

**本轮明确不做（各自归属）**：断点续跑（顺序扫描才有真断点 → M1c）；进度"百分比"（quick scan 无线性目标总量，M1b 给 phase+readBytes+foundCount，雕刻阶段才有真百分比 → M1c）；位图/FAT 缓存（M1d 实测热点后）；`read` 的 reason 枚举（M1d 文案动工前）。

---

## 文件结构

```
proto/
├── v0/                              # 原样封存（不发版改动）
└── v1/                              # 新增：README.md + examples/*.json（golden）
crates/
├── xd-core/
│   ├── src/api.rs                   # PROTOCOL_VERSION=1；v1 类型（ScanState/ScanEntry/ScanProgress + 六个错误构造器；params 结构体归 T5）
│   ├── src/notify.rs                # 新增：通知信封（无 id 的 JSON-RPC）
│   ├── src/store.rs                 # 新增：SQLite（tasks/entries 两表；分页查询）
│   ├── src/scan_task.rs             # 新增：状态机 + worker + 进度回调 + catch_unwind
│   └── tests/contract_v1.rs         # 新增：v1 golden 双侧断言（Rust 侧）
├── xd-device/src/lib.rs             # DeviceInfo + transport（String 契约值）
├── xd-fs-fat/src/{scan.rs→scan.rs, read.rs}   # 拆分欠账（M1a 注 11）
├── xd-fs-fat/src/scan.rs            # +scan_with_observer（进度回调）
├── xd-fs-exfat/src/scan.rs          # 同上；(a) 裁定实现（read.rs 删除分支）
├── xd-fs-exfat/src/read.rs          # (a)：删除链只走不猜
└── xd-daemon/src/main.rs            # worker 线程 + stdout Mutex + 通知节流 + 路由
ui/
└── test/protocol_v1_test.dart       # v1 golden Dart 侧断言（传输层部分）
scripts/e2e-scan.sh                  # daemon 全链路 e2e（镜像 → scan → 分页 → 暂停/取消 → 重启持久）
```

---

### Task 1: 契约 v1 —— proto/v1 + xd-core 类型 + 双侧 golden

**Files:**
- Create: `proto/v1/README.md`、`proto/v1/examples/*.json`（**21 个 golden**，见下）
- Modify: `crates/xd-core/src/api.rs`（`PROTOCOL_VERSION: u32 = 1` + v1 类型）、`crates/xd-core/src/lib.rs`（+`pub mod notify;`）
- Create: `crates/xd-core/src/notify.rs`
- Create: `crates/xd-core/tests/contract_v1.rs`
- Modify: `ui/test/protocol_test.dart`（+v1 golden 断言，或新文件 `protocol_v1_test.dart`）
- Modify: `crates/xd-device/src/lib.rs`（`DeviceInfo` + `transport: Option<String>`，serde default）

**契约 v1 要点（权威定义，README 与 golden 据此）：**

```jsonc
// 方法（请求/响应）
scan.start   {device, mode?:"quick"} → {taskId, fs, totalBytes}   // fs∈{"fat","exfat"}；EACCES→-32001；未知/不支持→-32002
scan.status  {taskId} → {taskId, state, readBytes, foundCount, elapsedMs}
             // state ∈ "pending"|"scanning"|"paused"|"canceled"|"completed"|"failed"
scan.results {taskId, offset, limit, deletedOnly?} → {total, entries:[ScanEntry]}
scan.pause   {taskId} → {taskId, state:"paused"}
scan.resume  {taskId} → {taskId, state:"scanning"}
scan.cancel  {taskId} → {taskId, state:"canceled"}
// 通知（无 id；daemon→UI）
scan.progress  {taskId, state, readBytes, foundCount, elapsedMs}   // 节流 ≥250ms 或 readBytes 增量 ≥1MiB
scan.finished  {taskId, state, foundCount, elapsedMs}              // state∈completed|canceled|failed
// ScanEntry
{idx, name, path, ext, sizeBytes, deleted, isDir, quality:"complete"|"maybeDamaged", firstCluster}
// 错误
-32001 DevicePermission  "Device permission denied: <id>"
-32002 UnsupportedFs     "Unsupported file system"
-32003 TaskNotFound      "Task not found: <taskId>"
```

**v1 golden 清单（21）**：`ping.request/response`（`protocol`=1，version 用 `"<VERSION>"` 占位——沿用 v0.2.0 约定）、`device_list.request/response`（含 image 设备[transport 缺省] + 物理设备[`"transport":"usb"`]）、`scan_start.request/response`、`scan_status.request/response`、`scan_results.request/response`（含 2 条 entries：一条 deleted jpg complete、一条 live）、`scan_pause.request/response`、`scan_resume.request/response`、`scan_cancel.request/response`、`scan_progress.notification`、`scan_finished.notification`、`error_device_permission.response`、`error_unsupported_fs.response`、`error_task_not_active.response`。

- [ ] **Step 1: 写 golden（逐字，先落文件再写测试）**

（每个文件一行紧凑 JSON，风格同 v0；下方为全部内容——`<VERSION>` 占位仅 ping。）

```bash
mkdir -p proto/v1/examples
cat > proto/v1/examples/ping.request.json <<'EOF'
{"jsonrpc":"2.0","id":1,"method":"ping","params":null}
EOF
cat > proto/v1/examples/ping.response.json <<'EOF'
{"jsonrpc":"2.0","id":1,"result":{"pong":true,"version":"<VERSION>","protocol":1}}
EOF
cat > proto/v1/examples/device_list.request.json <<'EOF'
{"jsonrpc":"2.0","id":2,"method":"device.list","params":null}
EOF
cat > proto/v1/examples/device_list.response.json <<'EOF'
{"jsonrpc":"2.0","id":2,"result":{"devices":[{"id":"image:test.img","name":"test.img","kind":"image","sizeBytes":4096,"removable":false,"fsGuess":null},{"id":"unix:/dev/sdb","name":"USB Disk","kind":"physical","sizeBytes":3907029168,"removable":true,"fsGuess":null,"transport":"usb"}]}}
EOF
cat > proto/v1/examples/scan_start.request.json <<'EOF'
{"jsonrpc":"2.0","id":3,"method":"scan.start","params":{"device":"unix:/dev/sdb","mode":"quick"}}
EOF
cat > proto/v1/examples/scan_start.response.json <<'EOF'
{"jsonrpc":"2.0","id":3,"result":{"taskId":1,"fs":"exfat","totalBytes":3907029168}}
EOF
cat > proto/v1/examples/scan_status.request.json <<'EOF'
{"jsonrpc":"2.0","id":4,"method":"scan.status","params":{"taskId":1}}
EOF
cat > proto/v1/examples/scan_status.response.json <<'EOF'
{"jsonrpc":"2.0","id":4,"result":{"taskId":1,"state":"scanning","readBytes":123456,"foundCount":42,"elapsedMs":1500}}
EOF
cat > proto/v1/examples/scan_results.request.json <<'EOF'
{"jsonrpc":"2.0","id":5,"method":"scan.results","params":{"taskId":1,"offset":0,"limit":2,"deletedOnly":false}}
EOF
cat > proto/v1/examples/scan_results.response.json <<'EOF'
{"jsonrpc":"2.0","id":5,"result":{"total":42,"entries":[{"idx":0,"name":"IMG_0001.JPG","path":"/DCIM","ext":"jpg","sizeBytes":12000,"deleted":true,"isDir":false,"quality":"complete","firstCluster":6},{"idx":1,"name":"READ_ME.TXT","path":"/","ext":"txt","sizeBytes":7,"deleted":false,"isDir":false,"quality":"complete","firstCluster":9}]}}
EOF
cat > proto/v1/examples/scan_pause.request.json <<'EOF'
{"jsonrpc":"2.0","id":6,"method":"scan.pause","params":{"taskId":1}}
EOF
cat > proto/v1/examples/scan_pause.response.json <<'EOF'
{"jsonrpc":"2.0","id":6,"result":{"taskId":1,"state":"paused"}}
EOF
cat > proto/v1/examples/scan_resume.request.json <<'EOF'
{"jsonrpc":"2.0","id":8,"method":"scan.resume","params":{"taskId":1}}
EOF
cat > proto/v1/examples/scan_resume.response.json <<'EOF'
{"jsonrpc":"2.0","id":8,"result":{"taskId":1,"state":"scanning"}}
EOF
cat > proto/v1/examples/scan_cancel.request.json <<'EOF'
{"jsonrpc":"2.0","id":7,"method":"scan.cancel","params":{"taskId":1}}
EOF
cat > proto/v1/examples/scan_cancel.response.json <<'EOF'
{"jsonrpc":"2.0","id":7,"result":{"taskId":1,"state":"canceled"}}
EOF
cat > proto/v1/examples/scan_progress.notification.json <<'EOF'
{"jsonrpc":"2.0","method":"scan.progress","params":{"taskId":1,"state":"scanning","readBytes":123456,"foundCount":42,"elapsedMs":1500}}
EOF
cat > proto/v1/examples/scan_finished.notification.json <<'EOF'
{"jsonrpc":"2.0","method":"scan.finished","params":{"taskId":1,"state":"completed","foundCount":42,"elapsedMs":5000}}
EOF
cat > proto/v1/examples/error_device_permission.response.json <<'EOF'
{"jsonrpc":"2.0","id":3,"error":{"code":-32001,"message":"Device permission denied: unix:/dev/sdb"}}
EOF
cat > proto/v1/examples/error_unsupported_fs.response.json <<'EOF'
{"jsonrpc":"2.0","id":3,"error":{"code":-32002,"message":"Unsupported file system"}}
EOF
cat > proto/v1/examples/error_task_not_active.response.json <<'EOF'
{"jsonrpc":"2.0","id":9,"error":{"code":-32004,"message":"Task not active: 1"}}
EOF
```

- [ ] **Step 2: `proto/v1/README.md`**（写清：与 v0 关系 = 超集/封存 v0、协议号 1、方法表、ScanEntry、错误码表、通知语义与节流、`<VERSION>` 占位约定引用 v0 段、golden 规则引用 v0 段；声明「破坏性变更才递增协议号；本版为新增」。**错误码表含全部六个**：-32700/-32601/-32602（v0 继承，-32602 起用于参数校验与 `Cannot open device: <id>`）、-32001 `Device permission denied: <id>`、-32002 `Unsupported file system`、-32003 `Task not found: <id>`、-32004 `Task not active: <id>`、-32603 `Internal error`。**DeviceInfo 增 `transport`**：值域 `usb|mmc|nvme|sata|virtio|other`；**缺失=未知**（镜像恒缺，序列化省略 null）——v0 golden 与序列化兼容，见 T1 Step 5。）

- [ ] **Step 3: api.rs v1 类型 + notify.rs + 测试（`contract_v1.rs`）**

api.rs 追加（`#[serde(rename_all = "camelCase")]` 于各结构体；`ScanState`/`Quality` 为字符串枚举小写）：
```rust
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanEntry {
    pub idx: u64,
    pub name: String,
    pub path: String,
    pub ext: String,
    pub size_bytes: u64,
    pub deleted: bool,
    pub is_dir: bool,
    pub quality: String, // "complete" | "maybeDamaged"
    pub first_cluster: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanState { Pending, Scanning, Paused, Canceled, Completed, Failed }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress { pub task_id: u64, pub state: ScanState, pub read_bytes: u64, pub found_count: u64, pub elapsed_ms: u64 }
```
`RpcError` 追加**六个**构造器（消息文案即契约，golden 逐字断言；T5 路由直接用）：
```rust
    pub fn device_permission(id: &str) -> Self { /* -32001, "Device permission denied: {id}" */ }
    pub fn unsupported_fs() -> Self           { /* -32002, "Unsupported file system" */ }
    pub fn task_not_found(id: u64) -> Self    { /* -32003, "Task not found: {id}" */ }
    pub fn task_not_active(id: u64) -> Self   { /* -32004, "Task not active: {id}" */ }
    pub fn cannot_open(id: &str) -> Self      { /* -32602, "Cannot open device: {id}" */ }
    pub fn internal() -> Self                 { /* -32603, "Internal error" */ }
```
（字段名 `code`/`message` 以 api.rs 现状为准。）

notify.rs：
```rust
//! 服务端主动通知（JSON-RPC 2.0 无 id）。
pub fn notification(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params})
}
```

contract_v1.rs 测试（Rust 侧）：对每个 golden 断言 decode 为强类型（Request/Response/通知）并 re-encode 全等（`id` 由固定值提供）；`ping` 按 `<VERSION>` 归一（复用 v0 同法）。

- [ ] **Step 4: Dart 侧** `ui/test/protocol_v1_test.dart`：解码 21 golden（Ping/DeviceList/ScanStartResponse/ScanProgress 等最小模型）；**传输层通知路由的契约点**：注释明示「无 id 行 = 通知，由 IpcCoreClient 按 method 分发」（路由实现归 M1d 的 IpcTransport，本任务只钉 JSON 形态）。

- [ ] **Step 5: xd-device DeviceInfo + transport**（v0 序列化兼容靠 `skip_serializing_if`：镜像/未知恒省略键，v0 golden 的 encode 断言不受影响）
```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
```
`linux.rs` 的 `device_info()` 构造点填 `Some(match … { Usb=>"usb", Mmc=>"mmc", Nvme=>"nvme", Virtio=>"virtio", Sata=>"sata", Other=>"other" })`（**枚举名/映射以 linux.rs 现状为准**，实施者读码适配）；image 恒 `None`。既有构造点（handlers 测试 4 处、linux 1 处）均需补 `transport` 字段——全仓 `grep -rn "DeviceInfo {"` 收口。

- [ ] **Step 5.5: handlers.rs 的 ping 往返测试改指 v1 golden（**不做则 `PROTOCOL_VERSION=1` 立刻打红既有测试**）**：`ping_round_trip_matches_response_golden` 的两个 `include_str!` 路径 `proto/v0/examples/ping.*.json` → `proto/v1/examples/ping.*.json`（归一化逻辑不变；v0 golden 封存，protocol=0 仅作为历史快照被 envelope 层解码）。`error_round_trip_matches_response_golden` 此任务**不动**（`scan.start` 在 T1 仍未路由，v0 文案暂时成立；T5 再改）。

- [ ] **Step 6: 门禁与提交**
- `cargo test --workspace --locked`（v0 全量不回归 + 新 v1 测试）；clippy/fmt；Dart `flutter test`（新文件）
- Commit：`feat(proto): 契约 v1（scan 事件流/ScanEntry/错误码/transport）+ 双侧 golden（21 文件）`

---
### Task 2: xd-core::store —— SQLite 任务与结果持久化

**Files:**
- Modify: `Cargo.toml`（workspace `[workspace.dependencies]` 加 `rusqlite`——用 `cargo add` 定版，不手写版本号）
- Modify: `crates/xd-core/Cargo.toml`（`rusqlite = { workspace = true }`）
- Create: `crates/xd-core/src/store.rs`（含 `mod tests`）
- Modify: `crates/xd-core/src/lib.rs`（`pub mod store;`）

**依赖定版（命令，勿手写版本）：**
```bash
cargo add rusqlite -p xd-core --features bundled
```
（随后把生成的 `rusqlite = "x.y"` 提为 workspace 依赖、xd-core 引 `workspace = true`——与仓库既有依赖风格一致。）

> **执行后同步（T2）**：定版为 rusqlite **0.40.2**（bundled，libsqlite3-sys 0.38.2）。本任务正文代码**非 rustfmt-clean**——`store.rs` 以 rustfmt 后形态为准（语义经 token 级比对：7 处非空白差异全为尾逗号/let-else 展开，68/68 字符串常量逐字相同）。测试清单执行后为 **12 个**（计划 8 + `reinsert_same_key_replaces_row`〔INSERT OR REPLACE 同键替换〕/ `set_progress_roundtrips` / `unknown_state_reads_as_failed` / `entries_and_clear_are_task_scoped`——后三者为 qual 缺口补测，代码以 `store.rs` 为准）。

- [ ] **Step 1: 写 store.rs（完整代码；新文件首行带水印头）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
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

    pub fn create_task(&self, device_id: &str, fs: &str, total_bytes: u64) -> Result<u64, StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tasks (device_id, fs, state, total_bytes) VALUES (?1, ?2, ?3, ?4)",
            params![device_id, fs, state_str(ScanState::Scanning), total_bytes as i64],
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

    pub fn set_progress(&self, id: u64, read_bytes: u64, found_count: u64, elapsed_ms: u64) -> Result<(), StoreError> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET read_bytes = ?2, found_count = ?3, elapsed_ms = ?4 WHERE id = ?1",
            params![id as i64, read_bytes as i64, found_count as i64, elapsed_ms as i64],
        )?;
        Ok(())
    }

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
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM entries WHERE task_id = ?1", params![task_id as i64])?;
        Ok(())
    }

    pub fn task(&self, id: u64) -> Result<Option<TaskRow>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT id, device_id, fs, state, read_bytes, found_count, elapsed_ms, total_bytes
             FROM tasks WHERE id = ?1",
        )?;
        let mut rows = st.query(params![id as i64])?;
        let Some(r) = rows.next()? else { return Ok(None) };
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
```

- [ ] **Step 2: store.rs 内 `mod tests`（全代码）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(idx: u64, name: &str, deleted: bool) -> ScanEntry {
        ScanEntry {
            idx,
            name: name.into(),
            path: "/".into(),
            ext: name.rsplit_once('.').map(|(_, x)| x.to_lowercase()).unwrap_or_default(),
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
        let all = vec![entry(0, "A.JPG", true), entry(1, "B.TXT", false), entry(2, "C.BIN", false)];
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
        s.insert_entries(id, &[entry(0, "A.JPG", true), entry(1, "B.TXT", false), entry(2, "C.JPG", true)]).unwrap();
        let (total, page) = s.entries(id, 0, 10, true).unwrap();
        assert_eq!(total, 2);
        assert_eq!(page.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["A.JPG", "C.JPG"]);
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
        assert_eq!(s.task(b).unwrap().unwrap().state, ScanState::Paused, "paused 可隔天继续");
        assert_eq!(s.task(c).unwrap().unwrap().state, ScanState::Completed);
    }

    #[test]
    fn set_state_if_active_guards_terminal_states() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "fat", 1).unwrap();
        s.set_state(id, ScanState::Completed).unwrap();
        s.set_state_if_active(id, ScanState::Paused).unwrap();
        assert_eq!(s.task(id).unwrap().unwrap().state, ScanState::Completed, "终态不被暂停覆写");
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
```

- [ ] **Step 3: lib.rs 注册 + 门禁 + 提交**
```bash
cargo test -p xd-core store --locked && cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-core Cargo.toml Cargo.lock
git commit -m "feat(core): SQLite 任务/结果持久化 store.rs（分页/过滤/中断标记，8 测试）"
```

---

### Task 3: 两引擎流式观察者 `scan_with_observer`

**Files:**
- Modify: `crates/xd-fs-exfat/src/scan.rs`
- Modify: `crates/xd-fs-fat/src/scan.rs`

**设计（两引擎**同构**，后序单回调）：** 目录条目在**其子项枚举完毕后**回调一次，`quality` 已是终值（子目录不可枚举的 `MaybeDamaged` 已写回）——流式消费者（SQLite）拿到的分级与整表结果逐字一致。观察者只读，`scan()` 保持原签名（`&mut |_| {}` 包装）。

- [ ] **Step 1: exfat —— `scan` 改包装 + `scan_with_observer`**

```rust
pub fn scan(dev: &dyn BlockDevice) -> Result<Vec<ExfatEntry>, ExfatError> {
    scan_with_observer(dev, &mut |_| {})
}

/// 扫描并逐条回调 `observer`。**后序语义**：目录条目在其子项枚举完毕后回调，`quality` 为终值
/// （子目录不可枚举的降级已写回）——流式落盘与整表结果分级逐字一致。观察者只读、不得中断
/// （中断由上层取消机制处理，见 xd-core scan_task）。
pub fn scan_with_observer(
    dev: &dyn BlockDevice,
    observer: &mut dyn FnMut(&ExfatEntry),
) -> Result<Vec<ExfatEntry>, ExfatError> {
    let boot = boot::parse(dev)?;
    let fat = Fat32::new(dev, &boot);
    let mut out = Vec::new();
    let root_data = read_root_dir(dev, &boot, &fat)?;
    let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
    let bitmap = load_bitmap_from_specials(dev, &boot, &fat, &root.specials);
    scan_parsed(
        dev,
        &boot,
        &fat,
        bitmap.as_ref(),
        &root.entries,
        "/",
        0,
        &mut out,
        observer,
    )?;
    Ok(out)
}
```

`scan_parsed` 增参 `observer: &mut dyn FnMut(&ExfatEntry)`，递归透传；**在目录递归 if 块之后、循环体末尾**插入（此时 `out[pushed]` 的 quality 为终值）：

```rust
        observer(&out[pushed]);
```

- [ ] **Step 2: exfat 测试（全代码）**

```rust
    #[test]
    fn observer_streams_post_order_with_final_quality() {
        // 健康 DCIM：IMG.JPG 先于 DCIM 回调（后序）；根文件 ROOT.TXT 在 DCIM 后
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_subdir("/", "DCIM")
            .add_file("/DCIM", "IMG.JPG", &[3u8; 100])
            .add_file("/", "ROOT.TXT", b"root")
            .build();
        let (_f, dev) = dev_for(&image);
        let mut seen: Vec<String> = Vec::new();
        let entries = scan_with_observer(&dev, &mut |e| seen.push(e.name.clone())).unwrap();
        assert_eq!(seen, vec!["IMG.JPG", "DCIM", "ROOT.TXT"], "后序：子项在目录前");
        assert_eq!(entries.len(), 3, "返回值与整表同源");
    }

    #[test]
    fn observer_sees_downgraded_dir_quality() {
        // DCIM 不可枚举（首簇越界）→ observer 收到 DCIM 时 quality 已是终值 MaybeDamaged
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_subdir("/", "DCIM")
            .add_file("/DCIM", "IMG.JPG", &[3u8; 100])
            .build();
        let mut patched = image.clone();
        patched[SET + 32 + 20..SET + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let mut seen: Vec<(String, RecoverQuality)> = Vec::new();
        scan_with_observer(&dev, &mut |e| seen.push((e.name.clone(), e.quality))).unwrap();
        let dcim = seen.iter().find(|(n, _)| n == "DCIM").unwrap();
        assert_eq!(dcim.1, RecoverQuality::MaybeDamaged, "流式层拿到终值分级");
    }
```

- [ ] **Step 3: fat —— 同构改动**

`scan` 改包装；`append_parsed` 增参 `observer: &mut dyn FnMut(&FatEntry)` 并在 `if e.is_dir && !e.deleted && e.first_cluster >= 2 { … }` 块**之后**（scan.rs:169 的 `}` 与循环 `}` 之间）插入：

```rust
        observer(&out[pushed]);
```

`scan_fixed_root` / `scan_cluster_dir` 增参透传（两处递归调用点同步）。fat 测试：

```rust
    #[test]
    fn observer_streams_post_order_with_final_quality() {
        let image = xd_fixtures::FatImageBuilder::new()   // 名字以 fixtures 实际 API 为准
            .add_dir("/", "DIR")
            .add_file("/DIR", "IN.TXT", b"x")
            .add_file("/", "ROOT.TXT", b"y")
            .build();
        let (_f, dev) = dev_for(&image);
        let mut seen: Vec<String> = Vec::new();
        scan_with_observer(&dev, &mut |e| seen.push(e.name.clone())).unwrap();
        assert_eq!(seen, vec!["IN.TXT", "DIR", "ROOT.TXT"], "后序：子项在目录前");
    }
```
（fat 侧的 `dev_for`/builder 名以现有测试头部 import 为准；分级终值一test 与 exfat 同构：坏子目录链 → observer 拿到 MaybeDamaged。）

- [ ] **Step 4: 门禁与提交**（两 crate 既有测试为回归护栏：`scan()` 行为不得变）
```bash
cargo test -p xd-fs-exfat -p xd-fs-fat --locked
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-fs-exfat crates/xd-fs-fat
git commit -m "feat(fs): 两引擎 scan_with_observer 流式回调（后序/终值分级，scan() 签名不变）"
```

---

### Task 4: 安全裁定 (a) 落码 —— exFAT 删除项「链只走不猜」

**裁定（M1b，lead 定）**：`deleted && NoFatChain=0` → **只沿 stale 链**走到首个被占用/断裂簇（诚实短前缀），**绝不回退连续猜读**；分级同步（链覆盖不了 need → 永不 Complete）。依据：exFAT 删除不清 FAT（stale 链是删除留下的指纹、不是缺失），"宁可漏报不可错报"为项目铁律；碎裂场景的正解是 M1c 雕刻。`deleted && NoFatChain=1` 是规范保证的连续，照旧（位图逐簇把关）。M1a FAT 引擎**不动**（FAT 删除即清链，连续回退是唯一可得路径，语义不同）。
`read_allocation`（位图自身加载，fattab.rs）**不动**——那是引导结构的读取弹性，不是用户数据交付。

**Files:**
- Modify: `crates/xd-fs-exfat/src/read.rs`（头注 + `read_file` 早分支 + 删旧回退块 + 两探针测试）
- Modify: `crates/xd-fs-exfat/src/scan.rs`（`grade_deleted` 重写 + 一个分级测试）

> **执行后同步（重要——本任务实现含三道追加界卫，代码以仓库为准）**：计划原稿只有「链不足 need 永不 Complete」。执行中经 impl 自抓 + spec/qual 三轮，`read_file`（deleted 早分支、live 链式分支）与 `grade_deleted`（非连续臂）落成**三道诚实性界卫**：①可达界卫（起点越界或 `need > max_cluster - fc + 1` → 空 / MaybeDamaged）；②链前缀回访检测（环/回折自证伪：deleted → 空交付，live → 截断至首回访点——**刻意不对称**，证据权威论）；③live 回访扫描界 `min(need, len)`（性能）。完整裁定链与变异实证见文末执行记录 T4。下方 Step 2/3 代码块为裁定前原稿，**以仓库 `read.rs`/`scan.rs` 现状为规格**。

- [ ] **Step 1: read.rs —— 头注更新（第 2-7 行替换）**

```rust
//! 文件读取：按分配拓扑（NoFatChain 连续 / FAT 链 / 删除项 stale 链）重建字节流。
//! 交付长度 = min(VDL,DL)（T4 保证 VDL ≤ DL ⇒ 即 `size_bytes`）——`[VDL,DL)` 是未初始化区，
//! **绝不交付**；设备边界/坏读 → 诚实短前缀。
//! 删除项的分配权威是**位图**（FAT 已 stale）。**M1b (a) 裁定**：deleted+NoFatChain=0 → 只沿
//! stale 链走到首个被占用/断裂簇（诚实短前缀），绝不连续猜读；deleted+NoFatChain=1 → 连续为
//! 规范保证。碎裂删除场景的正解是 M1c 雕刻。
```

- [ ] **Step 2: read.rs —— `read_file` 早分支（插在 `let mut buf = vec![0u8; cb as usize];` 之后、live 分支之前；同时**删除**原 132-150 行的 deleted-Chain 回退块与旧 `resolve_clusters` 后的 mangle）**

```rust
    // (a) 裁定（M1b）：删除项 + 非连续 → **只沿 stale 链**（删除留下的指纹；首个被占用/坏读簇
    // 即止 → 诚实短前缀），绝不回退连续猜读——旧式"链被证伪后退连续"会交付错位数据而无从发现。
    // contiguous=true 的删除项是 NoFatChain 规范保证，不走此分支。
    if entry.deleted && !entry.contiguous {
        let chain = fat.chain(entry.first_cluster).unwrap_or_default();
        let n = (need as usize).min(chain.len());
        read_prefix(
            dev,
            &boot,
            chain[..n].iter().copied(),
            bitmap.as_ref(),
            size,
            &mut out,
            &mut buf,
        );
        out.truncate(size);
        return Ok(out);
    }
```

删除旧 mangle 后，下半段收敛为（`resolve_clusters` 在 `read_file` 中此后只服务 live-链式之外的连续路径；`Chain` 臂保留，`read_subdir_bytes`/`grade_deleted` 仍可能走）：

```rust
    let Some(resolved) = resolve_clusters(&boot, &fat, entry.first_cluster, need, entry.contiguous)
    else {
        // 起点非法 / need 超可达簇数 → 确定性为空（野生 first_cluster 同界截断，M1a I4 对等）
        return Ok(Vec::new());
    };
    match resolved {
        Resolved::Chain(chain) => read_prefix(
            dev, &boot, chain.iter().copied(), bitmap.as_ref(), size, &mut out, &mut buf,
        ),
        Resolved::Contiguous { first, n } => {
            // 逐簇 first+i，不物化连续段（qual-t5 I1：need 可被污染放大）；删除项由 read_prefix
            // 的位图关卡逐簇截断
            read_prefix(
                dev,
                &boot,
                (0..n).map(|i| (first as u64 + i) as u32),
                bitmap.as_ref(),
                size,
                &mut out,
                &mut buf,
            );
        }
    }
    out.truncate(size);
    Ok(out)
```

`read_file` 文档注释尾段同步改为：「拓扑可由 `entry.contiguous` 表述（deleted+contiguous=规范保证连续；deleted+!contiguous=按删除链；live+!contiguous=只信 FAT 链——M1d 文案据此，不得再称"可能连续猜读"）」。

- [ ] **Step 3: scan.rs —— `grade_deleted` 重写（替换 218-258 行整函数）**

```rust
/// 删除项分级（(a) 裁定版）：**非连续 → 只信 stale 链**——链覆盖不了 need（含链断裂/被清）
/// 即证据不足（MaybeDamaged），绝不按连续假设评级；连续（NoFatChain 规范保证）→ 起点/可达
/// 界卫 + 逐簇位图全空才算 Complete。位图是分配权威（FAT 对删除项已 stale）。
fn grade_deleted(
    boot: &ExfatBoot,
    fat: &Fat32,
    bitmap: Option<&Bitmap>,
    e: &ParsedEntry,
) -> RecoverQuality {
    if boot.backup_used {
        return RecoverQuality::MaybeDamaged; // 卷级几何未验证 → 封顶
    }
    let Some(bitmap) = bitmap else {
        return RecoverQuality::MaybeDamaged; // 位图不可读 → 永不给 Complete
    };
    if e.data_length == 0 {
        return RecoverQuality::Complete;
    }
    let need = e.data_length.div_ceil(boot.cluster_bytes());
    let all_free = |c: u32| matches!(bitmap.is_free(c), Ok(true));
    if !e.contiguous {
        // (a)：链只走不猜——链不足 need（含解析失败）→ 交付必短，证据不足
        let Ok(chain) = fat.chain(e.first_cluster) else {
            return RecoverQuality::MaybeDamaged;
        };
        if (chain.len() as u64) < need {
            return RecoverQuality::MaybeDamaged;
        }
        return if chain[..need as usize].iter().all(|c| all_free(*c)) {
            RecoverQuality::Complete
        } else {
            RecoverQuality::MaybeDamaged
        };
    }
    // 连续（NoFatChain 规范保证）：起点/可达界卫 + 逐簇空闲
    let max_cluster = boot.cluster_count as u64 + 1;
    if !(2..=max_cluster).contains(&(e.first_cluster as u64)) {
        return RecoverQuality::MaybeDamaged;
    }
    if need > max_cluster - e.first_cluster as u64 + 1 {
        return RecoverQuality::MaybeDamaged;
    }
    for i in 0..need {
        if !all_free((e.first_cluster as u64 + i) as u32) {
            return RecoverQuality::MaybeDamaged;
        }
    }
    RecoverQuality::Complete
}
```

**既有 15 个删除/分级测试逐一对表（实施者必须逐个复跑确认，勿想当然）**：`deleted_contiguous_is_complete`✓、`deleted_chained_stale_fat_bitmap_clear_is_complete`✓（链足 need 全空）、`deleted_with_reused_cluster_is_maybe_damaged`✓（链被重写后不足 need）、`unreadable_bitmap_never_complete`✓、`texfat_fallback_still_scans`✓、`backup_geometry_caps_deleted_quality`✓、`deleted_chain_grades_on_need_prefix_only`✓（链 [6,9,7,10] 足 need=2、前缀全空）、`deleted_reachable_exact_fit_is_complete`✓、`deleted_polluted_datalength_degrades`✓（连续：need≫可达 / fc 越界）。read.rs 侧：`deleted_chained_uses_stale_chain_when_bitmap_free`✓、`deleted_chained_occupied_middle_cluster_truncates_prefix`✓、`deleted_chain_tail_occupied_prefix_free_delivers_full`✓（链 [6,9,7,10] 足 need=2 → 前缀 [6,9]）、`deleted_with_unreadable_bitmap_uses_stale_chain`✓（`None => true` 语义保留：无从证伪 → 用链，但**只到链尾**）、`deleted_contiguous_reads_exact`✓、`polluted_lengths_read_file_stays_bounded_without_panic`✓（连续路径界卫不动）。**若发现某测试行为确有变化，停下来核对裁定语义而不是改测试**。

- [ ] **Step 4: 新探针测试（read.rs 两枚，全代码；`FAT_B` 常量已存在于 read.rs 测试头部）**

```rust
    #[test]
    fn deleted_wiped_stale_chain_delivers_honest_prefix() {
        // (a) 探针 B：碎片化删除项（链序 [6,9,7] ≠ 物理序），删除后 stale 链被清（FAT[6]=0，
        // 如部分工具删除时清链）→ 只沿链走到链断：仅簇 6 可交付（4096B）。旧式"链证伪退连续"
        // 会交付连续 [6,7,8] 的 12288B——其中 [7][8] 是他人/空闲数据，错位交付且无从发现。
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 223) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &data, &[6, 9, 7], false)
            .delete("/", "OLD.BIN")
            .build();
        let mut patched = image.clone();
        patched[FAT_B + 6 * 4..FAT_B + 6 * 4 + 4].copy_from_slice(&0u32.to_le_bytes()); // 清链首跳
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::MaybeDamaged, "链证不足 → 封顶");
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes, data[..4096], "只交付链上确证的第一个簇，绝不连续猜读");
    }

    #[test]
    fn deleted_occupied_chain_cluster_stops_even_if_contiguous_free() {
        // (a) 探针 C：链 [6,9,7]，簇 9 被 NEW.BIN 复用（位图置位、FAT[9]=EOC）→ 链从 6 走到 9
        // 即止、且簇 9 被占用 → 只交付簇 6（4096B）；连续区间 [6,7,8] 全空闲也不得猜读。
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 211) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &data, &[6, 9, 7], false)
            .delete("/", "OLD.BIN")
            .add_file_in_clusters("/", "NEW.BIN", &[5u8; 4000], &[9], true)
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "OLD.BIN")
            .unwrap();
        assert_eq!(e.quality, RecoverQuality::MaybeDamaged, "链 [6,9] < need=3");
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes, data[..4096], "被占用簇即止；连续区间空闲≠可猜");
    }
```

- [ ] **Step 5: scan.rs 分级测试（全代码）**

```rust
    #[test]
    fn deleted_short_stale_chain_never_complete() {
        // (a)：碎片化删除项 stale 链被清（FAT[6]=0）→ 链只剩首簇 < need → MaybeDamaged；
        // 旧式会对连续区间 [6,7,8] 全空闲错误给出 Complete（探针 B 的分级半壁）。
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "OLD.BIN", &[7u8; 9000], &[6, 9, 7], false)
            .delete("/", "OLD.BIN")
            .build();
        let mut patched = image.clone();
        patched[24 * 512 + 6 * 4..24 * 512 + 6 * 4 + 4].copy_from_slice(&0u32.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::MaybeDamaged, "链不足 need 不得按连续评级");
    }
```

- [ ] **Step 6: 门禁与提交**
```bash
cargo test -p xd-fs-exfat --locked
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-fs-exfat
git commit -m "fix(exfat): (a) 裁定——删除项非连续只沿 stale 链（探针 B/C：链证不足永不 Complete）"
```

---

### Task 5: xd-core::scan_task 编排 + handlers 路由 + 协议常量波及

**Files:**
- Modify: `crates/xd-core/Cargo.toml`（+`xd-fs-fat`、`xd-fs-exfat`；dev-deps +`xd-fixtures`）
- Modify: `crates/xd-core/src/api.rs`（params 结构体 + `task_not_active`/`cannot_open`/`internal` 构造器）
- Create: `crates/xd-core/src/scan_task.rs`（编排，全代码见下）
- Create: `crates/xd-core/src/testutil.rs`（`#[cfg(test)]` 共用：SlowDev/exfat 夹具/wait 助手）
- Modify: `crates/xd-core/src/lib.rs`（`pub mod scan_task;` + `#[cfg(test)] pub(crate) mod testutil;`）
- Modify: `crates/xd-core/src/handlers.rs`（CoreCtx 扩展开关 + 6 个路由 + 测试）
- Modify: `ui/lib/core_client/protocol.dart` 及 Dart 测试断言（协议号 0→1；grep 收尾）

- [ ] **Step 1: api.rs 追加 params 结构体（错误构造器六个已在 T1 就位）**

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStartParams {
    pub device: String,
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskIdParams {
    pub task_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResultsParams {
    pub task_id: u64,
    pub offset: u64,
    pub limit: u64,
    #[serde(default)]
    pub deleted_only: bool,
}
```
（`use serde::Deserialize;` 若未在 api.rs 中 import，按现状补。）

> **T1 执行后同步（见文末执行记录 ③）**：错误构造器六个**已全部在 T1 就位**（含 `task_not_active`/`cannot_open`/`internal`），T5 不得重复定义——本节此处的原始三枚构造器块已删除。

- [ ] **Step 2: scan_task.rs（全代码；首行水印头）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! 扫描任务编排：状态机（暂停/取消）、worker 线程、崩溃隔离（catch_unwind）、进度回调与
//! 流式落盘（store）。daemon 只做传输与线程托管（设计 §4.4）。
//! 取消用 `panic_any(ScanCanceled)` 在条目边界 unwind——worker 的 catch_unwind 按类型区分
//! （取消 ≠ 故障）；daemon 侧 panic hook 对该标记静默。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use xd_device::{BlockDevice, DeviceError, DeviceInfo};

use crate::api::{ScanEntry, ScanState};
use crate::store::{Store, StoreError, TaskRow, state_str};

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

#[derive(Debug)]
pub enum ProbeError {
    Unsupported,
}

/// 引导扇区签名粗筛（唯一 1 个扇区读）。exFAT 签名定死；FAT 做 0x55AA + BPB 字段合理性
/// 粗筛，完整校验留给引擎（worker 内 parse 失败 → 任务 failed）。
pub fn probe(dev: &dyn BlockDevice) -> Result<FsKind, ProbeError> {
    let mut buf = [0u8; 512];
    let n = dev.read_at(0, &mut buf).map_err(|_| ProbeError::Unsupported)?;
    if n >= 11 && &buf[3..11] == b"EXFAT   " {
        return Ok(FsKind::Exfat);
    }
    if n >= 512 {
        let bps = u16::from_le_bytes([buf[11], buf[12]]);
        let spc = buf[13];
        let fats = buf[16];
        let sig_ok = buf[510] == 0x55 && buf[511] == 0xAA;
        if sig_ok && matches!(bps, 512 | 1024 | 2048 | 4096) && spc.is_power_of_two() && (1..=2).contains(&fats) {
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

#[derive(Default)]
struct Ctrl {
    paused: AtomicBool,
    canceled: AtomicBool,
}

struct Active {
    device: Arc<dyn BlockDevice>,
    ctrl: Arc<Ctrl>,
    running: Arc<AtomicBool>,
}

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
        let id = self.store.create_task(&device.info().id, fs.as_str(), total)?;
        self.spawn(id, device);
        Ok(ScanStarted {
            task_id: id,
            fs,
            total_bytes: total,
        })
    }

    fn spawn(&self, id: u64, device: Arc<dyn BlockDevice>) {
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
        std::thread::spawn(move || {
            run_worker(id, device, ctrl, store, notify);
            running.store(false, Ordering::SeqCst);
        });
    }

    fn active_of(&self, id: u64) -> Option<(Arc<Ctrl>, Arc<AtomicBool>, Arc<dyn BlockDevice>)> {
        self.tasks
            .lock()
            .unwrap()
            .get(&id)
            .map(|a| (a.ctrl.clone(), a.running.clone(), a.device.clone()))
    }

    pub fn status(&self, id: u64) -> Result<TaskRow, ScanError> {
        self.store.task(id)?.ok_or(ScanError::TaskNotFound(id))
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

    /// 重启后重跑（quick scan 重跑成本低；真断点续跑归 M1c）：清旧结果 → 重新入册开跑。
    pub fn restart(&self, id: u64, device: Arc<dyn BlockDevice>) -> Result<(), ScanError> {
        if self.status(id)?.state != ScanState::Paused {
            return Err(ScanError::TaskNotActive(id));
        }
        probe(&*device).map_err(|_| ScanError::UnsupportedFs)?;
        self.store.clear_entries(id)?;
        self.store.set_state(id, ScanState::Scanning)?;
        self.spawn(id, device);
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
        let _ = self.store.insert_entries(self.task_id, std::slice::from_ref(&entry)); // 库错不中断扫描
        let now = Instant::now();
        let read = self.bytes.load(Ordering::Relaxed);
        if now.duration_since(self.last_notify_at) >= Duration::from_millis(250)
            || read.saturating_sub(self.last_notify_bytes) >= 1024 * 1024
        {
            let elapsed = self.start.elapsed().as_millis() as u64;
            let _ = self.store.set_progress(self.task_id, read, self.found, elapsed);
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

fn run_worker(id: u64, device: Arc<dyn BlockDevice>, ctrl: Arc<Ctrl>, store: Arc<Store>, notify: NotifyFn) {
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
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
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
```
注意：worker 自己也重新 `probe`（用户取消/暂停前先确认引擎选择一致；start 时的 probe 只用于响应与建表）。`FsKind` 与 start 时一致（设备不变）。

- [ ] **Step 3: testutil.rs（全 crate 测试共用；`#[cfg(test)]`）**

```rust
//! 测试共用（cfg(test) 全 crate 可见）：慢速设备（暂停/取消确定性）、exfat 夹具、状态轮询。
use std::sync::Arc;
use std::time::{Duration, Instant};

use xd_device::{BlockDevice, DeviceError, DeviceInfo};
use xd_device::image::ImageFileDevice;

use crate::api::ScanState;
use crate::scan_task::ScanManager;

/// 每次 read_at 睡 `delay`：让扫描的条目边界在测试里可观测、可驻停。
pub struct SlowDev {
    inner: Arc<dyn BlockDevice>,
    delay: Duration,
}

impl SlowDev {
    pub fn new(inner: Arc<dyn BlockDevice>, delay: Duration) -> Arc<dyn BlockDevice> {
        Arc::new(Self { inner, delay })
    }
}

impl BlockDevice for SlowDev {
    fn info(&self) -> &DeviceInfo {
        self.inner.info()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        std::thread::sleep(self.delay);
        self.inner.read_at(offset, buf)
    }
}

/// 写镜像字节到临时文件并打开为设备（临时文件由调用方持有存活）。
pub fn dev_from_bytes(bytes: &[u8]) -> (tempfile::NamedTempFile, Arc<dyn BlockDevice>) {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(bytes).unwrap();
    f.flush().unwrap();
    let dev = ImageFileDevice::open(f.path()).unwrap();
    (f, Arc::new(dev))
}

/// 小 exfat 夹具：2 live 文件 + 1 删除文件（**共 3 条目**——T5/T8 全链断言 `foundCount==3`、idx 集合 0..3；
/// 计划初稿曾只放 2 条目，与断言不一致，已修正）。
pub fn exfat_fixture() -> (tempfile::NamedTempFile, Arc<dyn BlockDevice>) {
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "LIVE_A.TXT", b"aaaa")
        .add_file("/", "LIVE_B.PNG", &[5u8; 100])
        .add_file("/", "DEL_ME.JPG", &[7u8; 9000])
        .delete("/", "DEL_ME.JPG")
        .build();
    dev_from_bytes(&image)
}

/// 轮询到目标态（超时 panic 带现场）。
pub fn wait_for_state(mgr: &ScanManager, id: u64, want: ScanState, timeout: Duration) -> crate::store::TaskRow {
    let deadline = Instant::now() + timeout;
    loop {
        let t = mgr.status(id).unwrap();
        if t.state == want {
            return t;
        }
        assert!(
            Instant::now() < deadline,
            "state stuck at {:?} (wanted {want:?}), task={t:?}",
            t.state
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
```
（fixtures builder 名字以实际 API 为准——`add_file`/`delete` 已在上游测试中如此使用。）

- [ ] **Step 4: scan_task.rs 的 `mod tests`（全代码）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
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
        assert_eq!(del.name, "DEL_ME.JPG", "exFAT 删除名一字不差（流式落盘不丢）");
        assert_eq!(del.quality, "complete");
        let (dtotal, dpage) = m.results(s.task_id, 0, 10, true).unwrap();
        assert_eq!((dtotal, dpage.len()), (1, 1));
    }

    #[test]
    fn pause_freezes_reads_and_resume_completes() {
        let m = mgr();
        let (_f, dev) = exfat_fixture();
        let slow = crate::testutil::SlowDev::new(dev, Duration::from_millis(30));
        let s = m.start(slow).unwrap();
        m.pause(s.task_id).unwrap();
        let row = wait_for_state(&m, s.task_id, ScanState::Paused, Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(250)); // 驻停稳定窗口
        let a = m.status(s.task_id).unwrap().read_bytes;
        std::thread::sleep(Duration::from_millis(200));
        let b = m.status(s.task_id).unwrap().read_bytes;
        assert_eq!(a, b, "驻停后读量冻结（worker 在条目边界不再前进）");
        m.resume(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Completed, Duration::from_secs(20));
        // 注意：row 的 read/found 可能为 0（暂停早于首条）——此处只断言终态与条目完整性
        let (total, _) = m.results(s.task_id, 0, 10, false).unwrap();
        assert_eq!(total, 3);
        let _ = row;
    }

    #[test]
    fn pause_before_first_entry_still_freezes_and_completes() {
        // 与上测试同构但覆盖「暂停先于任何条目」：暂停后 found 可为 0，resume 后必须 3 条全到
        let m = mgr();
        let (_f, dev) = exfat_fixture();
        let slow = crate::testutil::SlowDev::new(dev, Duration::from_millis(20));
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
        let slow = crate::testutil::SlowDev::new(dev, Duration::from_millis(30));
        let s = m.start(slow).unwrap();
        m.cancel(s.task_id).unwrap();
        wait_for_state(&m, s.task_id, ScanState::Canceled, Duration::from_secs(10));
        let done = events
            .lock()
            .unwrap()
            .iter()
            .find(|v| v["method"] == "scan.finished")
            .cloned()
            .expect("必须发出 scan.finished");
        assert_eq!(done["params"]["state"], "canceled");
        assert!(m.results(s.task_id, 0, 10, false).is_ok(), "部分结果保留可查");
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
        wait_for_state(&m, s2.task_id, ScanState::Completed, Duration::from_secs(10));
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
            let slow = crate::testutil::SlowDev::new(dev, Duration::from_millis(30));
            let s = m.start(slow).unwrap();
            id = s.task_id;
            m.pause(id).unwrap();
            wait_for_state(&m, id, ScanState::Paused, Duration::from_secs(5));
        } // 模拟 daemon 退出（worker 随进程死；此处 m drop，worker 线程仍在跑——它已驻停）
        let m2 = ScanManager::new(Store::open(&path).unwrap(), Arc::new(|_| {}));
        m2.recover_after_restart().unwrap();
        assert_eq!(m2.status(id).unwrap().state, ScanState::Paused, "paused 跨重启保留");
        match m2.resume(id).unwrap() {
            Resume::NeedsDevice { device_id } => assert!(device_id.starts_with("image:")),
            Resume::InPlace => panic!("worker 已不在场，必须 NeedsDevice"),
        }
        let (_f2, dev2) = exfat_fixture();
        m2.restart(id, dev2).unwrap();
        wait_for_state(&m2, id, ScanState::Completed, Duration::from_secs(10));
        assert_eq!(m2.results(id, 0, 10, false).unwrap().0, 3, "清后重跑结果完整");
    }

    #[test]
    fn recover_after_restart_fails_interrupted_keeps_paused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let (scanning_id, paused_id) = {
            let s = Store::open(&path).unwrap();
            let a = s.create_task("image:a.img", "exfat", 1).unwrap();
            let b = s.create_task("image:b.img", "exfat", 1).unwrap();
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
```

- [ ] **Step 5: handlers.rs —— CoreCtx 扩展与 6 路由（全代码）**

**扫描/存储的导入源约定**：`ScanState` 属 `crate::api`（store.rs 私有再导入不再转出）；`state_str`/`TaskRow`/`Store`/`StoreError` 属 `crate::store`；`DeviceOpener`/`OpenError`/`NoopOpener`/`Resume`/`ScanError`/`ScanManager` 属 `crate::scan_task`。全任务代码按此导入，勿从 store re-export。

`CoreCtx` 结构体与构造改（devices 变 `Arc`；新增 `scans`/`opener`）：

```rust
pub struct CoreCtx {
    devices: Vec<Arc<dyn BlockDevice>>,
    /// 枚举到但**未打开**的设备（device.list 用；零 open()——M1e 契约要求）。
    list_only: Vec<DeviceInfo>,
    scans: Arc<ScanManager>,
    opener: Arc<dyn DeviceOpener>,
}

impl CoreCtx {
    pub fn new(devices: Vec<Arc<dyn BlockDevice>>) -> Self {
        Self {
            devices,
            list_only: Vec::new(),
            scans: Arc::new(ScanManager::new(
                crate::store::Store::open_memory().expect("sqlite in-memory"),
                Arc::new(|_| {}),
            )),
            opener: Arc::new(crate::scan_task::NoopOpener),
        }
    }

    pub fn with_list_only(mut self, infos: Vec<DeviceInfo>) -> Self { /* 原样 */ }

    /// daemon 注入：真 store（文件/内存）+ stdout 通知通道 + 平台 opener。
    pub fn with_scan(mut self, scans: Arc<ScanManager>, opener: Arc<dyn DeviceOpener>) -> Self {
        self.scans = scans;
        self.opener = opener;
        self
    }

    /// 解析 scan.start/resume 的设备：已打开优先；否则懒打开（唯一 open 出口）。
    fn resolve_device(&self, id: &str) -> Result<Arc<dyn BlockDevice>, RpcError> {
        if let Some(d) = self.devices.iter().find(|d| d.info().id == id) {
            return Ok(d.clone());
        }
        match self.opener.open(id) {
            Ok(d) => Ok(d),
            Err(OpenError::PermissionDenied) => Err(RpcError::device_permission(id)),
            Err(OpenError::Other(_)) => Err(RpcError::cannot_open(id)),
        }
    }
}
```
（`resolve_device` 在 `impl CoreCtx` 内；`OpenError::Other` 的细节由 opener 实现自身 eprintln 留痕，契约只给稳定文案。）

`handle_request` 增臂 + 辅助：

```rust
        "scan.start" => scan_start(ctx, req),
        "scan.status" => scan_status(ctx, req),
        "scan.results" => scan_results(ctx, req),
        "scan.pause" => scan_pause(ctx, req),
        "scan.resume" => scan_resume(ctx, req),
        "scan.cancel" => scan_cancel(ctx, req),
```

```rust
fn parse_params<T: serde::de::DeserializeOwned>(req: &Request) -> Result<T, Response> {
    // 注意：`RpcError::invalid_params(&str)` 是 M0 既有签名（输出 "Invalid params: {message}" 前缀）
    match req.params.clone() {
        Some(v) if !v.is_null() => serde_json::from_value(v)
            .map_err(|_| err(req, RpcError::invalid_params("missing or malformed params"))),
        _ => Err(err(req, RpcError::invalid_params("missing or malformed params"))),
    }
}

fn scan_err(req: &Request, e: ScanError) -> Response {
    match e {
        ScanError::TaskNotFound(id) => err(req, RpcError::task_not_found(id)),
        ScanError::TaskNotActive(id) => err(req, RpcError::task_not_active(id)),
        ScanError::UnsupportedFs => err(req, RpcError::unsupported_fs()),
        _ => err(req, RpcError::internal()),
    }
}

fn scan_start(ctx: &CoreCtx, req: &Request) -> Response {
    let p: ScanStartParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if let Some(m) = &p.mode
        && m != "quick"
    {
        return err(req, RpcError::invalid_params("unsupported mode"));
    }
    let dev = match ctx.resolve_device(&p.device) {
        Ok(d) => d,
        Err(e) => return err(req, e),
    };
    match ctx.scans.start(dev) {
        Ok(s) => ok(
            req,
            serde_json::json!({
                "taskId": s.task_id, "fs": s.fs.as_str(), "totalBytes": s.total_bytes,
            }),
        ),
        Err(e) => scan_err(req, e),
    }
}

fn scan_status(ctx: &CoreCtx, req: &Request) -> Response {
    let p: TaskIdParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match ctx.scans.status(p.task_id) {
        Ok(t) => ok(
            req,
            serde_json::json!({
                "taskId": t.id, "state": crate::store::state_str(t.state),
                "readBytes": t.read_bytes, "foundCount": t.found_count, "elapsedMs": t.elapsed_ms,
            }),
        ),
        Err(e) => scan_err(req, e),
    }
}

fn scan_results(ctx: &CoreCtx, req: &Request) -> Response {
    let p: ScanResultsParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !(1..=1000).contains(&p.limit) {
        return err(req, RpcError::invalid_params("limit out of range 1..=1000")); // 契约
    }
    match ctx.scans.results(p.task_id, p.offset, p.limit, p.deleted_only) {
        Ok((total, entries)) => ok(req, serde_json::json!({ "total": total, "entries": entries })),
        Err(e) => scan_err(req, e),
    }
}

fn scan_pause(ctx: &CoreCtx, req: &Request) -> Response {
    let p: TaskIdParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match ctx.scans.pause(p.task_id) {
        Ok(()) => ok(req, serde_json::json!({ "taskId": p.task_id, "state": "paused" })),
        Err(e) => scan_err(req, e),
    }
}

fn scan_resume(ctx: &CoreCtx, req: &Request) -> Response {
    let p: TaskIdParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match ctx.scans.resume(p.task_id) {
        Ok(Resume::InPlace) => {
            ok(req, serde_json::json!({ "taskId": p.task_id, "state": "scanning" }))
        }
        Ok(Resume::NeedsDevice { device_id }) => {
            let dev = match ctx.resolve_device(&device_id) {
                Ok(d) => d,
                Err(e) => return err(req, e),
            };
            match ctx.scans.restart(p.task_id, dev) {
                Ok(()) => {
                    ok(req, serde_json::json!({ "taskId": p.task_id, "state": "scanning" }))
                }
                Err(e) => scan_err(req, e),
            }
        }
        Err(e) => scan_err(req, e),
    }
}

fn scan_cancel(ctx: &CoreCtx, req: &Request) -> Response {
    let p: TaskIdParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match ctx.scans.cancel(p.task_id) {
        Ok(()) => ok(req, serde_json::json!({ "taskId": p.task_id, "state": "canceled" })),
        Err(e) => scan_err(req, e),
    }
}
```
imports 更新：`use std::sync::Arc; use crate::api::{..., ScanStartParams, TaskIdParams, ScanResultsParams}; use crate::scan_task::{DeviceOpener, OpenError, Resume, ScanError, ScanManager};`

**既有 handlers 测试的 Arc 迁移**：`CoreCtx::new(vec![Box::new(dev)])` → `vec![Arc::new(dev) as Arc<dyn BlockDevice>]`（3 处 + stub 2 处）。`unknown_method_returns_minus_32601` 用例改用未实现方法名（`"scan.start"` 现在是合法方法、且无 params 会走 -32602！改钉 `"no.such.method"` 并同步 v0 golden 的 error_method_not_found 文案？golden 里 message 是 "Method not found: scan.start"——**必须换**：golden 文件的 message 改为 `Method not found: no.such.method`（id 不变）。这是 v0 golden 的必要修正（scan.start 从"不存在"变成"存在"）。

- [ ] **Step 6: handlers 新测试（全代码）**

```rust
    fn req_with(id: i64, method: &str, params: serde_json::Value) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(id),
            method: method.into(),
            params: Some(params),
        }
    }

    fn ctx_with_fixture() -> (tempfile::NamedTempFile, CoreCtx) {
        let (f, dev) = crate::testutil::exfat_fixture();
        (f, CoreCtx::new(vec![dev]))
    }

    #[test]
    fn scan_start_happy_then_status_results() {
        let (_f, ctx) = ctx_with_fixture();
        let dev_id = ctx.devices[0].info().id.clone();
        let resp = handle_request(
            &ctx,
            &req_with(3, "scan.start", serde_json::json!({"device": dev_id, "mode": "quick"})),
        );
        let Response::Ok(ok) = resp else { panic!("{:?}", resp) };
        assert_eq!(ok.result["fs"], "exfat");
        assert_eq!(ok.result["taskId"], 1);
        let total_bytes = ok.result["totalBytes"].as_u64().unwrap();
        assert!(total_bytes > 0);
        // 轮询到 completed（handler 级小图秒级）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let r = handle_request(&ctx, &req_with(4, "scan.status", serde_json::json!({"taskId": 1})));
            let Response::Ok(o) = r else { panic!() };
            if o.result["state"] == "completed" {
                assert_eq!(o.result["foundCount"], 3);
                break;
            }
            assert!(std::time::Instant::now() < deadline, "scan stuck: {o:?}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(5, "scan.results", serde_json::json!({"taskId": 1, "offset": 0, "limit": 10, "deletedOnly": true})),
        ) else {
            panic!()
        };
        assert_eq!(o.result["total"], 1);
        assert_eq!(o.result["entries"][0]["name"], "DEL_ME.JPG");
        assert_eq!(o.result["entries"][0]["deleted"], true);
        assert_eq!(o.result["entries"][0]["quality"], "complete");
        // qual-t3 纵深防御：observer 1:1 ⇒ idx 集合恰为 0..found_count（防回调重复致库内双行）
        let Response::Ok(all) = handle_request(
            &ctx,
            &req_with(6, "scan.results", serde_json::json!({"taskId": 1, "offset": 0, "limit": 10})),
        ) else {
            panic!()
        };
        let mut idxs: Vec<u64> = all.result["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["idx"].as_u64().unwrap())
            .collect();
        idxs.sort_unstable();
        assert_eq!(idxs, vec![0, 1, 2], "idx 集合 == 0..found_count");
    }

    #[test]
    fn scan_start_permission_denied_maps_to_minus_32001() {
        struct Denied;
        impl crate::scan_task::DeviceOpener for Denied {
            fn open(&self, _id: &str) -> Result<Arc<dyn BlockDevice>, crate::scan_task::OpenError> {
                Err(crate::scan_task::OpenError::PermissionDenied)
            }
        }
        let ctx = CoreCtx::new(vec![]).with_scan(
            Arc::new(ScanManager::new(
                crate::store::Store::open_memory().unwrap(),
                Arc::new(|_| {}),
            )),
            Arc::new(Denied),
        );
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(3, "scan.start", serde_json::json!({"device": "unix:/dev/sdb"})),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32001);
        assert_eq!(e.error.message, "Device permission denied: unix:/dev/sdb");
    }

    #[test]
    fn scan_start_unsupported_and_unknown_device() {
        let (_f, zeros) = crate::testutil::dev_from_bytes(&[0u8; 4096]);
        let ctx = CoreCtx::new(vec![zeros]);
        let dev_id = ctx.devices[0].info().id.clone();
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(3, "scan.start", serde_json::json!({"device": dev_id})),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32002);
        let Response::Err(e2) = handle_request(
            &ctx,
            &req_with(4, "scan.start", serde_json::json!({"device": "unix:/dev/nope"})),
        ) else {
            panic!()
        };
        assert_eq!(e2.error.code, -32602);
        assert_eq!(e2.error.message, "Cannot open device: unix:/dev/nope");
    }

    #[test]
    fn scan_task_not_found_and_invalid_params() {
        let ctx = CoreCtx::new(vec![]);
        for m in ["scan.status", "scan.pause", "scan.resume", "scan.cancel"] {
            let Response::Err(e) =
                handle_request(&ctx, &req_with(9, m, serde_json::json!({"taskId": 42})))
            else {
                panic!()
            };
            assert_eq!(e.error.code, -32003, "{m}");
            assert_eq!(e.error.message, "Task not found: 42");
        }
        let Response::Err(e) = handle_request(&ctx, &req_with(9, "scan.start", serde_json::json!({})));
        assert_eq!(e.error.code, -32602, "缺 device");
        // qual-t1 裁定 (c)：-32602 的 message 逐字钉前缀（reason 不属契约、前缀属之）
        assert_eq!(e.error.message, "Invalid params: missing or malformed params");
        let Response::Err(e2) = handle_request(
            &ctx,
            &req_with(9, "scan.results", serde_json::json!({"taskId": 1, "offset": 0, "limit": 0})),
        );
        assert_eq!(e2.error.code, -32602, "limit=0 越契约");
        assert_eq!(e2.error.message, "Invalid params: limit out of range 1..=1000");
    }

    #[test]
    fn scan_pause_on_completed_task_is_not_active() {
        use crate::api::ScanState;
        use crate::store::Store;
        let store = Store::open_memory().unwrap();
        let id = store.create_task("image:x.img", "exfat", 1).unwrap();
        store.set_state(id, ScanState::Completed).unwrap();
        let ctx = CoreCtx::new(vec![]).with_scan(
            Arc::new(ScanManager::new(store, Arc::new(|_| {}))),
            Arc::new(NoopOpener),
        );
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(9, "scan.pause", serde_json::json!({"taskId": id})),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32004);
        assert_eq!(e.error.message, format!("Task not active: {id}"));
    }

    #[test]
    fn scan_response_goldens_round_trip() {
        // 确定性构造：store 直接建任务/写进度/插条目（handler 只读路径），逐字对 golden。
        use crate::store::Store;
        let store = Store::open_memory().unwrap();
        let id = store.create_task("unix:/dev/sdb", "exfat", 3907029168).unwrap();
        assert_eq!(id, 1);
        store.set_progress(id, 123456, 42, 1500).unwrap();
        let entries: Vec<crate::api::ScanEntry> = serde_json::from_str(
            r#"[{"idx":0,"name":"IMG_0001.JPG","path":"/DCIM","ext":"jpg","sizeBytes":12000,"deleted":true,"isDir":false,"quality":"complete","firstCluster":6},
                {"idx":1,"name":"READ_ME.TXT","path":"/","ext":"txt","sizeBytes":7,"deleted":false,"isDir":false,"quality":"complete","firstCluster":9}]"#,
        )
        .unwrap();
        store.insert_entries(id, &entries).unwrap();
        let ctx = CoreCtx::new(vec![]).with_scan(
            Arc::new(ScanManager::new(store, Arc::new(|_| {}))),
            Arc::new(NoopOpener),
        );

        let golden = |name: &str| -> serde_json::Value {
            serde_json::from_str(match name {
                "status" => include_str!("../../../proto/v1/examples/scan_status.response.json").trim(),
                "results" => include_str!("../../../proto/v1/examples/scan_results.response.json").trim(),
                _ => unreachable!(),
            })
            .unwrap()
        };
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(4, "scan.status", serde_json::json!({"taskId": 1})),
        ) else {
            panic!()
        };
        assert_eq!(serde_json::to_value(&o).unwrap(), golden("status"));
        let Response::Ok(o2) = handle_request(
            &ctx,
            &req_with(5, "scan.results", serde_json::json!({"taskId": 1, "offset": 0, "limit": 2, "deletedOnly": false})),
        ) else {
            panic!()
        };
        assert_eq!(serde_json::to_value(&o2).unwrap(), golden("results"));
    }
```
（`scan.start` 响应 golden 的 taskId/totalBytes 是运行期值，用**归一化**法与既有 `<VERSION>` 先例同款：start 真扫描小图后把 `taskId`→1、`totalBytes`→3907029168 再全等比对——放在上面 happy 测试尾部或独立小测试，实施者二者取一即可，需注释「与 v0.2.0 的 `<VERSION>` 归一化同款先例」。）

`device_list` v1 golden 往返（全代码，追加进本 mod）：
```rust
    #[test]
    fn device_list_v1_golden_round_trip() {
        // 确定性构造：打开 image（transport 省略）+ list_only 物理设备（transport:"usb"）
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&[0u8; 4096]).unwrap();
        f.flush().unwrap();
        let img = ImageFileDevice::open(f.path()).unwrap();
        let ctx = CoreCtx::new(vec![Arc::new(img)]).with_list_only(vec![DeviceInfo {
            id: "unix:/dev/sdb".into(),
            name: "USB Disk".into(),
            kind: xd_device::DeviceKind::Physical,
            size_bytes: 3907029168,
            removable: true,
            fs_guess: None,
            transport: Some("usb".into()),
        }]);
        let Response::Ok(o) = handle_request(&ctx, &req(2, "device.list")) else {
            panic!()
        };
        let expected: serde_json::Value = serde_json::from_str(
            include_str!("../../../proto/v1/examples/device_list.response.json").trim(),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&o).unwrap(), expected);
    }
```

- [ ] **Step 7: 协议号波及面收尾（先 grep 再改，改完全量复跑）**

> **T1 执行后同步（见文末执行记录 ①②）**：Rust 部分已完成——`crates/xd-daemon/tests/ipc.rs:79` 已改 1；`crates/xd-core/tests/contract.rs:47`（v0 ping 期望值用活常量，grep 曾漏）已改为封存字面量 0 并留注释。**T5 仅剩 Dart 侧**（下述 grep 去掉 `crates/` 路径）。

```bash
grep -rn "protocol" ui/lib ui/test | grep -v "\.json"
```
- `ui/lib/core_client/protocol.dart`：协议常量/期望值 0 → 1（含 mismatch 判定逻辑的常量）
- `ui/test/protocol_test.dart` / `ipc_integration_test.dart` / `home_page_test.dart`：断言与 fake 的 protocol 0 → 1（v0 golden 解码测试除外——v0 文件里 protocol 仍是 0，若某测试直接读 v0 golden 解码则保持 0）

- [ ] **Step 8: 门禁与提交**
```bash
cargo test --workspace --locked          # v0+v1 契约、两引擎、daemon 既有全量不回归
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
/home/erik/flutter/bin/flutter test --no-pub   # ui/（绝对路径；见 memory）
git add -A crates/xd-core ui crates/xd-daemon/tests
git commit -m "feat(core): scan_task 编排（状态机/暂停取消/panic 隔离/流式落盘）+ 路由与协议号 1"
```

---

### Task 6: daemon 并发接线（扫描线程 + stdout 串行化 + 懒打开 + panic hook）

**Files:**
- Modify: `crates/xd-daemon/src/main.rs`
- Modify: `crates/xd-daemon/tests/ipc.rs`（+`image:` 懒开拒绝断言）
- （协议号断言已在 T5 Step 7 收尾）

> **执行后同步（T6）**：计划代码块里 `Arc<Mutex<StdoutLock<'static>>>` **在 rustc 1.99 不可编译**（`StdoutLock` 因内含 `ReentrantLockGuard` 为 `!Send`，spec 已独立复现 E0277）——实现为 `Arc<Mutex<std::io::Stdout>>`（串行化+每行 flush 语义不变）；计划其余段落照旧。另 ipc.rs 增 5 行 `XDG_STATE_HOME` 隔离（防 daemon 测试在真实 HOME 建库）。

- [ ] **Step 1: main.rs 改造（关键段落全代码）**

设备容器与 CLI 增加 `--db`：
```rust
use std::sync::{Arc, Mutex};

use xd_core::scan_task::{DeviceOpener, NotifyFn, OpenError, ScanCanceled, ScanManager};
use xd_core::store::Store;

fn default_db_path() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_STATE_HOME")
        && !x.is_empty()
    {
        return Some(PathBuf::from(x).join("xiaodun/tasks.db"));
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".local/state/xiaodun/tasks.db"))
}
```
`--image`/`--device` push 改 `Arc`：
```rust
devices.push(Arc::new(dev) as Arc<dyn BlockDevice>);
```
新增参数分支：
```rust
            "--db" => {
                let Some(path) = args.next() else {
                    eprintln!("error: --db requires a path");
                    std::process::exit(2);
                };
                db_path = Some(PathBuf::from(path));
            }
```
（`let mut db_path: Option<PathBuf> = None;` 于参数循环前。）

启动段（枚举之后）：
```rust
    // 取消用 unwind 标记作控制流：静默其对 stderr 的默认输出；真 panic 照常打印。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info
            .payload()
            .downcast_ref::<ScanCanceled>()
            .is_none()
        {
            default_hook(info);
        }
    }));

    let db = db_path.or_else(default_db_path);
    let store = match &db {
        Some(p) => {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match Store::open(p) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("warn: 任务库打开失败（{e}），改用内存库（重启后结果不保留）");
                    Store::open_memory().expect("sqlite in-memory")
                }
            }
        }
        None => Store::open_memory().expect("sqlite in-memory"),
    };

    let out: Arc<Mutex<std::io::StdoutLock<'static>>> =
        Arc::new(Mutex::new(std::io::stdout().lock()));
    let notify: NotifyFn = {
        let out = out.clone();
        Arc::new(move |v: serde_json::Value| write_line(&out, &v))
    };
    let mgr = Arc::new(ScanManager::new(store, notify));
    if let Err(e) = mgr.recover_after_restart() {
        eprintln!("warn: 遗留任务状态修复失败: {e}");
    }

    #[cfg(target_os = "linux")]
    let opener: Arc<dyn DeviceOpener> = Arc::new(DaemonOpener);
    #[cfg(not(target_os = "linux"))]
    let opener: Arc<dyn DeviceOpener> = Arc::new(xd_core::scan_task::NoopOpener);

    let ctx = CoreCtx::new(devices)
        .with_list_only(list_only)
        .with_scan(mgr, opener);
    let stdin = std::io::stdin();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                eprintln!("error: read failed: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle_request(&ctx, &req),
            Err(_) => Response::Err(RpcErr {
                jsonrpc: "2.0".into(),
                id: serde_json::Value::Null,
                error: RpcError::parse_error(),
            }),
        };
        write_line(&out, &serde_json::to_value(&response).unwrap());
    }
```
`write_line` 与平台 opener：
```rust
/// 唯一 stdout 写口（主循环与扫描 worker 的通知共用；`Mutex` 串行化整行输出）。
fn write_line(out: &Mutex<std::io::StdoutLock<'static>>, v: &serde_json::Value) {
    let mut w = out.lock().unwrap();
    let _ = writeln!(w, "{v}");
    let _ = w.flush();
}

/// 懒打开物理设备（device.list 零 open 铁律的唯一出口）。`image:` 一律拒绝——镜像只能经
/// 启动参数 `--image` 注册（root 走 privcheck 准入）；否则提权 daemon 会沦为任意路径读取器。
#[cfg(target_os = "linux")]
struct DaemonOpener;

#[cfg(target_os = "linux")]
impl DeviceOpener for DaemonOpener {
    fn open(&self, id: &str) -> Result<Arc<dyn BlockDevice>, OpenError> {
        if id.starts_with("image:") {
            return Err(OpenError::Other(
                "images must be registered via --image at startup".into(),
            ));
        }
        let Some(path) = id.strip_prefix("unix:") else {
            return Err(OpenError::Other(format!("unknown device id scheme: {id}")));
        };
        match xd_device::linux::LinuxBlockDevice::open(&PathBuf::from(path)) {
            Ok(d) => Ok(Arc::new(d)),
            Err(xd_device::DeviceError::Io(e))
                if e.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                Err(OpenError::PermissionDenied)
            }
            Err(e) => {
                eprintln!("warn: 打开设备 {id} 失败: {e}");
                Err(OpenError::Other(e.to_string()))
            }
        }
    }
}
```
（`NoopOpener` 需在 scan_task.rs 中 `pub`（T5 已定义，加 pub）。）

- [ ] **Step 2: ipc.rs**：`image:` 懒开拒绝的集成断言在 **T8 的 `scan_ipc.rs`**（`scan_start_on_unregistered_image_id_is_refused`）落地，本任务不重复造测试——T6 只需保证 CI 上 `cargo test -p xd-daemon` 全绿（T8 前该测试尚不存在，先跑既有 ipc.rs 全量）。

- [ ] **Step 3: 门禁与提交**
```bash
cargo test -p xd-daemon --locked
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-daemon
git commit -m "feat(daemon): 扫描线程接线（stdout 串行化/通知/--db/懒打开/panic hook 静默取消标记）"
```

---

### Task 7: 结构欠账清理（拆分/内移/去重；纯重构，零行为变化）

**Files:**
- Modify/Create: `crates/xd-fs-fat/src/{scan.rs, read.rs}`（M1a 注 11：`read_file` 迁出）
- Modify/Create: `crates/xd-fs-exfat/src/{scan.rs, scan_tests.rs, read.rs, read_tests.rs}`
- Modify: `crates/xd-fixtures/src/lib.rs`（+`set_checksum` / `refix_deleted_checksum`）
- Modify: `crates/xd-fs-exfat/tests/roundtrip.rs`（若其中也有重复助手，一并换用）

- [ ] **Step 1: 分级枚举补文档**（M1a2 欠账）：exfat `RecoverQuality` 两变体补 doc（对齐 fat 侧措辞，并写入 (a) 语义）：`Complete`="数据簇按拓扑（连续/链）全部确证空闲，交付长度可满"；`MaybeDamaged`="有簇已被重新分配（覆盖风险）、簇信息缺失或链证不足"。
- [ ] **Step 2: fat 拆分**：`scan.rs` 中 `read_file` 及其私有助手整体迁 `read.rs`；scan.rs 顶部留 `pub use crate::read::read_file;`（与 exfat 同款路径兼容）；`read_file` 的测试随迁 `read.rs` 的 `mod tests`。lib.rs 注册 `mod read;`（公开面不变）。
- [ ] **Step 3: exfat 测试内移**：`scan.rs` 的 `mod tests` → `scan_tests.rs`，scan.rs 顶部 `#[cfg(test)] #[path = "scan_tests.rs"] mod tests;`（**不是** tests/ 外部目录——testutil 是 crate 内 cfg(test)）；`read.rs` 同法 → `read_tests.rs`。内移后 scan.rs、read.rs 各 ≤ 500 行（仓库线宽纪律）。
- [ ] **Step 4: 助手去重**：`set_checksum`/`refix_deleted_checksum` 迁 `xd-fixtures`（`pub fn`，文档注释随迁，含"删除只清 bit7、不重算"语义）；exfat 各测试与 roundtrip 换 import，删本地副本。
- [ ] **Step 5: `linux.rs` 拆分（qual-t1 结构欠账：585 行 > 500 行规则，T1 +51）**：机械拆分——`crates/xd-device/src/linux.rs` 的枚举/归类（`BlockEnumerator`、`classify_transport`、`sysfs_transport`、transport 映射）迁 `crates/xd-device/src/linux/enumerate.rs`（或 `linux_enum.rs`，二者取一以 rustfmt/模块风格顺眼为准；`linux.rs` 保留 `pub use` 重导出 + `LinuxBlockDevice` 读取层）；`mod linux` 改为目录模块若选目录形态。**公开路径不变**（`xd_device::linux::*` 全部可用）。可读性优先，不做行为性重构。纯机械迁移 → 测试集合与计数**不得变化**（与 Step 4 的副本合并不同：本步是零计数变化）。
- [ ] **Step 6: 门禁与提交**（纯重构：测试全绿且**行为零变化**；测试绝对数略降只因副本合并——在提交信息里说明；linux.rs 拆分项零计数变化）
```bash
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-fs-fat crates/xd-fs-exfat crates/xd-fixtures
git commit -m "refactor(fs): fat read_file 拆分/两引擎测试内移(#\[path\])/fixtures 助手去重（纯重构）"
```

---

### Task 8: 端到端 —— daemon 全链路集成测试 + 环回设备扫描

**Files:**
- Modify: `crates/xd-daemon/Cargo.toml`（dev-deps +`xd-fixtures`、`tempfile`；serde_json 已有）
- Create: `crates/xd-daemon/tests/scan_ipc.rs`（全代码见下）
- Modify: `scripts/e2e-loop.sh`（真实环回设备补 scan 断言块）

> **T6 评审移交增补（spec-m1b-t6 + qual-m1b-t6 汇总，本任务必须覆盖）**：
> 1. **`XDG_STATE_HOME`/`--db` 注入**：scan_ipc.rs 的每个 daemon spawn 必须带 `--db <tempdir>`（或注入 XDG）——否则测试在真实 HOME 落库（T6 的 ipc.rs 已踩过此坑）。
> 2. **stderr 静默断言**：cancel 场景的 daemon 以 `stderr(Stdio::piped())` 捕获——断言无 `"panicked"` 字样（钉死 ScanCanceled hook 静默）；配套断言 cancel 后 `scan.finished state=canceled`（证明取消真发生而非没跑）。
> 3. **`--db` 降级存活**：daemon 以 `--db <指向目录>` 启动 → ping 正常 + stderr 含 `任务库打开失败` + 进程不退出。
> 4. **中断→failed 恢复**：kill daemon（SIGKILL）于扫描中 → 同 `--db` 重启 → `scan.status` 为 **failed**（recover_after_restart 护栏的 daemon 级钉死）；paused 任务则保留。
> 5. **EACCES 端到端**：`chmod 000` 的**常规文件** + `unix:` id → `-32001 Device permission denied`（CI 非 root 可测；T6 spec 已手工验过一次）。
> 6. `scan.progress` 发射分支（250ms 节流）目前无测试断言——大介质难入 CI，**裁定：以 `elapsedMs`/`readBytes` 语义断言替代**，发射分支不做 CI 钉死（真机手测清单记一笔即可）。
> 7. **XDG 优先级护栏（qual-t6 变异 9 无归属）**：起 daemon（不传 `--db`）**同时注入 `XDG_STATE_HOME` 与 `HOME`** → 断言库落在 XDG 路径而非 HOME（5 行，用既有 spawn harness）。不加则"XDG 优先"永无 CI 护栏。

- [ ] **Step 1: scan_ipc.rs（全代码；首行水印头）**

```rust
//! daemon 全链路：scan.start → 通知 → 分页/过滤 → 重启持久 → image: 拒绝。
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{RecvTimeoutError, Receiver, channel};
use std::time::{Duration, Instant};

fn spawn_daemon(
    image: &std::path::Path,
    db: &std::path::Path,
) -> (Child, ChildStdin, Receiver<serde_json::Value>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
        .arg("--image")
        .arg(image)
        .arg("--db")
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                let _ = tx.send(v);
            }
        }
    });
    (child, stdin, rx)
}

fn send(stdin: &mut ChildStdin, v: serde_json::Value) {
    writeln!(stdin, "{v}").unwrap();
    stdin.flush().unwrap();
}

fn read_response(
    rx: &Receiver<serde_json::Value>,
    id: i64,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(v) if v.get("id").and_then(|x| x.as_i64()) == Some(id) => return v,
            Ok(_) => continue, // 通知行
            Err(RecvTimeoutError::Timeout) => panic!("timeout waiting response id={id}"),
            Err(RecvTimeoutError::Disconnected) => panic!("daemon closed stdout"),
        }
    }
}

fn wait_notification(
    rx: &Receiver<serde_json::Value>,
    method: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(v) if v.get("method").and_then(|x| x.as_str()) == Some(method) => return v,
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout) => panic!("timeout waiting notification {method}"),
            Err(RecvTimeoutError::Disconnected) => panic!("daemon closed stdout"),
        }
    }
}

fn exfat_image_bytes() -> Vec<u8> {
    // 与 xd-core testutil::exfat_fixture 同构：3 条目（2 live + 1 删除）
    xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "LIVE_A.TXT", b"aaaa")
        .add_file("/", "LIVE_B.PNG", &[5u8; 100])
        .add_file("/", "DEL_ME.JPG", &[7u8; 9000])
        .delete("/", "DEL_ME.JPG")
        .build()
}

#[test]
fn scan_flow_and_restart_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("vol.img");
    std::fs::write(&img_path, exfat_image_bytes()).unwrap();
    let db = dir.path().join("tasks.db");

    let (mut child, mut stdin, rx) = spawn_daemon(&img_path, &db);
    send(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":1,"method":"device.list","params":null}));
    let dl = read_response(&rx, 1, Duration::from_secs(10));
    let dev_id = dl["result"]["devices"][0]["id"].as_str().unwrap().to_string();
    assert!(dev_id.starts_with("image:"));

    send(
        &mut stdin,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev_id,"mode":"quick"}}),
    );
    let st = read_response(&rx, 2, Duration::from_secs(10));
    assert_eq!(st["result"]["fs"], "exfat");
    let task_id = st["result"]["taskId"].as_i64().unwrap();

    let fin = wait_notification(&rx, "scan.finished", Duration::from_secs(30));
    assert_eq!(fin["params"]["taskId"], task_id);
    assert_eq!(fin["params"]["state"], "completed");
    assert_eq!(fin["params"]["foundCount"], 3);

    send(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":3,"method":"scan.results","params":{"taskId":task_id,"offset":0,"limit":2,"deletedOnly":false}}));
    let rs = read_response(&rx, 3, Duration::from_secs(10));
    assert_eq!(rs["result"]["total"], 3);
    assert_eq!(rs["result"]["entries"].as_array().unwrap().len(), 2, "分页 limit=2");
    send(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":4,"method":"scan.results","params":{"taskId":task_id,"offset":0,"limit":10,"deletedOnly":true}}));
    let rs2 = read_response(&rx, 4, Duration::from_secs(10));
    assert_eq!(rs2["result"]["total"], 1);
    assert_eq!(rs2["result"]["entries"][0]["name"], "DEL_ME.JPG", "exFAT 删除名一字不差");
    assert_eq!(rs2["result"]["entries"][0]["deleted"], true);

    drop(stdin); // stdin EOF → daemon 退出
    let _ = child.wait();

    // 重启：同一 --db → 结果可查、completed 不被 mark_interrupted 误伤
    let (mut child2, mut stdin2, rx2) = spawn_daemon(&img_path, &db);
    send(&mut stdin2, serde_json::json!({"jsonrpc":"2.0","id":5,"method":"scan.status","params":{"taskId":task_id}}));
    let s2 = read_response(&rx2, 5, Duration::from_secs(10));
    assert_eq!(s2["result"]["state"], "completed");
    send(&mut stdin2, serde_json::json!({"jsonrpc":"2.0","id":6,"method":"scan.results","params":{"taskId":task_id,"offset":0,"limit":10,"deletedOnly":false}}));
    let r2 = read_response(&rx2, 6, Duration::from_secs(10));
    assert_eq!(r2["result"]["total"], 3, "重启后结果可查（SQLite 持久）");
    drop(stdin2);
    let _ = child2.wait();
}

#[test]
fn scan_start_on_unregistered_image_id_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("vol.img");
    std::fs::write(&img, exfat_image_bytes()).unwrap();
    let db = dir.path().join("t.db");
    let (mut child, mut stdin, rx) = spawn_daemon(&img, &db);
    send(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":1,"method":"scan.start","params":{"device":"image:/etc/hostname"}}));
    let r = read_response(&rx, 1, Duration::from_secs(10));
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(r["error"]["message"], "Cannot open device: image:/etc/hostname");
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn pause_resume_over_ipc() {
    // 大图 + 立即 pause：条目边界驻停 → status 冻结 → resume 完成；再跑一任务 cancel
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("big.img");
    let mut b = xd_fixtures::ExfatImageBuilder::new();
    for i in 0..80u32 {
        b.add_file("/", &format!("F{i:04}.BIN"), &[3u8; 200]);
    }
    std::fs::write(&img_path, b.build()).unwrap();
    let db = dir.path().join("t.db");
    let (mut child, mut stdin, rx) = spawn_daemon(&img_path, &db);
    let dev_id = {
        send(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":1,"method":"device.list","params":null}));
        read_response(&rx, 1, Duration::from_secs(10))["result"]["devices"][0]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    send(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev_id}}));
    let task_id = read_response(&rx, 2, Duration::from_secs(10))["result"]["taskId"]
        .as_i64()
        .unwrap();
    // 小图可能秒完：pause 对终态返回 -32004 或对活动态 ok paused，两者皆合法——断言二选一
    send(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":3,"method":"scan.pause","params":{"taskId":task_id}}));
    let p = read_response(&rx, 3, Duration::from_secs(10));
    let code = p["error"]["code"].as_i64();
    assert!(p["result"]["state"] == "paused" || code == Some(-32004), "pause: {p}");
    if p["result"]["state"] == "paused" {
        send(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":4,"method":"scan.resume","params":{"taskId":task_id}}));
        let r = read_response(&rx, 4, Duration::from_secs(10));
        assert_eq!(r["result"]["state"], "scanning");
    }
    let fin = wait_notification(&rx, "scan.finished", Duration::from_secs(30));
    assert_eq!(fin["params"]["taskId"], task_id);
    assert_eq!(fin["params"]["foundCount"], 80);
    drop(stdin);
    let _ = child.wait();
}
```
（第三条测试接受 pause 竞态二态——IPC 层不强求确定性时序（确定性时序已由 T5 的 SlowDev 单测覆盖）；此测试钉"IPC 路径可走通且终态正确"。）

- [ ] **Step 2: `scripts/e2e-loop.sh` 增扫描断言块**：在既有 daemon--环回设备流程后（与脚本既有风格一致：`sudo`、`udevadm settle`、`set -euo pipefail`），若脚本已用 daemon 打开 loop 设备，则追加：
```bash
# M1b：环回设备上的真实扫描全链路（root；脚本既有 root 检查之后）
printf '%s\n' '{"jsonrpc":"2.0","id":20,"method":"scan.start","params":{"device":"unix:'"$LOOP"'","mode":"quick"}}' \
  | "$DAEMON_BIN" --db "$TMPDB" ... 交互流程按脚本现法 …
```
实施者按脚本实际结构改写（**若既有脚本以单条管道喂 daemon，须先重构为 mkfifo/coproc 或 heredoc 多行**——最省事：把 scan 三步（start→读取至 finished→results）写成 heredoc 多行一次性喂入，daemon 顺序处理并在 finished 后仍读 stdin，末尾 EOF 退出；断言 grep `"state":"completed"` 与至少一个删除文件名字）。CI（Linux 作业）已跑该脚本——无需新 workflow。

- [ ] **Step 3: 门禁与提交**
```bash
cargo test -p xd-daemon --locked
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
bash scripts/e2e-loop.sh   # 本机需 sudo；CI 由既有 job 覆盖
git add -A crates/xd-daemon scripts/e2e-loop.sh
git commit -m "test(daemon): 扫描全链路 IPC 集成（通知/分页/重启持久/image 拒绝/pause-resume）+ 环回设备扫描断言"
```

---

### Task 9: 出口验收（全量门禁 + 变异抽检 + 文档同步 + 合入 main）

**Files:**
- Modify: `docs/superpowers/plans/2026-10-02-xiaodun-m1b-contract-orchestration.md`（执行记录/偏差同步）
- Modify: 设计文档（裁定 (a) 补记——exFAT 引擎段落的删除语义）
- Modify: `README.md`（如引用契约版本，指向 proto/v1）

- [ ] **Step 1: 全量门禁（debug + release）**
```bash
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test --workspace --release --locked
bash scripts/e2e.sh
/home/erik/flutter/bin/flutter test --no-pub   # ui/
```
- [ ] **Step 2: 变异抽检（实施者手工做，kill 即通过；列出逐条结论进执行记录）**
1. `scan_task` 节流条件删 `found`/`read` 任一侧 → T5 通知测试须仍过（节流是性能非正确性——变体应存活，记录为"非承重"）；改成**永不节流**（每次条目都发）→ 应有测试可判别吗？**须承认此变异可能存活**——补一条假测试：单测 `Progress` 不可直接测（私有），改为在 e2e 断言 finished 前 progress 数量 ≤ 条目数（弱判别，记录诚实局限）。
2. `store.entries` 的 `ORDER BY idx` 删除 → 分页测试须 kill（stable 排序断言）。
3. `deleted_only` 过滤反转/忽略 → 过滤测试 kill。
4. `set_state_if_active` 的 `WHERE state IN …` 删 → 终态护栏测试 kill。
5. `checkpoint` 的 paused 检查删 → 暂停冻结测试 kill。
6. 取消 downcast 判断删（一律 failed）→ cancel 测试 kill。
7. `read_file` 早分支的 `!entry.contiguous` 删 → 探针 B/C kill。
8. `grade_deleted` 链长检查删 → `deleted_short_stale_chain_never_complete` kill。
9. 迁移映射（quality 字符串互换 complete/maybeDamaged）→ handler/e2e 断言 kill。
10. `limit` 钳位删 → invalid params 测试 kill。
- [ ] **Step 3: 水印与溯源**：`bash scripts/apply-copyright.sh` 覆盖新文件（幂等复跑一次确认）；provenance 重生成与签名归**发版时**（用户触发发布流程）——本次合并记录里注明待发版。
- [ ] **Step 4: 文档同步**：设计文档 exFAT 段落补裁定 (a) 一行（"删除项非连续只沿 stale 链，绝无连续猜读；碎裂场景归雕刻"）；计划文件写入执行记录块（每任务 commit SHA、fix rounds、偏差——按 M1a2/M1e 计划既有体例）；README 契约指针更新（如有）。
- [ ] **Step 5: 合入与 CI**：milestone 分支 `m1b-contract-orchestration` 全任务完成后合 main（fast-forward 或 merge commit 按既有习惯），push，`gh run view <id> --json jobs` **逐 job 验证**（勿只看结论行；此前教训：`watch --exit-status | tail` 会吞退出码）；CI 全绿后本切片关闭。

---

## 执行记录：T9 出口验收（lead 执行）

**Step 1 全量门禁（本地）**：`cargo fmt --check` ✓ / `clippy -D warnings` ✓ / **debug 291 passed / 0 failed** / **release 291 passed / 0 failed**（release 档是 M1a2 抓出过 release-only 缺陷的必跑档）/ `scripts/e2e.sh` **E2E OK**（真机枚举可见 `/dev/sda|sdb` 且 `transport:"sata"`——T1 成果的活证据）/ flutter `+19 ~1` + analyze 干净 + dart format 0 changed。

**Step 2 变异抽检（跨层 kill 链）**：把 `scan_worker.rs` 的 `"scan.finished"` 通知名改成 `"scan.progress"` → `cargo test -p xd-daemon` **恰 3 枚集成测试红**（cancel_mid_scan / scan_flow_and_restart_persistence / pause_resume_over_ipc）→ 还原（cmp 逐字节）。各任务 qual 累计变异：T1 11 + T2 14 + T3 8 + T4 20+ + T5 13 + T6 9 + T7 5 ≈ **80 个变异**，等价/护栏类均逐条有据。

**Step 3 水印与溯源**：`apply-copyright.sh` 幂等（T7 复跑 stamped 0）；provenance.sha256 重生成与签名归发版时（用户触发的发布流程）——本合并记录注明待发版。

**Step 4 文档同步**：README 项目状态节（M1b 合入如实描述 + 剩余切片）与 daemon CLI（`--db`/scan RPC）；`docs/security/linux-privilege-model.md` §5 手测清单追加「真机扫描全链路长跑」条目（进度节奏/暂停读停/取消静默/XDG 库/真百分比）。

**Step 5 合入与 CI**：`--no-ff` merge 至 main（`090cde5`，65 文件 +8429/−1632；含 v0.2.0 以来全部 M1b 工作与三份后续计划）；push main → workflow_dispatch →（CI 结果见下）

---

## 验收定义（M1b Done 的判据）

1. `proto/v1/` 20 个 golden（19 + device.list 对）双侧断言（Rust contract_v1 + handlers 往返 + Dart），`PROTOCOL_VERSION = 1`，v0 golden 除 method-not-found 文案外不动。
2. 两个真实引擎经 daemon 全链路可扫：`scan.start` 返回 fs/taskId/totalBytes；`scan.progress/finished` 通知按时序出现；`scan.results` 分页/过滤正确；`pause/resume/cancel` 语义按 §T5 状态机。
3. SQLite 持久：重启 daemon 后结果可查；中途死掉的 scanning → failed；paused 保留且 resume 可重跑。
4. 崩溃隔离：worker panic → 任务 failed、daemon 存活、后续任务正常（单测钉死）。
5. (a) 裁定落码且探针 B/C 钉死；`read_file`/`grade_deleted` 同源语义。
6. 懒打开唯一出口 + `image:` 拒绝（提权安全）；EACCES → -32001 契约码。
7. 全量门禁（debug+release+clippy+fmt+flutter+e2e.sh+e2e-loop.sh 含扫描）绿。

---

## 执行记录

### T1（契约 v1）—— impl-m1b-t1。提交沿革：`d67a858`（首版）→ `3fa947f`（终版，含裁定落地与发现⑤）→ `f5ec631`（水印头修复）；事故详见第 8 条。DONE_WITH_CONCERNS → spec 评审 **PASS**

门禁：229 passed（+12）/ clippy clean / fmt clean / flutter +18~1（新 9，skip=无 XD_DAEMON_BIN 的既有 ipc_integration）/ analyze clean / release 档 exit 0。golden 21 个用 awk 从计划提取后 `diff -r` 逐字节比对（0 差异）。

实施者发现 5 条（含 2 条计划自身缺陷），lead 裁定：

1. **`xd-daemon/tests/ipc.rs`（:80）protocol 0→1**（计划排在 T5 且行号写 :79，与 T1 全绿门禁冲突）——**批准 T1 内改**；机械波及同 Step 5.5。T5 Step 7 Rust 部分就此完成。
2. **`xd-core/tests/contract.rs`（:50）**（v0 ping 用例用活常量构造期望值；Step 5.5 与 T5 Step 7 的 grep 都漏）——**批准**：期望值改封存字面量 0 + 注释（v0 是历史快照；活契约往返由 contract_v1.rs/handlers v1 用例承担），保住「当前类型仍能 decode/re-encode v0 文件」的兼容路径。
3. **T5 Step 1 重复定义三个错误构造器**（计划文本自冲突）——已删 T5 处代码块（六个全在 T1）。
4. **文件结构注误把 ScanStartParams 归 T1**——已改注（params 归 T5，T1 未提前添加，正确）。
5. **`linux.rs::open_with_sysfs` 是第 2 个 DeviceInfo 构造点**（计划计数错；`--device` 打开行 `transport: None`）——接受的后果：先开行在 device.list first-wins 去重时遮蔽枚举行的 transport。**裁定：升级路径（canonicalize sysfs → classify_transport，~3 行）延后到 M1d**（UI 真正显示 transport 时才有一致性诉求），记入 M1d 前置清单。

**T1 终版补记（`3fa947f`，34 文件，Rust 230 / flutter +19 全过 0 skip）**：

6. **contingency 被触发**：CI（ci.yml:70-71）设了 `XD_DAEMON_BIN`，`ui/test/ipc_integration_test.dart:34` 会真起 daemon 打红 → 按预先授权改为 1（仅此一处；`protocol.dart` 与其它断言仍归 T5）。**里程碑门禁清单补 `dart format --check`**（ci.yml:66 既有闸，此前不在清单）。
7. **发现⑤提前关闭**：`open_with_sysfs` 的 transport 经 `sysfs_transport()`（canonicalize class/block/<name> → classify_transport）与 device.list 同源；测试 `sysfs_transport_classifies_and_none_when_missing`（确定性假 sysfs 根）。M1d 计划对应前置项撤销（已回改 M1d 计划注释：遗留的是"复核"而非"实现"）。
8. **git 事故（已恢复，如实记录）**：impl 的 amend 与 lead 的 docs 提交竞态——第一次 amend 卷入 lead 已 staged 的 4 份计划文档（悬空 d39dbef），修复后再次 amend 时又把 lead 已提交的 feb43d4（docs）`reset --soft` 撤回（内容零丢失，回工作树）。恢复：在终版 3fa947f 上重建 docs 提交（bd8f54b）+ `push --force-with-lease` 对齐远端（远端曾含悬空链，全部为自家提交、内容有本地副本）。**管线纪律更新（已写入团队管线记忆）**：代理永不 push、永不 amend/rebase/reset（修复轮一律追加提交）；lead 是唯一 push 者；lead 发"授权后续修改"消息前须确认自己不再动 git（本次竞态根因）。
9. **lead 对齐抽查（T2 前）**：T1 落地后的 `api.rs` 里 `RpcError::invalid_params(&str)` 是 M0 原形（带参、输出 `Invalid params: {message}` 前缀），T5 计划原稿有 4 处无参调用会编译失败——计划已同步为带参调用（`"missing or malformed params"` / `"unsupported mode"` / `"limit out of range 1..=1000"`）；-32602 无 golden、文案前缀随各调用点，v1 README 不钉死文案 ✓。

**spec 评审（spec-m1b-t1）——结论 PASS**（对 `3fa947f`；悬空 `d67a858` 仅作对照快照）。关键证据：21 golden 从计划 heredoc 独立重提取三方逐字节全等（并验证 heredoc 自 `837f7f8` 未变）；独立探针 `/tmp/di-probe` 18/18（六构造器文案逐字、transport None 无键/Some 有键、ScanEntry 无 byteOffset、通知信封）；门禁亲跑 230 passed/0 failed/0 ignored、CI 等价 `XD_DAEMON_BIN` 下 flutter +19 全过 0 skip；5 条发现复核全部成立、无报告出入。非阻断发现及处置：

- **A（已修，`f5ec631`）**：`notify.rs:1` 字面 `// WATERMARK` 占位（全仓唯一无水印头 .rs；`apply-copyright.sh` prepend 式不会清它）→ 按「head -1 现文件」逐字节置换真水印头 + `cmp` 验证 + 零行为变化复跑。**纪律遵守：追加提交、未 amend、未 push。**
- **B（本次订正）**：记录标题 SHA 与行号漂移（`:47→:50`、`:79→:80`、Dart `:34→:36`）已在上文修正。
- provenance 对 api.rs 已过期——按 Task 9 Step 3 约定归发版时重签，非 T1 门禁（仅记录）。

**qual 评审（qual-m1b-t1）——结论 ISSUES（非阻塞）→ 补丁 `a563cb1` → 复审增量 APPROVED（T1 关闭，232/0，树净）**。变异表：10 条 9 KILL + 1 等价变异体（#6 `#[serde(default)]` 对 `Option` 冗余——serde 隐式缺失=None，保留作自文档）；加分变异 **11 存活 = 真缺口**（`enumerate` 的 transport 赋值零覆盖）。三个探针结论：ping 归一行删掉仍全绿（独立价值仅自洽，净覆盖由 `ping_returns_pong_and_protocol` 兜底）；三态无关歧义（missing/null→None、编码一律省略键，单向 daemon→UI + README 声明充分）；notify.rs 水印首行与 api.rs 逐字节相同。补丁（`a563cb1`，4 文件，232 测试）：
- (b) `enumerate_classifies_transport_from_sysfs`（假 sysfs 根含 `/usb` → list() 断言 `Some("usb")`）；实施者手工复验变异 11 → 该测红 → 还原。
- (a) `scan_state_all_variants_serialize_to_contract_literals`（pending/failed 字面量补齐）+ 订正 `contract_v1.rs` 假注释「全量覆盖」。
- (d) Dart 侧 golden 集合钉死（21 名单全等）。
- (c) README 补 -32602 文案说明（`Invalid params: <reason>` 中 reason 不属契约；`Cannot open device: <id>` 固定文案）——**其逐字测试归 T5**（无 producer 时不可钉；已加入 T5 测试清单）。
- 结构欠账（预存）：`linux.rs` 585 行超 500 行规则（T1 +51）——已加入 **T7** 拆分项。

### T2（xd-core::store）—— impl-m1b-t2。提交沿革：`89ad76e`（首版）→ `3a920ef`（spec 缺口补测）→ `399b36e`（qual 缺口补测 + 两注释）→ `a597db4`（注释措辞修正）。DONE → spec **PASS** → qual **APPROVED**（T2 关闭，244/0）

- **计划偏差（仅格式）**：任务正文非 rustfmt-clean（超长签名/`params!` 超 100 列/单行 let-else）与 `cargo fmt --all --check` 门禁冲突 → `cargo fmt --all`（仅 store.rs）。spec 独立复核：token 级比对 7 处非空白差异全为 rustfmt 产物；**68/68 字符串常量（含全部 SQL 文本）字节级相同**——声明成立。
- **spec 发现**：`INSERT OR REPLACE` 同键替换行为无测试（计划指定、行为正确）→ 补 `reinsert_same_key_replaces_row`（实施者并做 INSERT→非 REPLACE 有牙自证）。附加审计：schema 内省与计划列集精确相等（**无 byte_offset/scan_mode 等未来列** ✓）；外部写入未知 state → `task()` 降级 Failed 不 panic（探针验证）。
- **qual 变异表（11+3+4 条，收官口径）**：8 KILL；`ORDER BY idx` 删除 **≈等价变异体**（EXPLAIN：PK 索引天然 (task_id,idx) 序；保留作契约保证）；对称交换 `Canceled↔Completed` **预期 NO-KILL**（库内往返自洽；wire 值由 T5 pin）；`u64::MAX` 等极值 round-trip 逐位全等 ✅。真缺口 3 个（set_progress 零覆盖 / 未知态降级无测 / 跨任务隔离弱）→ 全部补测并以定向变异（`?1=?1`、列交换、`unwrap_or` 互换）四条独立复杀闭合。
- **质量裁定**：`Mutex` 中毒保持 `unwrap`（fail-stop；无用户代码临界区）——注释措辞「监督重启」经 qual 指出无据后改为「任务态由 `mark_interrupted`/`recover_after_restart` 兜底」（`a597db4`）；SQL 注入面=0（`format!` 仅插值编译期常量 `filter`，其余全参数绑定）。
- **前向残项（记录，不处理）**：`'pending'` 目前无 DB 生产路径（M1c 引入时随测试补）；DB 状态字面量无独立测试（wire 归 T5、字面量由 M1c 迁移测试覆盖——裁定确认无新增风险）。

### T3（两引擎 `scan_with_observer`）—— impl-m1b-t3。提交沿革：`604ba92`（主）→ `c39f746`（卷标 1:1 强化）。DONE → spec **PASS** → qual **APPROVED**（T3 关闭，249/0）

- **插入点（判别核心）**：exfat `scan.rs:184-185`、fat `scan.rs:202-203`——均位于目录递归 if 块之后、`out[pushed].quality` 降级写回之后、循环体末尾（后序 + 终值）。spec 独立探针：回调数==表长且多重集相等；三层 exfat 序列 `DEEP.TXT→B→MID.TXT→A→ROOT.TXT`；两引擎降级目录回调即 `MaybeDamaged`。
- **偏差（全机械）**：fat 实际 API `fat16()/add_subdir`；8 参函数补 `#[allow(clippy::too_many_arguments)]`（-D warnings 必需，spec 已用合成函数复现 8/7 错误）；exfat 三个既有测试调用点补 `&mut |_| {}`。
- **qual 变异表（8 条）**：前序化/透传断链/降级前回调 **双引擎全 KILL**；**重复回调 6a/6b KILL**（精确序列断言已覆盖"每条目恰一次"——T5 `found_count` 虚增风险在引擎层已守）；`scan()` 内联 = 等价变异（观察者 noop 不可观测）；fat 卷标项不进回调流 = 语义差但裁定可接受 → **已补 `observer_stream_matches_table_with_volume_label`**（1:1 计数，真实盘必走路径；质询者亲验双态：干净绿/变异红——该测为 qual 自验代码，落码后免复审增量，理由记录于此）。
- **质量裁定**：`&mut dyn FnMut(&Entry)` 保留（收益=公共签名不泄类型参数；探问词"递归单态化"论据不成立，qual 纠正，源码无此措辞）；fat 8 参不抽 context struct（exfat 9 参先例；参数全为穿透借用）；头注"后序/终值"与实现逐条一致且对 T5 承重。
- **T5 纵深防御（已入计划）**：集成测试断言 `results` 的 idx 集合 == `0..found_count`。

### T4（(a) 裁定 + 三道诚实性界卫）—— impl-m1b-t4。提交沿革：`15d8481`（(a) 落码）→ `f680e93`（可达界卫）→ `d100766`（deleted 前缀回访）→ `6a0be97`（live 回访截断）→ `7d43f37`（G1 判别测试 + 性能界 need + 文档 + G2/G3）→ `4e0ddc8`（G2/G3 判别力补强）。DONE_WITH_CONCERNS → spec **PASS** → qual ISSUES → 补丁 → 增量复审 → 定向补强（T4 关闭，262/0；workspace 249 → 262，+13 测试）

**裁定链（本任务的核心产出，全部有 mutant 实证支撑）**：
1. **(a) 本体**：deleted+!contiguous 只沿 stale 链（探针 B/C：链被清→4096 诚实前缀；链簇被占→截断）。**spec 三维对照证明判据价值**：pre-T4 对探针 B 交付 9000B **错位数据**且评 Complete；mid（(a) 但无界卫）在泛化构型交付 12288B=同一簇×3 且评 Complete。
2. **可达界卫**（impl 自抓）：`need > max_cluster - fc + 1` → 物理不可能（fc 近堆尾 + DL 污染 + 环链可达 254 长）。qual 首轮发现**现有 loop 测试被回访检测双重兜住、界卫无判别输入**（真缺口）→ G1 用**非回访链**（253→6→7）补测，read/scan 各一，独立复杀"恰两红且环测仍绿"。
3. **链前缀回访检测**（spec 抓 + lead 裁定）：环/回折 → 交付重复簇字节（伪造序）。裁定用**前缀去重**而非 `len>reachable`（后者误伤 need=1 合法单簇交付）；deleted → 空（整链不可信证据），live → 截断至首回访点（FAT 权威、逐跳可信至首次矛盾）——**刻意不对称**，四处文档一致（证据权威论措辞，`grep 实指`=0）。
4. **live 车道同病收口**（impl 自抓）：live 自环此前同样重复交付 → 回访截断；quality 维持 entry 层 checksum 语义（scan.rs 分级梯已注释；M1c 若升级链感知分级需注意 I/O 放大）。
5. **性能**（qual 抓）：live 回访扫描原为**全链 O(n²)**（1M 簇链 ≈5×10¹¹ 比较）→ 界 `min(need, len)`，qual 穷举 32,800 (chain,need) 对证明逐点等价。

**变异实证汇总**：spec+qual 合计 KILL ≈ 20 个变异（含早分支回退连续、位图门控、live/deleted 车道混淆、链不足 Complete 等），等价变异 1（scan 连续臂界卫仅短路——`is_free` Err 已兜）、1 处判别力归属修正（G2 末簇→三簇中段；G3 设备尾巧合→堆中段，均以 qual 预定义 M1/M2 复刻双红收口）。**既有 15 个删除/分级测试逐函数体比对零改动、行为零变化**（spec 独立验证）。

### T5（scan_task 编排 + 路由 + 协议号收尾）—— impl-m1b-t5。提交沿革：`f2bf297`（主）→ `c1c6cf2`（FAT 冒烟+窄窗注记）→ `00a84eb`（spec 收尾：restart 护栏/README 三处/模块头）→ `ce56d21`（qual 收尾：两缺口补测/线程命名/残留）。DONE → spec **PASS**（flake 0/40）→ qual ISSUES → 补丁 → 定点自证（T5 关闭，283/0；workspace 262 → 283）

**产物**：`scan_task.rs`（公开 API/状态机，285 行）+ `scan_worker.rs`（worker 内部，183 行）+ `testutil.rs`；handlers 六路由 + CoreCtx Arc 化；Dart fake 收尾。

**计划缺陷（实施者实修，均编译/门禁所迫）**：计划测试代码两处 `let Response::Err(..) = …;` 缺 `else`（E0005）；clippy 两处（`type_complexity` → `ActiveHandle`；`new_ret_no_self` → `SlowDev::wrap`）；golden 往返两处对不上（真 ImageFileDevice 的 id 含绝对路径 → info 桩；`total:42` 需 40 条填充）；cancel 测试原设计会 flake（cancel 同步置态 vs finished 异步）→ 改轮询事件本体；daemon `main.rs` Arc 最小迁移被迫前置（workspace 编译要求）；v0 golden `error_method_not_found` message 修正（scan.start 转正）——v0 封存声明的唯一例外（README 已注）。

**裁定**：FAT 臂零覆盖（实施者自发现）→ 补 `fat_path_smoke_run_worker_and_fat_to_entry_mapping`（映射全字段）；cancel↔resume 窄窗 → **文档化不修**（地面真相一致、不可确定性测试；注记抽至 `Ctrl` doc 覆盖 pause/cancel/resume 三分支）；`restart` 双 worker 脚枪 → 加护栏 + 有牙测试（拒绝无副作用：resume 仍走完）；节流不 pin（无可判别观测手段）；worker 线程命名 `scan-{id}`（stderr 归因）。

**qual 变异表（13 条）**：8 KILL（cancel 检查/paused 驻停/idx 偏移/错误码互换/limit 界/spawn 登记/downcast/restart 护栏）；2 真缺口当场补测并定点自证（`mode` 校验、"已打开优先"）；等价 1（null params）；无判别 2（节流=性能非正确性；worker 终局条件写=竞态护栏类，与窄窗同族可接受）。**flake 压力 0/40**（串行 20 + 并发 20 进程次）。

**前向注记**：`tasks` map 持有 `Arc<dyn BlockDevice>` 至 daemon 退出（M1c 续跑复用；USB 安全弹出前的句柄策略归 M1d/M2）；`mode:"deep"` 现拒绝、M1c 转正时其断言改合法路径；`elapsedMs`=墙钟（README 已改）。

### T6（daemon 并发接线）—— impl-m1b-t6，提交 `af472ff`。DONE → spec **PASS** → qual **APPROVED**（T6 关闭，283/0）

- **计划-现实修正（1 项，最关键）**：`Arc<Mutex<StdoutLock<'static>>>` 在 rustc 1.99 **不可编译**（`StdoutLock` 含 `ReentrantLockGuard` ⇒ `!Send`；spec 独立复现 E0277，正对照 `Mutex<Stdout>` 通过）→ 实现为 `Arc<Mutex<Stdout>>`（同一把锁串行化整行 + flush）。**计划文本已改**（本任务头注）。
- **必要性偏差（1 项）**：ipc.rs 注入 `XDG_STATE_HOME=<per-test tempdir>`——否则 daemon 测试在真实 `~/.local/state/xiaodun/tasks.db` 建库（隔离 HOME 复跑验证：目录全空；真实 HOME 零污染）。
- **spec 证据（节选）**：hook 装点早于一切 worker spawn（唯一 spawn 点只经 serve 到达）；50 并发扫描 + 651 响应 stdout 压测 **751 行逐行 JSON 零坏行**；`image:` 拒绝以 **FIFO + /proc/pid/fd 双证零触碰**；**真 EACCES 端到端**（chmod 000 + unix: → -32001）当场验证；--db 四态 + 旧库续用（scanning→failed、paused 保留）。
- **qual 变异表（9 条）**：套件 10 轮全绿；**7 条探针杀**（hook 反转/`--db` exit/image 放行/recover 删除/EACCES 降级/HOME 优先反转/…）；等价 1（hook 装载点前移——无 worker 能先于 serve）；护栏 1（XDG 注入，套件不可红，如实记录）。6 条套件零覆盖 → 5 条已入 T8 增补清单、**XDG 优先级入 T8 item 7**。
- **性能账（域外观察，转 M1c）**：`insert_entries` 每条目一事务 + `synchronous=FULL` ≈ **6.9ms/条目 fsync**（513 条目 ext4 3539ms vs tmpfs 59ms）；M1c 大扫描前应评估批量提交或 WAL+synchronous=NORMAL（与"崩溃保部分结果"语义权衡）——已写入 M1c 计划。
- **接受性 nit（记录不修）**：`image:`/未知 scheme 拒绝分支无 stderr 留痕（另一分支有）；「真 panic 仍打印」在 daemon 无可达 panic 路径，T8 只钉可观测半边（cancel 静默）。

### T7（结构欠账纯重构）—— impl-m1b-t7。提交沿革：`e90b41b`（主：fat 拆分/测试内移/助手去重/linux 拆分）→ `c33e606`（收尾：fat 测试内移/线宽注/校验和对拍互锁）。DONE → spec **PASS** → qual **APPROVED**（T7 关闭，283/0 精确守恒）

- **三位一体零变化证据**：函数体**逐字节相同**（fat read_file 含 doc；linux 迁出条目除 4 处 `pub(crate)`〔sysfs_transport/transport_str/compose_disk_name/is_listable_name，仅白盒测试与父模块所需〕无任何可见性变化）；公开面 25 条路径对快照逐条编译状态一致（唯二差异=计划授权的 fixtures +2 导出，dev-dep-only）；测试名集合双向差集为空（283 条）。**qual 差分字节探针**（最强证据）：14 组夹具（fat12/16/32 存活/删除/复用/污染/截断 + exfat 链式/VDL/自环/污染）在重构前快照与现树两侧跑 scan+read 全输出（含内容 sha256）**逐字节相同**（65 行同 sha256），负控变异立即显差——非空转。
- **校验和对拍互锁（c33e606 新增资产）**：生产 `dirent.rs entry_set_checksum16` ↔ `xd_fixtures::entry_set_checksum` 在既有测试内对拍（3 样本含 128B 真实集），spec/qual 各自独立双向变异均红；qual M5 结论：**互锁须留真实集样本**（1 字节样本对移位漂移不可见），语料本身另有端到端兜底。
- **线宽规则澄清（写入本计划口径）**：500 行约束针对**生产源文件**；`*_tests.rs` 测试体集中不受约束（刻意的源文件线宽控制另一半，exfat 两测试文件头注已写明）。最终行数：exfat scan 299/read 235、fat scan 242/read 222、linux 420/enumerate 205。
- **导入解析漂移审计**：全仓无 glob 重导出；`xd_fixtures::refix_deleted_checksum` 仅 dev-dep（`cargo build --workspace` 绿、构建图零 fixtures）；无同名不同项。
- **测试路径改名**：9 条 fat read 测试 `scan::tests::*` → `read::tests::*`（实现真迁移，1:1）——全仓 grep 文档/CI/README 零外部引用 ✓。
- 四份新文件（fat read.rs/scan_tests.rs、exfat scan_tests/read_tests、linux/enumerate.rs）真水印头 cmp 通过；`apply-copyright.sh` 幂等 stamped 0。

### T8（e2e 集成 + 环回设备）—— impl-m1b-t8。提交沿革：`efb9803`（主：scan_ipc.rs 8 测 + e2e-loop scan 块）→ `b7af460`（e2e-loop 步骤 2 补 --db）→ `28332e6`（EXIT trap 兜底）。DONE → spec **FAIL（1 阻断）** → 修复 → 条件 PASS（T8 关闭，291/0）

- **8 枚测试全员落地**（计划 3 + 增补 2/3/4/5/7；增补 1 由统一 spawn 底盘保证）：全链分页/过滤/重启持久（含 **idx 集合纵深防御**）、image 拒绝、pause 二态、**cancel 严格 canceled + stderr 无 panicked**、--db 降级存活、**SIGKILL 中断→failed + paused 保留**、EACCES 端到端 -32001、XDG 优先。
- **慢扫镜像**：FAT16 根区 511 条 0xE5 删除项（513 条目）；spec 量化：ext4 ≈3.9-4.0s vs tmpfs ≈56-61ms；**cancel 余量 ≈55ms（测试实际只耗 2-10ms）**、tmpfs 下严格 cancel 20/20 + 全套件 10/10——**不 flake，维持严格断言**（二态/加条目/换盘三方案均不值）。sigkill 守卫诊断性非承重（晚杀必红反证）。EACCES 测本机 uid=1000 真跑（未 skip）。
- **spec 阻断项（已修）**：e2e-loop EXIT trap 对 root 属主 `$tmpdb` 以用户身份 `rm` EPERM → `set -e` 下**断言全过仍 exit 1**（docker 复现链条；CI 会红、挡 T9）→ `28332e6`：`sudo rm … || true` + 其余清理同款兜底；docker 前后对照 EXIT 1→0 实证。**同一脚本第二处污染（步骤 2 无 --db）由实施者自抓并修（`b7af460`）**——现两个 spawn 点均带 --db。
- **裁定**：不采纳"先普通 rm 再 sudo"叠层（`sudo -n true` 门在先，无 sudo 时 trap 未设置——现形态对所有可达路径正确，记录备查）。
- **T9 移交**：真机手测清单已定位（`docs/security/linux-privilege-model.md` §5，:81-92）——T9 在末条后追加「真机扫描/取消/进度条」一条。

---

© 2026 erik · https://erik.xyz · erik@erik.xyz
