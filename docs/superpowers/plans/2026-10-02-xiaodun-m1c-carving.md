# 小盾 M1c：文件雕刻（carving v1：JPEG/PNG）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增 `xd-carving`：在**未分配空间**上做签名雕刻——JPEG（marker 走链 + 熵段 EOI 重组）与 PNG（chunk 走链 + IEND + CRC 验证），产出无文件名结果与诚实完整度；深扫任务复用 M1b 的任务框架，带**真百分比进度**与**断点续跑检查点**（设计 §4.3/§4.4 约束 3、5 的真实兑现）。契约 v1.1（`mode:"deep"`、`quality:"carved"`、`ScanEntry.byteOffset`）。

**Architecture:** 雕刻引擎与 FS 解耦：输入 = 未分配**字节区间集**（exFAT 由位图产出、FAT 由 FAT 表产出）；顺序分块读 + 签名匹配 + **边界验证**（JPEG marker 结构走链 / PNG chunk 走链含 CRC）+ 重组；结果经 M1b 的 store 流式落盘、经 scan_task 的通知/暂停/取消通道推送。

**明确裁定（偏差记录）：**
1. **v1 顺序扫描，不做多线程并行块扫描**（设计 §4.3 的"多线程"项延后）：雕刻是 I/O 受限（顺序读占满设备带宽即达 §4.6 的 70% 底线），并行匹配只在 decode 受限时才有收益——留待 M2 实测后按需加，届时是"并行读多个 run"而非"并行签名匹配"。
2. **仅 JPEG/PNG**（MP4/PDF/OFFICE 归 M2，按路线图）。
3. **品质档新增 `carved`**（设计 §4.2「完整/可能损坏/仅雕刻」的第三档）；`byteOffset` 是雕刻条目的读取坐标（`firstCluster` 对雕刻无意义，恒 0）。
4. 未识别 FS 的设备深扫仍 `-32002`（无 FS 裸盘雕刻归专业版方向，不排期）。
5. 雕刻条目的**交付长度 = 重组到的实际字节数**（有 EOI/IEND = 结构完整；无 = 诚实截断前缀），`estimatedComplete: bool` 不在本版契约（UI 用 quality + size 表达；M1d 文案"可能不完整"的判据 = carved 恒如此表述）。

**前置：** M1b 已合入（契约 v1、scan_task/store、观察者、引擎 (a) 裁定）。本计划在 `m1c-carving` 分支上执行。

---

## 文件结构

```
crates/
├── xd-carving/                    # 新增 crate
│   ├── src/lib.rs                 # 公共面：carve_runs(dev, runs, opts, observer) -> CarveStats
│   ├── src/crc32.rs               # IEEE CRC32（PNG chunk 验证；无新依赖）
│   ├── src/signatures.rs          # JPEG/PNG 签名 + 头部边界验证
│   ├── src/jpeg.rs                # marker 走链 + 熵段 EOI 重组
│   ├── src/png.rs                 # chunk 走链 + IEND + CRC
│   ├── src/carver.rs              # 块扫描器（重叠窗口/坏读跳过/run 边界/进度）
│   └── tests/carve_e2e.rs         # 合成镜像端到端 + 恢复率回归门禁
├── xd-fs-exfat/src/freespace.rs   # unallocated_runs（位图驱动）
├── xd-fs-fat/src/freespace.rs     # unallocated_runs（FAT 驱动）
├── xd-fixtures/assets/tiny.jpg    # 316B 真实 JPEG（1×1）
├── xd-fixtures/assets/tiny.png    # 68B 真实 PNG（1×1）
├── xd-fixtures/src/carving.rs     # plant_in_free_cluster 等夹具助手
├── xd-core/src/{store.rs,scan_task.rs,handlers.rs}   # schema v2 / CarveWorker / deep 路由
└── proto/v1/                      # README amend + scan_results_carved golden
```

---

### Task 1: 契约 v1.1（mode/quality/byteOffset/totalBytes 语义）+ 结果落库字段

**Files:**
- Modify: `proto/v1/README.md`（amend 段，非新版本目录——纯增量：读旧客户端不受影响）
- Create: `proto/v1/examples/scan_results_carved.response.json`
- Modify: `crates/xd-core/src/api.rs`（`ScanEntry.byte_offset`）
- Modify: `crates/xd-core/src/store.rs`（schema v2 迁移 + 读写 + 测试助手补字段）
- Modify: `crates/xd-core/tests/contract_v1.rs`、`crates/xd-core/src/handlers.rs`（carved golden 往返）

- [ ] **Step 1: README amend 段**（追加"v1.1 增量"小节）：`mode` 值域 `"quick"（缺省）| "deep"`（deep 自 M1c 起有效）；`quality` 值域增 `"carved"`（雕刻件：结构重组成功度见 size 与重建语义）；`ScanEntry.byteOffset`：u64，**缺省=null**（FS 条目恒缺；雕刻条目为未分配空间内的起始字节坐标，`firstCluster` 恒 0）；`totalBytes` 语义=**本任务目标扫描字节数**（quick=设备大小；deep=未分配空间总字节）。golden 新增 `scan_results_carved.response.json`（既有 21 个不动——`byteOffset` 靠 `skip_serializing_if` 缺省省略，quality 值域扩展不改旧值）。

- [ ] **Step 2: golden**
```bash
cat > proto/v1/examples/scan_results_carved.response.json <<'EOF'
{"jsonrpc":"2.0","id":11,"result":{"total":2,"entries":[{"idx":0,"name":"","path":"","ext":"jpg","sizeBytes":24410,"deleted":true,"isDir":false,"quality":"carved","firstCluster":0,"byteOffset":835584},{"idx":1,"name":"","path":"","ext":"png","sizeBytes":4210,"deleted":true,"isDir":false,"quality":"carved","firstCluster":0,"byteOffset":892928}]}}
EOF
```

- [ ] **Step 3: api.rs —— `ScanEntry` 加字段（其他不变）**
```rust
    /// 雕刻条目在未分配空间内的起始字节坐标；FS 条目恒 None（序列化省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_offset: Option<u64>,
```

- [ ] **Step 4: store.rs —— schema v2 迁移 + 读写**
`init()` 改为：建表语句的 entries 表**加列** `byte_offset INTEGER`（可空）；随后迁移既库：
```rust
        // v1 → v2 迁移（§M1c）：entries.byte_offset。user_version 闸门 + 列探测保证幂等。
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
```
（实现细节：`init()` 内先 `let conn = self.conn.lock().unwrap();` 复用连接；建表语句一并更新。）`insert_entries`/`entries` 的 SQL 增列（写入 `e.byte_offset.map(|v| v as i64)`；读取 `r.get::<_, Option<i64>>(9)?.map(|v| v as u64)`）。**store.rs 测试助手 `entry()` 补 `byte_offset: None`**；新增测试：
```rust
    #[test]
    fn byte_offset_roundtrips_and_defaults_null() {
        let s = Store::open_memory().unwrap();
        let id = s.create_task("d", "exfat", 1).unwrap();
        let mut carved = entry(0, "", true);
        carved.quality = "carved".into();
        carved.byte_offset = Some(835584);
        s.insert_entries(id, &[carved.clone(), entry(1, "A.TXT", false)]).unwrap();
        let (_, page) = s.entries(id, 0, 10, false).unwrap();
        assert_eq!(page[0].byte_offset, Some(835584));
        assert_eq!(page[1].byte_offset, None);
    }

    #[test]
    fn v1_database_migrates_to_v2() {
        // 手工造 v1 库（无 byte_offset 列、user_version=1）→ Store::open 迁移后可读写
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
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let id = s.create_task("d", "exfat", 1).unwrap();
        let mut e = entry(0, "OLD.JPG", true);
        e.byte_offset = Some(4096);
        s.insert_entries(id, &[e]).unwrap();
        assert_eq!(
            s.entries(id, 0, 10, false).unwrap().1[0].byte_offset,
            Some(4096)
        );
    }
```

- [ ] **Step 5: 契约双侧断言**
`contract_v1.rs`：carved golden envelope 解码 + `ScanEntry` 强类型解码（断言 `byte_offset == Some(835584)`、`quality == "carved"`、`name.is_empty()`）。`handlers.rs`：在既有 `scan_response_goldens_round_trip` 同文件追加 carved 页往返——**为 golden 单独造 memory store 与独立 task**（避免与既有 task 1 抢位），预置 carve golden 的两条 entries → `scan.results {taskId:1,offset:0,limit:10}` → 全等 carved golden。

- [ ] **Step 6: 门禁与提交**
```bash
cargo test --workspace --locked && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A proto/v1 crates/xd-core
git commit -m "feat(proto): v1.1 增量——mode/quality:carved/byteOffset/totalBytes 语义 + store schema v2 迁移"
```

---

### Task 2: 两引擎未分配空间枚举 `unallocated_runs`

**Files:**
- Create: `crates/xd-fs-exfat/src/freespace.rs`（+lib.rs 注册 `pub mod freespace;`）
- Create: `crates/xd-fs-fat/src/freespace.rs`（+lib.rs 注册）

**语义（两引擎同一契约）：** 返回**已排序、互不相交**的字节区间 `Vec<Range<u64>>`（`[start,end)`，簇边界对齐、落于数据簇堆内）；**元数据不可读 → Err**（绝不在"未知分配"上雕刻——诚实拒绝）；空盘/全分配 → 正常值（空盘=一整段）。上限 `MAX_RUNS = 100_000`：到顶截断（调用方以 Σ区间长 为进度目标，不虚报）。

- [ ] **Step 1: exfat freespace.rs（全代码）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! 未分配空间枚举（雕刻输入，§M1c）：**位图是分配权威**，空闲簇合并为字节区间。
//! 位图不可读 → Err（雕刻拒绝在未知分配上猜）；上限 MAX_RUNS 截断并如实计数。

use std::ops::Range;

use crate::ExfatError;
use crate::bitmap::Bitmap;
use crate::boot::{self, ExfatBoot};
use crate::dirent;
use crate::fattab::Fat32;
use crate::read::load_bitmap_from_specials;
use xd_device::BlockDevice;

pub const MAX_RUNS: usize = 100_000;

pub fn unallocated_runs(dev: &dyn BlockDevice) -> Result<Vec<Range<u64>>, ExfatError> {
    let boot = boot::parse(dev)?;
    let fat = Fat32::new(dev, &boot);
    let root_data = crate::scan::read_root_dir(dev, &boot, &fat)?;
    let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
    let bitmap = load_bitmap_from_specials(dev, &boot, &fat, &root.specials)
        .ok_or_else(|| ExfatError::InvalidBoot("位图不可读——雕刻拒绝在未知分配上猜".into()))?;
    Ok(runs_from_free(&boot, &bitmap))
}

/// 空闲簇 2..=count+1 线性合并。`is_free` Err 视为已分配（保守：宁可漏扫不可误扫）。
fn runs_from_free(boot: &ExfatBoot, bitmap: &Bitmap) -> Vec<Range<u64>> {
    let max_cluster = boot.cluster_count as u64 + 1;
    let mut out: Vec<Range<u64>> = Vec::new();
    let mut run_start: Option<u64> = None;
    for c in 2..=max_cluster {
        let free = matches!(bitmap.is_free(c as u32), Ok(true));
        match (free, run_start) {
            (true, None) => run_start = Some(c),
            (false, Some(s)) => {
                out.push(boot.cluster_to_byte(s as u32)..boot.cluster_to_byte(c as u32));
                run_start = None;
            }
            _ => {}
        }
        if out.len() >= MAX_RUNS {
            break; // 截断：不虚报未扫区间
        }
    }
    if let Some(s) = run_start
        && out.len() < MAX_RUNS
    {
        out.push(boot.cluster_to_byte(s as u32)..boot.cluster_to_byte(max_cluster as u32 + 1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot::testutil::dev_for;

    #[test]
    fn fragmented_free_space_yields_exact_runs() {
        // 位图 2 / upcase 3,4 / 根 5 已占用；文件占 7 与 9 → 空闲 = 6、8、10..=253（三区间）
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "A.BIN", &[1u8; 5000], &[7], true)
            .add_file_in_clusters("/", "B.BIN", &[2u8; 5000], &[9], true)
            .build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let runs = unallocated_runs(&dev).unwrap();
        let cb = boot.cluster_bytes();
        assert_eq!(runs.len(), 3, "{runs:?}");
        assert_eq!(runs[0], boot.cluster_to_byte(6)..boot.cluster_to_byte(7));
        assert_eq!(runs[1], boot.cluster_to_byte(8)..boot.cluster_to_byte(9));
        assert_eq!(runs[2], boot.cluster_to_byte(10)..boot.cluster_to_byte(254));
        let total: u64 = runs.iter().map(|r| r.end - r.start).sum();
        assert_eq!(total, (1 + 1 + 244) * cb);
    }

    #[test]
    fn empty_volume_is_one_run_and_unreadable_bitmap_is_err() {
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let runs = unallocated_runs(&dev).unwrap();
        assert_eq!(runs.len(), 1, "空盘：6..=253 一整段");
        assert_eq!(runs[0].start, boot.cluster_to_byte(6));
        // 位图项（根目录 0x81）首簇改越界 → 位图不可读 → Err
        let mut patched = image.clone();
        const ROOT_B: usize = 32 * 512 + 3 * 4096;
        patched[ROOT_B + 32 + 20..ROOT_B + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
        let (_f2, dev2) = dev_for(&patched);
        assert!(unallocated_runs(&dev2).is_err(), "位图不可读必须拒绝");
    }

    #[test]
    fn runs_are_sorted_disjoint_and_never_allocated() {
        // 不变量：排序、不相交、Σ 与位图空闲簇数一致
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "A.BIN", &[1u8; 12000], &[7, 9, 11], false)
            .build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let root_data = crate::scan::read_root_dir(&dev, &boot, &fat).unwrap();
        let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
        let bm = load_bitmap_from_specials(&dev, &boot, &fat, &root.specials).unwrap();
        let runs = unallocated_runs(&dev).unwrap();
        for w in runs.windows(2) {
            assert!(w[0].end <= w[1].start, "不得相交");
        }
        let free_clusters = (2..=boot.cluster_count as u64 + 1)
            .filter(|c| matches!(bm.is_free(*c as u32), Ok(true)))
            .count() as u64;
        let run_bytes: u64 = runs.iter().map(|r| r.end - r.start).sum();
        assert_eq!(run_bytes, free_clusters * boot.cluster_bytes());
    }
}
```

- [ ] **Step 2: fat freespace.rs（同构；FAT 表驱动）**
```rust
//! 未分配空间枚举（雕刻输入，§M1c）：FAT 表空闲项（0x000/0x0000/0x00000000）合并为字节区间。
//! FAT 解析失败 → Err。注意：FAT 删除即清链 → 删除文件的数据簇恰好落在此处（雕刻主战场）。
```
合并循环同构，几何用 `bpb.data_cluster_count()`、`bpb.cluster_to_byte(c)`、`fat.is_free(c)`（三件 API 均已在 fat 侧存在——实施者读 bpb.rs/fat.rs 对齐具体签名与 FAT12 半字节语义）；`is_free` Err 视为已分配；MAX_RUNS 同款。测试（三枚）：
1. `fat12_layout_invariants`：`FatImageBuilder::fat12()` 加一文件 → 每个 run 的字节对齐簇边界、`Σrun 字节 == 空闲簇数 × cluster_bytes`、**文件的簇不在任何 run 内**。
2. `deleted_file_clusters_become_free`：fat32 加文件后 `.delete()` → 这些簇被某 run 覆盖（雕刻可见性：删除文件的空间必须可扫）。
3. `sorted_disjoint_invariants`：`windows(2)` 断言不相交 + 升序。

- [ ] **Step 3: 门禁与提交**
```bash
cargo test -p xd-fs-exfat -p xd-fs-fat --locked
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-fs-exfat crates/xd-fs-fat
git commit -m "feat(fs): 两引擎 unallocated_runs（位图/FAT 驱动的空闲区间枚举，拒绝未知分配）"
```

---

### Task 3: xd-carving 骨架 —— crc32 / 签名与头部验证 / 夹具与资产

**Files:**
- Create: `crates/xd-carving/Cargo.toml`（deps: xd-device path；dev-deps: xd-fixtures path、tempfile）
- Create: `crates/xd-carving/src/{lib.rs, crc32.rs, signatures.rs}`
- Modify: 根 `Cargo.toml`（members + workspace deps 若需）
- Create: `crates/xd-fixtures/src/carving.rs`（`mini_jpeg`/`mini_png`/`plant_in_run`；lib.rs 注册）+ `tests` 小用例
- Create: `crates/xd-fixtures/assets/tiny.png`（68B 已知真实 PNG，base64 见 Step 3）

**重组裁定（v1，写进 lib.rs 头注）：** 候选文件**只在其所在 run 内重组**（跨 run=跨过已分配区拼接他人数据，不可验证 → 拒绝）；同一候选内嵌 JPEG（EXIF 缩略图）不重复上报（跳过已重组区间）；坏读即诚实截断。

- [ ] **Step 1: crc32.rs（全代码）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! IEEE CRC32（PNG chunk 校验；表驱动，无新依赖）。

const fn build_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const TABLE: [u32; 256] = build_table();

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// 增量 CRC（PNG 分块流式校验）：先 crc32_init，逐段 update，末段 finalize。
pub struct Crc32(u32);

impl Crc32 {
    pub fn new() -> Self {
        Self(0xFFFF_FFFF)
    }
    pub fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.0 = TABLE[((self.0 ^ b as u32) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }
    pub fn finalize(self) -> u32 {
        !self.0
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"IEND"), 0xAE42_6082); // PNG IEND 空 chunk 的已知 CRC
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn incremental_matches_oneshot() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let mut inc = Crc32::new();
        inc.update(&data[..10]);
        inc.update(&data[10..]);
        assert_eq!(inc.finalize(), crc32(data));
    }
}
```

- [ ] **Step 2: lib.rs（骨架 + 裁定头注）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! 文件雕刻（carving v1：JPEG/PNG）：在未分配字节区间上做签名识别 + **头部结构验证** +
//! 结构走链重组，产出无文件名结果（byteOffset/size/complete）。
//!
//! 裁定（v1）：候选文件**只在其所在 run 内重组**——跨 run 拼接会跨过已分配区把他人数据
//! 缝进来，顺序不可验证，违反"宁可漏报不可错报"；坏读/区间尽头即诚实截断；同一候选内嵌的
//! 容器（EXIF 缩略图）不重复上报。
//! 说明：设计 §4.3 的"多线程并行块扫描"延后（雕刻 I/O 受限，顺序读已达 §4.6 底线；
//! 并行收益须实测后按需加，届时并行的是"多个 run 的读"而非签名匹配）。

pub mod crc32;
mod jpeg;
mod png;
mod signatures;

pub use signatures::{Carved, Cursor};
```

- [ ] **Step 3: fixtures —— `mini_jpeg`/`mini_png`/`plant_in_run` + tiny.png 资产**
```bash
# 68B 规范 1×1 PNG（已知 base64，含正确 CRC）
printf 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==' | base64 -d > crates/xd-fixtures/assets/tiny.png
test "$(stat -c%s crates/xd-fixtures/assets/tiny.png)" = 68
```
`crates/xd-fixtures/src/carving.rs`：
```rust
//! 雕刻夹具：规范结构合成 JPEG/PNG（供走链器测试）+ 自由区间埋数据。
use std::ops::Range;

/// 结构规范的最小 JPEG（SOI/APP0/SOF0/SOS/熵段/EOI；熵段长 entropy_len，纯 0xAA 无 FF）。
pub fn mini_jpeg(entropy_len: usize) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8]; // SOI
    v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]); // APP0, len=16
    v.extend_from_slice(b"JFIF\0");
    v.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00]); // SOF0
    v.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]); // SOS
    v.extend(std::iter::repeat_n(0xAAu8, entropy_len)); // 熵段（无 0xFF）
    v.extend_from_slice(&[0xFF, 0xD9]); // EOI
    v
}

/// 规范结构的最小 PNG（IHDR + 一个 IDAT + IEND，全部 CRC 正确）。
pub fn mini_png(payload: &[u8]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(ty);
        out.extend_from_slice(data);
        let mut c = xd_carving_crc(ty, data); // 见下：本 crate 不允许依赖 xd-carving？——改为本地实现见 Step 4 裁定
        out.extend_from_slice(&c.to_be_bytes());
        c = !c; // 占位，实际实现见下
    }
    ...
}
```
**裁定：fixtures 不得依赖 xd-carving**（依赖方向：测试 crate 都得能建夹具，引擎→fixtures 依赖会成环）。`xd-fixtures` 自带 10 行 crc32 拷贝并加注释「与 xd-carving::crc32 同源实现的第二份——夹具与被测实现不得共用同一份代码，否则 CRC 验证测试会自证；两副本由 `crc32_matches_carving_impl`（xd-carving 侧 dev-dep xd-fixtures 的对照测试）钉住一致」。`mini_png` 完整实现（IHDR：宽 1 高 1 位深 8 色型 6，IDAT 为传入 payload（可为任意字节，不需要真实 zlib——走链器不解释 IDAT），IEND 空）：
```rust
fn crc32(data: &[u8]) -> u32 { /* 同标准算法（10 行） */ }
fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(ty);
    out.extend_from_slice(data);
    let mut c = crc32(ty);
    c = crc32_update(c, data); // 或拼接后一次算（ty+data 拼 8+N 字节，量小无妨）
    out.extend_from_slice(&c.to_be_bytes());
}
pub fn mini_png(payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    chunk(&mut v, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    chunk(&mut v, b"IDAT", payload);
    chunk(&mut v, b"IEND", &[]);
    v
}
```
再加载 68B 资产为 `pub const TINY_PNG: &[u8] = include_bytes!("../assets/tiny.png");`。`plant_in_run`：
```rust
/// 把 `data` 写进镜像的绝对字节 `offset`（测试专用：调用方负责选空闲簇区间内的偏移）。
pub fn plant_in_run(img: &mut [u8], offset: u64, data: &[u8]) {
    let o = offset as usize;
    img[o..o + data.len()].copy_from_slice(data);
}
```

- [ ] **Step 4: fixtures 自测（小）**
```rust
#[test]
fn mini_fixtures_are_wellformed() {
    // mini_jpeg：SOI 开头 EOI 结尾、长度自洽；mini_png：magic 开头 IEND 结尾、TINY_PNG 也走通
    let j = mini_jpeg(100);
    assert_eq!((j[0], j[1], j[j.len() - 2], j[j.len() - 1]), (0xFF, 0xD8, 0xFF, 0xD9));
    let p = mini_png(b"abc");
    assert_eq!(&p[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    assert_eq!(&p[p.len() - 8..p.len() - 4], &[0, 0, 0, 0]);
    assert_eq!(&p[p.len() - 4..], &[0xAE, 0x42, 0x60, 0x82], "IEND CRC 已知向量");
    assert_eq!(TINY_PNG.len(), 68);
}
```

- [ ] **Step 5: signatures.rs（全代码）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! 签名识别与游标：签名只是"候选"，是否成立由结构走链器（jpeg.rs/png.rs）裁决。

use xd_device::BlockDevice;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signature {
    Jpeg,
    Png,
}

pub const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// 扫描窗口需在块边界保留的重叠字节数（≥ 最长签名 + 余量）。
pub const OVERLAP: usize = 16;

/// 在窗口内找所有候选签名起点（朴素扫描；4MiB 块上 memchr 级开销可接受）。
pub fn find_candidates(buf: &[u8]) -> Vec<(usize, Signature)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] == 0xFF && buf[i + 1] == 0xD8 && buf[i + 2] == 0xFF {
            out.push((i, Signature::Jpeg));
            i += 3;
            continue;
        }
        if i + 8 <= buf.len() && buf[i..i + 8] == PNG_MAGIC {
            out.push((i, Signature::Png));
            i += 8;
            continue;
        }
        i += 1;
    }
    out
}
```
（PNG_MAGIC 比较用逐字节更快？`buf[i..i+8] == PNG_MAGIC` 对 slice 可行 ✓。）

```rust
/// 结果：重组长度（字节）+ 是否结构完整（找到 EOI/IEND）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Carved {
    pub len: u64,
    pub complete: bool,
}

/// 绝对字节游标：只前进、止于 `end`（所在 run 的右界）；设备坏读/越界 → None（诚实截断）。
pub struct Cursor<'a> {
    dev: &'a dyn BlockDevice,
    pub pos: u64,
    pub end: u64,
}

impl<'a> Cursor<'a> {
    pub fn new(dev: &'a dyn BlockDevice, pos: u64, end: u64) -> Self {
        Self { dev, pos, end }
    }

    /// 读满 buf 并前进；任一步失败/到界 → None（游标停在失败处）。
    pub fn take(&mut self, buf: &mut [u8]) -> Option<()> {
        if buf.is_empty() {
            return Some(());
        }
        if self.pos + buf.len() as u64 > self.end {
            return None;
        }
        match self.dev.read_at(self.pos, buf) {
            Ok(n) if n == buf.len() => {
                self.pos += n as u64;
                Some(())
            }
            _ => None,
        }
    }

    /// 读 1/2/4 字节便捷。
    pub fn u8(&mut self) -> Option<u8> {
        let mut b = [0u8; 1];
        self.take(&mut b)?;
        Some(b[0])
    }
    pub fn u16_be(&mut self) -> Option<u16> {
        let mut b = [0u8; 2];
        self.take(&mut b)?;
        Some(u16::from_be_bytes(b))
    }
    pub fn u32_be(&mut self) -> Option<u32> {
        let mut b = [0u8; 4];
        self.take(&mut b)?;
        Some(u32::from_be_bytes(b))
    }

    /// 跳过 n 字节（不读，只走位；用于长度字段声明的段体）。
    pub fn skip(&mut self, n: u64) -> Option<()> {
        if self.pos + n > self.end {
            return None;
        }
        self.pos += n;
        Some(())
    }

    /// 读一段（≤ cap）进内存（CRC 用；cap 防大段爆内存——分块调用方自理）。
    pub fn take_vec(&mut self, n: usize) -> Option<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.take(&mut v)?;
        Some(v)
    }
}
```

- [ ] **Step 6: 门禁与提交**
```bash
cargo test -p xd-carving -p xd-fixtures --locked
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A Cargo.toml Cargo.lock crates/xd-carving crates/xd-fixtures
git commit -m "feat(carving): xd-carving 骨架（CRC32/签名/Cursor）+ 夹具 mini_jpeg/mini_png/tiny.png"
```

---

### Task 4: JPEG 重组（marker 走链 + 熵段 EOI）

**Files:**
- Create: `crates/xd-carving/src/jpeg.rs`
- Modify: `crates/xd-carving/src/lib.rs`（`pub use jpeg::carve_jpeg;`）

**算法（写进模块头注）：** SOI → 段循环（段 = `FF marker` + 2 字节大端长度（含自身，≥2）+ 段体跳过；记号白名单：APPn/DQT/SOFn/COM/DRI/DHT；0xFF 填充字节允许）→ SOS 段（声明的头部跳过后）→ **熵段扫描**：`FF 00` 转义跳过、`FF D0..D7` RST 跳过、`FF D9` = EOI（**完整**）、其它 `FF xx` = 新记号（**回段循环**——渐进式 JPEG 多次 SOS 合法）；**未见过 SOS 就 EOI/遇到非法记号 → 拒绝（假阳性）**；到 `max_len` 或 run 界（游标 None）→ 诚实截断（`complete=false`，len=已耗字节）。

- [ ] **Step 1: jpeg.rs（全代码）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! JPEG 重组（见模块算法注）。返回 None = 结构裁决假阳性（调用方继续扫）。

use crate::signatures::{Carved, Cursor};

const MAX_SEGMENT: u32 = 1024 * 1024; // 单段上限（防长度字段攻击；合法 JPEG 段远小于此）

/// 从 SOI 起重组。`cur` 已定位在 SOI（FFD8FF 已被签名层确认前 3 字节）。
/// `max_len`：重组上限（超出 → 截断）。
pub fn carve_jpeg(cur: &mut Cursor<'_>, max_len: u64) -> Option<Carved> {
    let start = cur.pos;
    if cur.u8()? != 0xFF || cur.u8()? != 0xD8 {
        return None;
    }
    let mut saw_sos = false;
    loop {
        if cur.pos - start >= max_len {
            return Some(Carved { len: max_len.min(cur.pos - start), complete: false });
        }
        // 记号：允许 0xFF 填充
        let Some(mut m) = cur.u8() else {
            return Some(Carved { len: cur.pos - start, complete: false });
        };
        if m != 0xFF {
            return Some(Carved { len: cur.pos - start - 1, complete: false }); // 结构破裂 → 截断
        }
        while m == 0xFF {
            // 填充分隔；EOF/界 → 截断
            let Some(b) = cur.u8() else {
                return Some(Carved { len: cur.pos - start, complete: false });
            };
            m = b;
        }
        match m {
            0xD9 => {
                // EOI：未过 SOS = 无图像数据的空壳 → 假阳性拒绝
                return if saw_sos {
                    Some(Carved { len: cur.pos - start, complete: true })
                } else {
                    None
                };
            }
            0xDA => {
                // SOS：长度（含 2 字节自身）→ 跳头部 → 熵段扫描
                let Some(len) = cur.u16_be() else {
                    return Some(Carved { len: cur.pos - start, complete: false });
                };
                if !(2..=MAX_SEGMENT as u16).contains(&len) {
                    return Some(Carved { len: cur.pos - start, complete: false });
                }
                if cur.skip(len as u64 - 2).is_none() {
                    return Some(Carved { len: cur.pos - start, complete: false });
                }
                saw_sos = true;
                // 熵段扫描
                loop {
                    if cur.pos - start >= max_len {
                        return Some(Carved { len: max_len.min(cur.pos - start), complete: false });
                    }
                    let Some(b) = cur.u8() else {
                        return Some(Carved { len: cur.pos - start, complete: false });
                    };
                    if b != 0xFF {
                        continue;
                    }
                    let Some(n) = cur.u8() else {
                        return Some(Carved { len: cur.pos - start, complete: false });
                    };
                    match n {
                        0x00 | 0xD0..=0xD7 => continue,          // 转义/RST 跳
                        0xFF => { cur.pos -= 1; continue; }      // 连续 FF：回退一个当填充重判
                        0xD9 => return Some(Carved { len: cur.pos - start, complete: true }),
                        _ => {
                            cur.pos -= 2; // 新记号（渐进式多扫描/段间）：退回 FF xx 交回段循环
                            break;
                        }
                    }
                }
            }
            // 段记号白名单：白名单外（如 0xD8 二次 SOI、0x01 TEM 少见但合法、0x00 非法）
            0xC0..=0xCF | 0xDB | 0xDD | 0xE0..=0xEF | 0xFE | 0x01 => {
                let Some(len) = cur.u16_be() else {
                    return Some(Carved { len: cur.pos - start, complete: false });
                };
                if !(2..=MAX_SEGMENT as u16).contains(&len) {
                    // 长度非法：可能是假阳性（随机数据）——但已走这么远，按截断处理并交由上层去重
                    return Some(Carved { len: cur.pos - start, complete: false });
                }
                if cur.skip(len as u64 - 2).is_none() {
                    return Some(Carved { len: cur.pos - start, complete: false });
                }
            }
            _ => return Some(Carved { len: cur.pos - start, complete: false }),
        }
    }
}
```
**注意两处语义**：`0xC4`（DHT）与 `0xDA` 冲突？0xC4 ∈ 0xC0..=0xCF ✓ 白名单含。`0xD8` 二次出现 → `_` 臂截断（假阳性防线）。`cur.pos -= 1/2` 回退是安全的（pos 从未越界，回退仅在本函数已读的字节内；`skip`/`take` 只增 pos——回退后再次前进不破坏不变量）。**`complete=false` 且 len==0 的极端**（SOI 后立刻破结构）：`len=0` 会让扫描器原地打转！——扫描器契约：**推进至少 = 签名长**（见 T6），len=0 也前进 3 字节 ✓（此约定在 T6 Step 1 的实现里承重，jpeg.rs 无需特殊处理，但头注写明）。

- [ ] **Step 2: jpeg 测试（全代码）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::signatures::Cursor;
    use xd_fixtures::carving::mini_jpeg;
    use xd_fixtures::testdev::dev_from_bytes; — 若 fixtures 未提供，改用 testutil 模式建临时文件（实施者二选一，优先复用 xd-fixtures 中已有助手）

    fn carve_all(dev: &dyn xd_device::BlockDevice, start: u64, end: u64, max: u64) -> Option<Carved> {
        let mut cur = Cursor::new(dev, start, end);
        carve_jpeg(&mut cur, max)
    }

    #[test]
    fn carves_whole_jpeg_complete() {
        let j = mini_jpeg(1000);
        let mut img = vec![0u8; 4096];
        img[100..100 + j.len()].copy_from_slice(&j);
        let (_f, dev) = dev_for(&img);
        let r = carve_all(&dev, 100, 4096, 64 << 20).unwrap();
        assert_eq!(r.len, j.len() as u64, "重组长度逐字节等于原文件");
        assert!(r.complete);
    }

    #[test]
    fn stuffed_bytes_and_restart_markers_do_not_end_scan() {
        // 熵段含 FF00 与 FFD0（RST0）：不得误判结束；EOI 在最后
        let mut j = mini_jpeg(0);
        reinterpret: 在 SOS 之后手工插：0xAA,0xFF,0x00,0xAA,0xFF,0xD0,0xAA 再 EOI
        （做法：取 mini_jpeg(0)（SOI..SOS,EOI），把 EOI 两字节替换为上述序列+EOI）
        assert complete && len == j.len()
    }

    #[test]
    fn eoi_without_sos_is_rejected() {
        // FFD8 FFE0(len) … FFD9：无图像数据 → 假阳性 None
    }

    #[test]
    fn img_without_eoi_truncates_at_run_end() {
        let j = mini_jpeg(5000);
        let cut = j.len() - 3; // 砍掉 EOI 前两字节还会剩 FF → 用 cut = 到 EOI 前
        let mut img = vec![0u8; 1024 + 100];
        let n = (img.len() - 100).min(cut);
        img[100..100 + n].copy_from_slice(&j[..n]);
        let r = carve_all(&dev, 100, img.len() as u64, 64 << 20).unwrap();
        assert!(!r.complete && r.len == n as u64, "到 run 界诚实截断");
    }

    #[test]
    fn max_len_cap_truncates() {
        let j = mini_jpeg(100_000);
        let r = carve_all(&dev, 100, 100 + j.len() as u64, 4096).unwrap();
        assert!(!r.complete);
        assert_eq!(r.len, 4096, "上限截断");
    }

    #[test]
    fn garbage_after_soi_rejected_or_truncated_not_complete() {
        // FFD8FF 后随机字节：Never complete
    }

    #[test]
    fn progressive_like_multi_sos_completes() {
        // SOS…熵…FFDA(段)…SOS…熵…EOI（渐进式形态）→ complete
    }
}
```
（测试里构造具体字节序列的代码由实施者按上述语义补全——每个测试的**断言语义**是规范：长度逐字节相等 / complete 标志 / 截断位置。）

- [ ] **Step 3: 门禁与提交**
```bash
cargo test -p xd-carving --locked
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-carving
git commit -m "feat(carving): JPEG marker 走链重组（熵段转义/RST/多扫描，假阳性拒绝，诚实截断）"
```

---

### Task 5: PNG 重组（chunk 走链 + CRC + IEND）

**Files:**
- Create: `crates/xd-carving/src/png.rs`
- Modify: `crates/xd-carving/src/lib.rs`（`pub use png::carve_png;`）

**算法：** 8 字节 magic → 首 chunk **必须是 IHDR**（len=13、CRC 验证通过）→ chunk 循环：`len(u32 BE) + type(4) + data(len) + crc(4)`，**CRC = crc32(type ‖ data)** 逐块流式验证；CRC 不符 → 截断（len = 坏块起点）；`IEND`（CRC 正确）→ **完整**；`max_len`/run 界 → 截断。`len` 上限 `MAX_CHUNK = 16MiB`（更大 → 截断），防长度字段攻击。

- [ ] **Step 1: png.rs（全代码）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! PNG 重组：chunk 走链 + CRC32 逐块验证（比纯签名强得多的假阳性防线）+ IEND 收束。

use crate::crc32::Crc32;
use crate::signatures::{Carved, Cursor, PNG_MAGIC};

const MAX_CHUNK: u32 = 16 * 1024 * 1024;
const CRC_BUF: usize = 64 * 1024;

pub fn carve_png(cur: &mut Cursor<'_>, max_len: u64) -> Option<Carved> {
    let start = cur.pos;
    let mut magic = [0u8; 8];
    cur.take(&mut magic)?;
    if magic != PNG_MAGIC {
        return None;
    }
    let mut first = true;
    loop {
        if cur.pos - start >= max_len {
            return Some(Carved { len: max_len.min(cur.pos - start), complete: false });
        }
        let Some(len) = cur.u32_be() else {
            return Some(Carved { len: cur.pos - start, complete: false });
        };
        if len > MAX_CHUNK {
            return Some(Carved { len: cur.pos - start, complete: false });
        }
        let mut ty = [0u8; 4];
        if cur.take(&mut ty).is_none() {
            return Some(Carved { len: cur.pos - start, complete: false });
        }
        if first && (&ty != b"IHDR" || len != 13) {
            return None; // 头部裁决：首块非 13 字节 IHDR → 假阳性
        }
        first = false;
        let mut crc = Crc32::new();
        crc.update(&ty);
        let mut remaining = len as u64;
        let mut buf = vec![0u8; CRC_BUF.min(len.max(1) as usize)];
        while remaining > 0 {
            let n = remaining.min(buf.len() as u64) as usize;
            if cur.take(&mut buf[..n]).is_none() {
                return Some(Carved { len: cur.pos - start, complete: false });
            }
            crc.update(&buf[..n]);
            remaining -= n as u64;
        }
        let Some(expect) = cur.u32_be() else {
            return Some(Carved { len: cur.pos - start, complete: false });
        };
        if crc.finalize() != expect {
            // 坏块：交付到坏块起点（不含）——CRC 是强判据，此行之后的数据不可信
            let cut = cur.pos - start - (len as u64 + 8);
            return Some(Carved { len: cut, complete: false });
        }
        if &ty == b"IEND" {
            return Some(Carved { len: cur.pos - start, complete: true });
        }
    }
}
```
注意：`cut` 计算 = 当前位置 - 已读坏块长度(4+4+len+4=len+12) → `cur.pos - start - (len as u64 + 12)`。修正代码里写 8 是错的——Step 1 定为 `len + 12`。哦等等：坏块起点 = 该 chunk 的 len 字段开始处 = cur.pos - (4 len + 4 ty + len + 4 crc) = cur.pos - (len+12) ✓ `cut = cur.pos - start - (len as u64 + 12)`。若 cut 为 0（首块即坏）→ len=0，上层至少推进 8 字节（PNG 签名长）✓ 记入扫描器契约。

- [ ] **Step 2: png 测试（语义钉死，同 T4 风格）**
1. `carves_tiny_png_complete`：`TINY_PNG` 资产 → complete + len==68。
2. `bad_crc_truncates_at_chunk_start`：把 IDAT 的 CRC 改一字节 → complete=false 且 len == 坏块起点（IHDR 块之后）。
3. `first_chunk_not_ihdr_rejected`：magic + 随机块 → None。
4. `iend_with_bad_crc_not_complete`：IEND CRC 破坏 → 截断（不能 complete）。
5. `max_len_cap`：cap 小于文件 → 截断。
6. `crc32_matches_carving_impl`（对照测试）：`xd_fixtures::carving` 的内部 crc32 与 `crate::crc32::crc32` 对三组向量一致（副本一致性守住）。

- [ ] **Step 3: 门禁与提交**
```bash
cargo test -p xd-carving --locked
cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-carving
git commit -m "feat(carving): PNG chunk 走链重组（CRC 逐块验证/IEND 收束/坏块诚实截断）"
```

---

### Task 6: 顺序块扫描器 + 深扫编排接线（mode:"deep"）

**Files:**
- Create: `crates/xd-carving/src/carver.rs`（+lib.rs `pub use carver::{carve_runs, CarveEvent, CarvedEntry, CarveStats};`）
- Create: `crates/xd-carving/tests/carve_e2e.rs`（T8 前先立骨架，本任务的扫描器级测试放 `carver.rs` 内）
- Modify: `crates/xd-core/src/{api.rs（-32005 构造器）, store.rs（schema v3: mode 列）, scan_task.rs（ScanKind/start_deep/carve worker）, handlers.rs（mode 校验/路由）}`
- Modify: `proto/v1/README.md`（mode deep 生效 + -32005）+ Create: `proto/v1/examples/error_unallocated_unavailable.response.json`
- Modify: `crates/xd-core/tests/contract_v1.rs`、`handlers.rs` 测试

**扫描器不变量（写进 carver.rs 头注，测试钉死）：**
1. 每个候选无论裁决结果，扫描位置**至少推进其签名长度**（len=0 不原地打转）。
2. 窗口重叠由"重读窗口尾部 7 字节"实现：`next_read = max(pos, buf_end - 7)`；跨块签名不丢（PNG magic 8 字节，取 -7 即最坏起点回读）。
3. 跨块签名未取全且 run 未尽 → 记 `next_read = abs`，下一轮从候选处重读。
4. 坏读/空读：该窗口 span 计入 `scanned`（进度=**尝试扫描**字节，诚实到 100%），前进不中断。
5. `Scanned(累计)` 事件在**每窗口**发出（≤4MiB 粒度：暂停/取消/检查点响应及时）；`Entry` 事件每条雕刻发出；回调返回 `false` = 停止（取消）。

- [ ] **Step 1: carver.rs（全代码）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! 顺序块扫描器（不变量见计划 T6 头注 1-5）。只做 I/O 与调度；暂停/取消/落库由 `ev` 回调承载。

use std::ops::Range;

use xd_device::BlockDevice;

use crate::jpeg::carve_jpeg;
use crate::png::carve_png;
use crate::signatures::{Cursor, Signature, find_candidates};

pub const CHUNK_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// 签名最大长度（PNG magic 8）——窗口回读安全边界。
const MAX_SIG_LEN: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarvedEntry {
    pub byte_offset: u64,
    pub size: u64,
    pub complete: bool,
    pub signature: Signature,
}

impl Signature {
    pub fn ext(self) -> &'static str {
        match self {
            Signature::Jpeg => "jpg",
            Signature::Png => "png",
        }
    }
    fn len(self) -> u64 {
        match self {
            Signature::Jpeg => 3,
            Signature::Png => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CarveStats {
    pub scanned_bytes: u64,
    pub found: u64,
}

/// 扫描事件：窗口级进度 + 每条雕刻结果。回调返回 false → 立即停止（取消）。
pub enum CarveEvent<'a> {
    Scanned(u64),
    Entry(&'a CarvedEntry),
}

pub fn carve_runs(
    dev: &dyn BlockDevice,
    runs: &[Range<u64>],
    max_file_bytes: u64,
    ev: &mut dyn FnMut(CarveEvent) -> bool,
) -> CarveStats {
    let mut stats = CarveStats::default();
    for run in runs {
        if !scan_run(dev, run, max_file_bytes, ev, &mut stats) {
            return stats; // 取消
        }
    }
    stats
}

/// 单 run 扫描。返回 false = 取消。
fn scan_run(
    dev: &dyn BlockDevice,
    run: &Range<u64>,
    max_file_bytes: u64,
    ev: &mut dyn FnMut(CarveEvent) -> bool,
    stats: &mut CarveStats,
) -> bool {
    let mut pos = run.start; // 已处理到的绝对位置（候选起点下界）
    let mut next_read = run.start; // 下一窗口读取起点
    while next_read < run.end {
        let want = ((run.end - next_read) as usize).min(CHUNK_BYTES);
        let mut buf = vec![0u8; want];
        let got = match dev.read_at(next_read, &mut buf) {
            Ok(n) => n,
            Err(_) => 0,
        };
        let span_end = next_read + want as u64;
        stats.scanned_bytes += want as u64; // 不变量 4：尝试扫描
        if !ev(CarveEvent::Scanned(stats.scanned_bytes)) {
            return false;
        }
        if got == 0 {
            next_read = span_end;
            pos = pos.max(span_end);
            continue;
        }
        buf.truncate(got);
        let buf_end = next_read + got as u64;
        let mut advance_to = buf_end.saturating_sub(MAX_SIG_LEN - 1); // 不变量 2
        for (i, sig) in find_candidates(&buf) {
            let abs = next_read + i as u64;
            if abs < pos {
                continue; // 重叠区里上一轮已处理过
            }
            if abs + sig.len() > buf_end && buf_end < run.end {
                advance_to = abs; // 不变量 3：签名未取全，下轮从候选重读
                break;
            }
            let mut cur = Cursor::new(dev, abs, run.end);
            let carved = match sig {
                Signature::Jpeg => carve_jpeg(&mut cur, max_file_bytes),
                Signature::Png => carve_png(&mut cur, max_file_bytes),
            };
            let entry = carved.map(|c| CarvedEntry {
                byte_offset: abs,
                size: c.len,
                complete: c.complete,
                signature: sig,
            });
            match entry {
                Some(e) if e.size >= sig.len() => {
                    pos = e.byte_offset + e.size;
                    if !ev(CarveEvent::Entry(&e)) {
                        return false;
                    }
                    stats.found += 1;
                    advance_to = advance_to.max(pos); // 不变量 1 的强化：跳过已重组区间（内嵌容器不重报）
                }
                _ => {
                    pos = abs + sig.len(); // 不变量 1
                    advance_to = advance_to.max(pos);
                }
            }
        }
        next_read = next_read.max(advance_to);
        if next_read <= pos && pos < run.end {
            next_read = pos; // 防死角推进
        }
    }
    true
}
```
（`advance_to.max(pos)` 与 `next_read = next_read.max(advance_to)` 两行共同保证：`next_read` 单调不减且 ≥ 上轮 `next_read`——不死循环。实施者如发现等价化简可简化，但不变量 1-4 的测试必须全过。）

- [ ] **Step 2: carver.rs 测试（语义钉死；复用 T4/T5 的 dev 助手）**
1. `finds_both_formats_in_one_run`：run 内埋 mini_jpeg(1000)@100、mini_png@8000 → 2 条，byte_offset/size/ext 逐一相等。
2. `signature_straddling_chunk_boundary_is_found`：把 mini_png 起点放在 CHUNK_BYTES-4（跨窗口）→ 仍找到且 offset 精确。（用 4MiB+ 镜像；测试可接受 4MiB 内存。）
3. `bad_region_skipped_and_progress_reaches_target`：中间 1MiB 坏读（read_at 对区间返 Err 的包装设备）→ 后续候选仍被找到；`scanned == Σrun 长`（100%）。
4. `cancel_stops_immediately`：回调在首个 Scanned 返回 false → stats.found==0 且 scanned 只覆盖首个窗口。
5. `decoy_rejected`：`FFD8FF` + 随机垃圾（无 SOS）+ `PNG magic` + 非 IHDR 首块 → 0 条。
6. `adjacent_files_not_rescanned`：两个 JPEG 紧邻（中间仅 1 字节间隔）→ 都找到（第一条重组到 EOI 后从其后继续）。

- [ ] **Step 3: 契约与 store 扩展**
- `proto/v1/README.md`：mode 值域表注「deep 自 M1c 起有效」；错误表加 `-32005 `Cannot determine free space``；golden：
```bash
cat > proto/v1/examples/error_unallocated_unavailable.response.json <<'EOF'
{"jsonrpc":"2.0","id":12,"error":{"code":-32005,"message":"Cannot determine free space"}}
EOF
```
- `api.rs`：`RpcError::unallocated_unavailable()` → -32005（固定文案）。
- `store.rs` schema **v3**：`tasks` 加列 `scan_mode TEXT NOT NULL DEFAULT 'quick'`（迁移模版同 T1 v2：user_version<3 时列探测 + ALTER + 置 3）。`create_task` 增参 `mode: &str`（**全仓调用点更新**：scan_task quick 路径传 `"quick"`、handlers 测试、store 测试）；`TaskRow` 加 `scan_mode: String`；`task()` 读取。
- `contract_v1.rs`：error golden 解码断言（-32005 文案逐字）。

- [ ] **Step 4: scan_task.rs —— 深扫 worker（关键代码）**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    Quick,
    Deep,
}

/// 深扫启动：先解未分配区间（失败 → 拒绝），再入册开跑。
pub fn start_deep(&self, device: Arc<dyn BlockDevice>) -> Result<ScanStarted, ScanError> {
    let fs = probe(&*device).map_err(|_| ScanError::UnsupportedFs)?;
    let runs = unallocated_runs_of(&*device, fs)?;
    let total: u64 = runs.iter().map(|r| r.end - r.start).sum();
    let id = self.store.create_task(&device.info().id, fs.as_str(), "deep", total)?;
    self.spawn(id, device, ScanKind::Deep);
    Ok(ScanStarted { task_id: id, fs, total_bytes: total })
}

fn unallocated_runs_of(
    dev: &dyn BlockDevice,
    fs: FsKind,
) -> Result<Vec<std::ops::Range<u64>>, ScanError> {
    let r = match fs {
        FsKind::Fat => xd_fs_fat::freespace::unallocated_runs(dev),
        FsKind::Exfat => xd_fs_exfat::freespace::unallocated_runs(dev),
    };
    r.map_err(|_| ScanError::UnallocatedUnavailable)
}
```
`spawn` 增参 `kind: ScanKind`（`Active` 记录之；quick 传 `ScanKind::Quick`）；`Active` 加 `kind`。深扫 worker（与 `run_worker` 并列；取消**不用 panic**——雕刻循环是自家代码，回调 `false` 即停，头注写明与快扫的不对称原因）：
```rust
fn run_carve_worker(
    id: u64,
    device: Arc<dyn BlockDevice>,
    runs: Vec<std::ops::Range<u64>>,
    ctrl: Arc<Ctrl>,
    store: Arc<Store>,
    notify: NotifyFn,
) {
    let start = Instant::now();
    let counting = CountingDev { inner: device, bytes: AtomicU64::new(0) };
    let mut state = CarveProgress {
        task_id: id,
        store: &store,
        notify: &*notify,
        ctrl: &ctrl,
        bytes: &counting.bytes,
        start,
        found: 0,
        scanned: 0,
        last_notify_at: start,
        last_notify_bytes: 0,
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        xd_carving::carve_runs(&counting, &runs, xd_carving::MAX_FILE_BYTES, &mut |ev| state.on(ev))
    }));
    let (st, msg) = match outcome {
        Ok(stats) if ctrl.canceled.load(Ordering::SeqCst) => (ScanState::Canceled, None),
        Ok(_) => (ScanState::Completed, None),
        Err(_) => (ScanState::Failed, Some("carve worker panicked".into())),
    };
    let elapsed = start.elapsed().as_millis() as u64;
    let _ = store.set_progress(id, state.scanned, state.found, elapsed);
    let _ = store.set_state_if_active(id, st);
    (notify)(crate::notify::notification(
        "scan.finished",
        json!({ "taskId": id, "state": state_str(st), "foundCount": state.found, "elapsedMs": elapsed }),
    ));
    if let Some(m) = msg {
        eprintln!("warn: carve task {id} failed: {m}");
    }
}

struct CarveProgress<'a> { /* 字段同 Progress + scanned: u64 */ }
impl CarveProgress<'_> {
    fn on(&mut self, ev: xd_carving::CarveEvent<'_>) -> bool {
        if self.ctrl.canceled.load(Ordering::SeqCst) {
            return false;
        }
        while self.ctrl.paused.load(Ordering::SeqCst) {
            if self.ctrl.canceled.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        match ev {
            xd_carving::CarveEvent::Scanned(n) => {
                self.scanned = n;
                let now = Instant::now();
                let read = self.bytes.load(Ordering::Relaxed);
                if now.duration_since(self.last_notify_at) >= Duration::from_millis(250)
                    || self.scanned.saturating_sub(self.last_notify_bytes) >= 1024 * 1024
                {
                    let elapsed = self.start.elapsed().as_millis() as u64;
                    let _ = self.store.set_progress(self.task_id, self.scanned, self.found, elapsed);
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
                let _ = read; // readBytes 口径=已扫（目标对齐）；CountingDev 计数仅供诊断
            }
            xd_carving::CarveEvent::Entry(e) => {
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
                };
                self.found += 1;
                let _ = self.store.insert_entries(self.task_id, std::slice::from_ref(&entry));
            }
        }
        true
    }
}
```
（`ScanEntry` 字面量改动波及快扫映射：`fat_to_entry`/`exfat_to_entry` 加 `byte_offset: None`——全仓 `ScanEntry {` 收口。）

- [ ] **Step 5: handlers 路由**
`scan.start`：mode 校验改为 `None | Some("quick") | Some("deep")`（其它 → -32602）；`Some("deep")` → `ctx.scans.start_deep(dev)`；`scan_err` 增 `ScanError::UnallocatedUnavailable => err(req, RpcError::unallocated_unavailable())`。测试：
1. `deep_scan_streams_carved_entries`：exfat 夹具 + `plant_in_run` 埋 mini_jpeg（在空闲簇绝对偏移）→ `scan.start {mode:"deep"}` → 轮询 completed → `scan.results` 有 `quality=="carved"` 且 `byteOffset` 精确、`size` 为完整重组长；`totalBytes == Σ空闲`。
2. `deep_without_free_space_info_fails_32005`：位图首簇改越界 → `scan.start {mode:"deep"}` → -32005（同设备 quick 仍可成功——对照断言）。
3. `unknown_mode_rejected`：`mode:"full"` → -32602。

- [ ] **Step 6: 门禁与提交**
```bash
cargo test --workspace --locked && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-carving crates/xd-core proto/v1
git commit -m "feat(carving): 顺序块扫描器 + 深扫编排（mode:deep/-32005/carved 落库与进度百分比）"
```

---

### Task 7: 深扫检查点与断点续跑（设计 §4.4 约束 3 的真实兑现）

**Files:**
- Modify: `crates/xd-carving/src/{carver.rs（resume 起点）, lib.rs}`
- Modify: `crates/xd-core/src/{store.rs（schema v4: carved_offset）, scan_task.rs（深扫检查点写/续跑）, handlers.rs（无需改——resume 语义内部按 mode 分派）}`

**语义：** 深扫 worker 在每个 `Scanned(n)` 事件把 `carved_offset = n` 落库（节流同进度）；暂停/杀进程/重启后 `scan.resume` 对 **paused + deep** 任务：`restart()` 不清结果、按 `carved_offset` 从断点续跑（**已雕刻条目保留**，`idx` 从 `found_count` 续号）；`carved_offset` 为 NULL/0 或 quick 任务 → 维持现有语义（quick 清结果重跑；deep 无检查点则从头、但仍保留空结果表）。**同一进程内 pause/resume 走内存阻塞，不读检查点**（无重扫）。

- [ ] **Step 1: carver 续跑起点**
```rust
/// 从 `resume_from`（≥ 首个 run 起点）续扫：跳过完全在其前的 run，裁剪跨断点的首 run。
/// `resume_from` 落于某 run 内部（或 run 之间）都由 `skip` 逻辑吸收；≥ 全面界 → 空扫描（已完成态）。
pub fn carve_runs_from(
    dev: &dyn BlockDevice,
    runs: &[Range<u64>],
    resume_from: u64,
    max_file_bytes: u64,
    ev: &mut dyn FnMut(CarveEvent) -> bool,
) -> CarveStats { ... }
```
实现：`carve_runs` 增内部 `resume_from: u64` 参数（公开两形态：`carve_runs` 传 0、`carve_runs_from` 传值）；`scan_run` 起始 `pos = next_read = max(run.start, resume_from)`（**注意：断点恰在候选中途 → 该候选被视为新候选重扫一遍 → 可能重复雕刻**——去重规则：`Entry` 落库用 `INSERT OR REPLACE` + `idx` 续号 → 同一文件可能以两条记录出现（一条完整、一条从断点重扫的残余）。**裁定**：接受此罕见重复（断点粒度 4MiB 内、重启才可能），不做跨会话去重（复杂度不值）；在 README 局限节记一行。测试钉住行为：断点重扫会产生「至多一条残余记录」而非崩溃/丢数据。
测试：
1. `resume_from_mid_run_scans_remainder`：两个文件，resume_from 落在第二个文件起点 → 恰好找到第二个（第一个不重报）。
2. `resume_from_between_runs_skips_earlier`：多 run 夹具，resume_from 落 run 间隙 → 后续 run 全扫。
3. `resume_from_beyond_end_is_empty_stats`。

- [ ] **Step 2: store schema v4 + 读写**
`tasks` 加列 `carved_offset INTEGER`（NULL 可空；v4 迁移模版同前）。`create_task` 不变（deep 创建时 carved_offset NULL=0 起点）；`TaskRow.carved_offset: Option<u64>`；新增 `set_carved_offset(id, v)`（worker 检查点写）。测试：`carved_offset_roundtrip_and_null_default` + 迁移测试（v3 库 → v4 可读写）。

- [ ] **Step 3: scan_task —— 检查点写与续跑**
- `CarveProgress::on(Scanned)` 节流块内追加：`let _ = self.store.set_carved_offset(self.task_id, self.scanned);`
- `restart()` 重构为按 mode 分派：
```rust
    /// 重启后重跑/续跑：quick → 清结果重扫；deep → 从检查点续扫（结果与 idx 保留）。
    pub fn restart(&self, id: u64, device: Arc<dyn BlockDevice>) -> Result<(), ScanError> {
        let row = self.status(id)?;
        if row.state != ScanState::Paused {
            return Err(ScanError::TaskNotActive(id));
        }
        let fs = probe(&*device).map_err(|_| ScanError::UnsupportedFs)?;
        if row.scan_mode == "deep" {
            let runs = unallocated_runs_of(&*device, fs)?;
            let from = row.carved_offset.unwrap_or(0);
            self.store.set_state(id, ScanState::Scanning)?;
            self.spawn_carve(id, device, runs, from, row.found_count); // 不清 entries；idx 续号
        } else {
            self.store.clear_entries(id)?;
            self.store.set_state(id, ScanState::Scanning)?;
            self.spawn(id, device, ScanKind::Quick);
        }
        Ok(())
    }
```
`spawn_carve` 增参 `resume_from: u64, next_idx: u64`（首跑传 0/0；`run_carve_worker` 用 `carve_runs_from` 与 `found: next_idx` 起）。
测试（全用 SlowDev + 文件库）：
1. `deep_pause_restart_resumes_from_checkpoint`：慢速深扫 → paused（检查点已写 >0）→ drop manager → 新 manager 同库 → `recover_after_restart` → `resume` → `NeedsDevice` → `restart(id, dev)` → completed 后 `results` 含**首段已雕条目 + 续段条目**（总数 == 全量，含可能的 1 条残余重复）且无丢条。
2. `quick_restart_still_clears`：quick 语义不回归（既有测试复跑）。
3. `carved_offset_persisted_during_scan`：SlowDev 深扫进行中，暂停后读 `task()` 断言 `carved_offset > 0` 且 ≥ 已落库条目的 byte_offset。

- [ ] **Step 4: 门禁与提交**
```bash
cargo test --workspace --locked && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-carving crates/xd-core
git commit -m "feat(core): 深扫检查点与断点续跑（carved_offset/schema v4，重启续扫保留已雕结果）"
```

---

### Task 8: 端到端 + 恢复率回归门禁（设计 §8.2）

**Files:**
- Create: `crates/xd-carving/tests/carve_e2e.rs`（引擎级：多 run/跨 run 裁定/坏读）
- Modify: `crates/xd-daemon/tests/scan_ipc.rs`（daemon 级深扫全链路 + 重启续跑）

**恢复率门禁（本任务即 §8.2 的 v1 基线）：** 合成镜像埋 N 个文件（含真假签名搅局）→ **找回率必须 100%（N/N）且假阳性 0**；测试失败即门禁失败（任何 PR 弄低恢复率会红）。

- [ ] **Step 1: carve_e2e.rs（引擎级全代码要点）**
```rust
//! 引擎级端到端：真实 exfat 镜像 → unallocated_runs → carve_runs → 断言恢复率与假阳性。
#[test]
fn exfat_free_space_recovery_rate_is_100_percent_no_false_positives() {
    // 1) 构建 exfat（含一个删除的 JPG 走 build 路径）+ 在空闲簇绝对偏移用 plant_in_run 埋：
    //    mini_jpeg(20000)、TINY_PNG、decoy：FFD8FF+垃圾（无 SOS）、PNG magic+非 IHDR 块
    // 2) runs = xd_fs_exfat::freespace::unallocated_runs(&dev)
    // 3) entries = carve_runs 收集
    // 4) 断言：恰好 2 条；byte_offset 与埋点逐一相等；size 与源长逐一相等；
    //    decoy 无对应条目（假阳性 0）；且删除文件的簇区必被某 run 覆盖（雕刻可见性）
}

#[test]
fn jpeg_fragmented_across_runs_is_honestly_truncated() {
    // 把 mini_jpeg 的前半埋 run A 尾部、后半埋 run B（中间隔一个已分配簇）：
    // 裁定：只在 run A 内重组 → 1 条 complete=false 且 len == run A 内实际字节；
    // run B 里的后半段无签名 → 不上报（不跨洞缝合）
}
```
- [ ] **Step 2: scan_ipc.rs 深扫链路**
```rust
#[test]
fn deep_scan_over_ipc_with_checkpoint_restart() {
    // 大 exfat（80+ 文件撑大空区）+ 埋 2 个真文件 + decoy → daemon --db 文件库
    // scan.start mode=deep → 立即 pause（收到 ok 或 -32004 的竞态二态，同既有 pause 测试惯例）
    // paused 时：kill daemon（drop stdin/wait）→ 重启同 --db → scan.resume → 轮询 completed
    // 断言：results 含 2 条 carved（byteOffset 精确）；totalBytes == 重启前 status 的 totalBytes（runs 稳定）
    // 断言：重启前已落库的 carved 记录在重启后仍存在（检查点续跑不丢）
}
```
（确定性：sizes 大 + SlowDev 不可用于 daemon（真进程）——用"先 pause 再看"策略，若 pause 竞态输（-32004 completed）则本测试退化为纯全量断言并打 `eprintln!` 标注——**实施者若发现无法稳定复现，允许把检查点语义完全交给 T7 单测、本测试只钉 IPC 全链路**，但须在 commit/执行记录里说明降级。）

- [ ] **Step 3: 门禁与提交**
```bash
cargo test --workspace --locked && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates/xd-carving crates/xd-daemon
git commit -m "test(carving): 恢复率门禁（100%/-0假阳性）+ daemon 深扫全链路与检查点重启"
```

---

### Task 9: 出口验收

- [ ] **Step 1: 全量门禁（debug+release）+ flutter + 既有 e2e（同 M1b T9 Step 1 清单）**
- [ ] **Step 2: 变异抽检（kill 即通过；逐条结论进执行记录）**
1. `find_candidates` 的 JPEG 条件删一个字节（FFD8 仅两字节）→ decoy 假阳性测试 kill。
2. JPEG 熵段 `0xFF 0x00` 转义分支删 → stuffed 测试 kill。
3. JPEG "未过 SOS 即 EOI → None" 删 → `eoi_without_sos_is_rejected` kill。
4. PNG CRC 比较删（恒过）→ `bad_crc_truncates_at_chunk_start` kill。
5. PNG 首块 IHDR 检查删 → `first_chunk_not_ihdr_rejected` kill。
6. 扫描器签名推进 `abs + sig.len()` 改 `abs` → 死循环（测试超时）或 `signature_straddling` kill。
7. 检查点 `set_carved_offset` 删 → `carved_offset_persisted_during_scan` kill。
8. `restart` deep 分支的 `clear_entries` 误加 → 重启续跑丢条测试 kill。
9. `unallocated_runs` 的 `is_free Err → 视为已分配` 反转（Err 视为空闲）→ 位图不可读 Err 测试 kill（改法：把 `.ok_or_else` 删掉返回空 runs → `empty_volume` 与 bit 测试两向夹住）。
- [ ] **Step 3: 文档**：设计文档 §4.3 补 v1 裁定（run 内重组/multi-thread 延后/JPEG+PNG）；计划执行记录块；README 「深度扫描」功能行更新（M1 里程碑进度）。
- [ ] **Step 4: 合入 main + CI 逐 job 验证（`gh run view <id> --json jobs`）**；provenance 重生成与发版归用户触发的发版流程。

---

## 验收定义（M1c Done 的判据）

1. `xd-carving`：JPEG/PNG 结构走链重组，假阳性有判别测试，截断/上限/坏读全部诚实（不长不短不猜）。
2. 两引擎 `unallocated_runs` 语义一致（排序/不相交/拒绝未知分配），FAT 删除文件的空间可扫。
3. daemon 全链路：`scan.start {mode:"deep"}` → 真百分比进度（readBytes/totalBytes 对齐 Σruns）→ `scan.results` carved 条目（byteOffset 精确）；`-32005` 契约码。
4. 深扫暂停 → 杀进程 → 重启 → resume 从检查点续扫，已雕结果保留（至多一条断点残余重复，README 记为已知局限）。
5. 恢复率门禁测试入 CI：合成镜像 100% 找回 + 0 假阳性。
6. 全量门禁绿（debug+release+clippy+fmt+flutter+e2e.sh+e2e-loop.sh）。

---

© 2026 erik · https://erik.xyz · erik@erik.xyz
