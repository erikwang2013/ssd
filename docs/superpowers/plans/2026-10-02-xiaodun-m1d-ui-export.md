# 小盾 M1d：扫描/预览/恢复三页 + 恢复导出（M1 收口）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完成 M1 的端到端产品面——契约 v1.2（`fs.read` / `export.*` / 目标盘错误码）、后端补齐（分片读取含雕刻件回读、**导出落盘 + 目标盘三重校验 + root 降权子进程**）、Flutter 三页向导（扫描控制 / 结果浏览 / 预览）+ 恢复导出与报告 + 通知流路由 + EACCES 引导。

**Architecture:** Dart 层只做 UI 状态机与 transport（设计 §6 铁律：任何扫描/恢复逻辑不进 Dart）。恢复导出由 daemon 以**子进程**（`--export-worker`，stdin 收任务、stdout 回进度/条目）执行：**先开源设备 fd（此时仍是原权限）→ 目标盘校验 → root 模式下 `setresuid` 降到调用者 → 以普通身份写文件**——特权不落写到用户目录、子进程崩溃隔离于父。

**关键裁定：**
1. **目标盘"不落回源设备"校验**用内核事实：`st_dev(目标目录)` 与源块设备的 `st_rdev` 直接比较（root 降权前做）；**镜像文件源不做此校验**（目标是普通文件生态，写目标不碰镜像内容——但会在文档注明"恢复目标勿选镜像所在盘的满盘"）。
2. **大文件支持**：引擎新增 `read_file_range`（分片读取），预览/导出都不再整文件物化；`fs.read` 契约限单片 ≤1MiB，导出内部片 4MiB。
3. **`fs.read` 上限**：条目 `size_bytes > 64MiB` → `-32009`（UI 文案"文件过大，暂不支持预览"；**导出不受此限**——导出走流式）。
4. **导出报告**：完成通知带计数 + **降级/失败条目清单**（≤1000，截断置 `itemsTruncated`）；成功条目不回传（UI 从计数展示）——避免十万条通知爆流。
5. UI 文案铁律（(a) 裁定后的准确表述）：删除+连续（exFAT）= 规范保证；删除+非连续 = 按删除链、可能不完整；**不得对任何条目称"连续假设"**；雕刻件 = "仅雕刻 · 可能不完整"；VDL 未初始化区不预览不导出（引擎已保证）。
6. `file_selector`（官方）与 `rustix`（目标盘校验/降权）为本切片新增依赖。
7. **承接 M1b T1 发现⑤（已提前关闭）**：`open_with_sysfs` 的 `transport` 归类已在 M1b T1 终版（`3fa947f`）随 `sysfs_transport()` 完成并有测试；本切片只需在 UI 展示 transport 时**复核**一致性（先开行与枚举行同源），无实现工作。
8. **承接 M1c T7 风险记录（daemon 测试的时间窗口依赖）**：`scan_ipc.rs` 的 cancel/SIGKILL 中途测试依赖"扫描窗口足够长"——原靠每条目 fsync（3.5s），M1c T7 换 WAL 后窗口 ≈46-62ms（对一次 IPC 往返 ~590-900× 裕度，12 连跑稳定）。**若本切片或后续再加速落库/扫描（如批量导出、更快索引），这批测试的窗口假设会失效**——届时须改用显式闸门（suspend/hook）而非时长撑开。改动落库路径时**必须复跑 scan_ipc 12 连跑**。（spec-t7 曾观测到一次并发全量构建叠加下的单次红、随后 5 连跑全绿——正是该时长期望形状的实证。）
9. **ui 侧新契约面**（M1c 起）：`scan.status` 不暴露 `carvedOffset`（深扫断点直读库才可见）——若 UI 需要展示"续跑位置"，M1d 与后端协商加字段（契约 v1.2 增量的候选）。

**前置：** M1b（契约 v1/scan_task/store）、M1c（carving/深扫/检查点）已合入。

---

## 文件结构

```
crates/
├── xd-carving/src/{jpeg.rs, png.rs, lib.rs}     # 核心循环重构为 collector 形态 + read_back
├── xd-fs-fat/src/read.rs                        # +read_file_range
├── xd-fs-exfat/src/read.rs                      # +read_file_range
├── xd-core/src/fs_read.rs                       # 新增：三分支（live/删除/carved）分片读取
├── xd-core/src/export.rs                        # 新增：ExportManager（父进程侧）
├── xd-core/src/store.rs                         # +entry(task_id, idx)
├── xd-core/src/{api.rs, handlers.rs}            # v1.2 类型/路由/错误码
└── xd-daemon/src/{main.rs, export_worker.rs}    # --export-worker 子进程模式
proto/v1/                                        # README v1.2 + 5 个新 golden + 5 个错误 golden
ui/
├── lib/core_client/{core_client.dart, ipc_transport.dart, protocol.dart}
├── lib/features/{scan/, results/, preview/, recover/}
├── lib/main.dart                                # 向导路由
└── test/{protocol_v12_test.dart, scan_page_test.dart, results_page_test.dart, preview_page_test.dart, recover_page_test.dart, export_flow_integration_test.dart}
docs/security/linux-privilege-model.md           # 导出降权子进程模型
```

---

### Task 1: 契约 v1.2（fs.read / export.* / 五个错误码）

**Files:**
- Modify: `proto/v1/README.md`（"v1.2 增量"小节）
- Create: `proto/v1/examples/{fs_read.request.json, fs_read.response.json, export_start.request.json, export_start.response.json, export_progress.notification.json, export_finished.notification.json, export_cancel.request.json, export_cancel.response.json, error_target_on_source.response.json, error_target_not_writable.response.json, error_entry_not_found.response.json, error_entry_too_large.response.json, error_insufficient_space.response.json}`
- Modify: `crates/xd-core/src/api.rs`（params 结构体 + 5 个错误构造器）、`crates/xd-core/tests/contract_v1.rs`

**契约 v1.2 权威定义（README 与 golden 据此）：**

```jsonc
// 方法
fs.read   {taskId, idx, offset, length}          // length ∈ 1..=1048576
          → {bytesBase64, eof}                    // eof = 已交付到该条目可得数据的末端（损坏件可能 < sizeBytes）
export.start {taskId, idxs:[...], targetDir}      // idxs 非空、≤100000；targetDir 绝对路径
          → {exportId, fileCount, estimatedBytes} // estimatedBytes = Σ size_bytes（上界；降级件实际可能更短）
export.cancel {exportId} → {exportId, state:"canceled"|"completed"}  // 终态幂等原样返回
// 通知
export.progress {exportId, done, total, writtenBytes, elapsedMs}   // 节流同 scan.progress
export.finished {exportId, succeeded, degraded, failed, canceled, targetDir, items, itemsTruncated}
//   items: [{idx, name, status:"degraded"|"failed", reason}]（仅降级/失败，≤1000 条；超限截断置 true）
// 错误
-32006 TargetOnSourceDevice  "Target is on the source device: <dir>"
-32007 TargetNotWritable     "Target not writable: <dir>"
-32008 EntryNotFound         "Entry not found: <idx>"
-32009 EntryTooLarge         "Entry too large: <sizeBytes>"
-32010 InsufficientSpace     "Insufficient space on target: need <n> bytes"
```

- [ ] **Step 1: golden（逐字写入）**
```bash
mkdir -p proto/v1/examples
cat > proto/v1/examples/fs_read.request.json <<'EOF'
{"jsonrpc":"2.0","id":21,"method":"fs.read","params":{"taskId":1,"idx":0,"offset":0,"length":16}}
EOF
cat > proto/v1/examples/fs_read.response.json <<'EOF'
{"jsonrpc":"2.0","id":21,"result":{"bytesBase64":"aGVsbG8sIHhpYW9kdW4h","eof":true}}
EOF
cat > proto/v1/examples/export_start.request.json <<'EOF'
{"jsonrpc":"2.0","id":22,"method":"export.start","params":{"taskId":1,"idxs":[0,1],"targetDir":"/home/user/Recovered"}}
EOF
cat > proto/v1/examples/export_start.response.json <<'EOF'
{"jsonrpc":"2.0","id":22,"result":{"exportId":1,"fileCount":2,"estimatedBytes":16007}}
EOF
cat > proto/v1/examples/export_progress.notification.json <<'EOF'
{"jsonrpc":"2.0","method":"export.progress","params":{"exportId":1,"done":1,"total":2,"writtenBytes":12000,"elapsedMs":300}}
EOF
cat > proto/v1/examples/export_finished.notification.json <<'EOF'
{"jsonrpc":"2.0","method":"export.finished","params":{"exportId":1,"succeeded":1,"degraded":1,"failed":0,"canceled":false,"targetDir":"/home/user/Recovered","items":[{"idx":0,"name":"IMG_0001.JPG","status":"degraded","reason":"short read"}],"itemsTruncated":false}}
EOF
cat > proto/v1/examples/export_cancel.request.json <<'EOF'
{"jsonrpc":"2.0","id":23,"method":"export.cancel","params":{"exportId":1}}
EOF
cat > proto/v1/examples/export_cancel.response.json <<'EOF'
{"jsonrpc":"2.0","id":23,"result":{"exportId":1,"state":"canceled"}}
EOF
cat > proto/v1/examples/error_target_on_source.response.json <<'EOF'
{"jsonrpc":"2.0","id":22,"error":{"code":-32006,"message":"Target is on the source device: /mnt/usb/Recovered"}}
EOF
cat > proto/v1/examples/error_target_not_writable.response.json <<'EOF'
{"jsonrpc":"2.0","id":22,"error":{"code":-32007,"message":"Target not writable: /root/nope"}}
EOF
cat > proto/v1/examples/error_entry_not_found.response.json <<'EOF'
{"jsonrpc":"2.0","id":21,"error":{"code":-32008,"message":"Entry not found: 999"}}
EOF
cat > proto/v1/examples/error_entry_too_large.response.json <<'EOF'
{"jsonrpc":"2.0","id":21,"error":{"code":-32009,"message":"Entry too large: 1073741824"}}
EOF
cat > proto/v1/examples/error_insufficient_space.response.json <<'EOF'
{"jsonrpc":"2.0","id":22,"error":{"code":-32010,"message":"Insufficient space on target: need 16007 bytes"}}
EOF
```
（`fs_read.response` 的 `bytesBase64` 是 `hello, xiaodun!`（16 字节）的 base64——golden 契约里 `eof:true` 表示 16 字节已到条目尾。）

- [ ] **Step 2: api.rs 类型与错误构造器**

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsReadParams {
    pub task_id: u64,
    pub idx: u64,
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStartParams {
    pub task_id: u64,
    pub idxs: Vec<u64>,
    pub target_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportIdParams {
    pub export_id: u64,
}
```
`RpcError` 追加（消息文案即契约）：
```rust
    pub fn target_on_source(dir: &str) -> Self   { /* -32006 "Target is on the source device: {dir}" */ }
    pub fn target_not_writable(dir: &str) -> Self{ /* -32007 "Target not writable: {dir}" */ }
    pub fn entry_not_found(idx: u64) -> Self     { /* -32008 "Entry not found: {idx}" */ }
    pub fn entry_too_large(size: u64) -> Self    { /* -32009 "Entry too large: {size}" */ }
    pub fn insufficient_space(need: u64) -> Self { /* -32010 "Insufficient space on target: need {need} bytes" */ }
```

- [ ] **Step 3: contract_v1.rs 断言扩展**：13 个新 golden 的 envelope 解码 + params 强类型解码（`FsReadParams`/`ExportStartParams`/`ExportIdParams` 逐字段）+ 5 个错误 golden 文案逐字。

- [ ] **Step 4: 门禁与提交**
```bash
cargo test -p xd-core --locked && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A proto/v1 crates/xd-core
git commit -m "feat(proto): v1.2 契约——fs.read/export.* 与五个目标盘/条目错误码（13 golden）"
```

---

### Task 2: 分片读取（引擎 read_file_range + 雕刻回读 + xd-core::fs_read）

**Files:**
- Modify: `crates/xd-fs-fat/src/read.rs`（+`read_file_range`）
- Modify: `crates/xd-fs-exfat/src/read.rs`（+`read_file_range`）
- Modify: `crates/xd-carving/src/{jpeg.rs, png.rs, lib.rs}`（collector 化 + `read_back`）
- Create: `crates/xd-core/src/fs_read.rs`（三分支路由；+lib.rs 注册）
- Modify: `crates/xd-core/Cargo.toml`（+`base64`（workspace 定版：`cargo add base64 -p xd-core`）、+`xd-carving` path）

**语义（三引擎一致）：** `read_file_range(dev, entry, offset, length)` 返回**从 offset 起的至多 length 字节**（越尾短交付=诚实；offset ≥ 可得长度 → 空）；绝不物化整文件；内部按簇流式。

- [ ] **Step 1: exfat `read_file_range`（全代码；fat 侧同构，契约测试同款）**

```rust
/// 分片读取：交付 `[offset, offset+length)` ∩ `[0, min(VDL,DL))` 的字节（流式，绝不物化整文件）。
/// 拓扑与 `read_file` 完全同源（live 链式只信链；删除+连续=规范保证；删除+非连续=只沿 stale 链）。
pub fn read_file_range(
    dev: &dyn BlockDevice,
    entry: &ExfatEntry,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, ExfatError> {
    let size = entry.size_bytes.min(entry.data_length); // 交付上界（T4 保证 VDL ≤ DL；直构也不越 DL）
    if offset >= size || length == 0 || entry.first_cluster < 2 {
        return Ok(Vec::new());
    }
    let take = length.min(size - offset);
    let boot = boot::parse(dev)?;
    let fat = Fat32::new(dev, &boot);
    let cb = boot.cluster_bytes();
    let bitmap = if entry.deleted {
        load_bitmap(dev, &boot, &fat)
    } else {
        None
    };
    // 簇序列（与 read_file 同裁定）：
    let clusters: Vec<u32> = if entry.deleted && !entry.contiguous {
        let chain = fat.chain(entry.first_cluster).unwrap_or_default();
        let n = entry.data_length.div_ceil(cb).min(chain.len() as u64) as usize;
        chain[..n].to_vec()
    } else if !entry.deleted && !entry.contiguous {
        let chain = fat.chain(entry.first_cluster).unwrap_or_default();
        let n = entry.data_length.div_ceil(cb).min(chain.len() as u64) as usize;
        chain[..n].to_vec()
    } else {
        let need = entry.data_length.div_ceil(cb);
        match resolve_clusters(&boot, &fat, entry.first_cluster, need, entry.contiguous) {
            Some(Resolved::Contiguous { first, n }) => {
                (0..n).map(|i| (first as u64 + i) as u32).collect()
            }
            Some(Resolved::Chain(c)) => c,
            None => return Ok(Vec::new()),
        }
    };
    // 跳过 offset 之前的整簇，簇内偏移用首簇截断读
    let skip_bytes = offset;
    let mut out = Vec::with_capacity(take.min(4 * 1024 * 1024) as usize);
    let mut produced: u64 = 0; // 对应 clusters 从头累计的逻辑字节数
    let mut buf = vec![0u8; cb as usize];
    for c in clusters {
        if produced >= offset + take {
            break;
        }
        if let Some(b) = bitmap.as_ref()
            && !matches!(b.is_free(c), Ok(true))
        {
            break; // 删除项被占用簇即止（与 read_file 同）
        }
        let n = match dev.read_at(boot.cluster_to_byte(c), &mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        let chunk_start = produced;
        let chunk_end = produced + n as u64;
        produced = chunk_end;
        if chunk_end <= skip_bytes {
            continue;
        }
        let from = skip_bytes.saturating_sub(chunk_start) as usize;
        let to = buf
            .len()
            .min(((offset + take).saturating_sub(chunk_start)) as usize);
        if from < to.min(n) {
            out.extend_from_slice(&buf[from..to.min(n)]);
        }
        if n < buf.len() {
            break;
        }
    }
    Ok(out)
}
```
测试（exfat，全代码语义；fat 同构）：
```rust
    #[test]
    fn ranged_read_matches_full_read_slices() {
        // 三种拓扑 × 五组 (offset,len)：range 结果 == read_file 的对应切片
        // 构型：live 连续(V.BIN 9000)、live 链式(F.BIN [7,6,8])、删除连续(G.BIN)、删除 stale 链(G.BIN chained)
        let cases: [(u64, u64); 5] = [(0, 10), (4090, 20), (4096, 1), (8192, 9000), (8999, 2)];
        for (off, len) in cases { ... assert_eq!(range, &full[off.min(full.len() as u64) as usize..(off+len).min(full.len() as u64) as usize]) }
    }

    #[test]
    fn ranged_read_offset_beyond_end_is_empty() { ... }

    #[test]
    fn ranged_read_deleted_stale_chain_stops_at_occupied() {
        // 删除+非连续+簇被复用：range 读取与 read_file 同样在占用簇处截断（跨 offset 也一样）
    }
```

- [ ] **Step 2: xd-carving collector 化 + `read_back`**

重构 `jpeg.rs`/`png.rs`：抽出收集版核心，两形态共用（**不得复制走链逻辑**）：
```rust
/// 核心：从游标走链，`sink` 收每段字节；返回 (总长, 是否完整)。
/// `carve_jpeg` = sink 丢弃；`collect_jpeg` = sink 攒入 Vec（上限 `take`）。
fn walk_jpeg(cur: &mut Cursor<'_>, max_len: u64, sink: &mut dyn FnMut(&[u8])) -> Option<Carved>
pub fn collect_jpeg(cur: &mut Cursor<'_>, max_len: u64) -> Option<(Vec<u8>, bool)>
```
（PNG 同构，`Crc32` 不受 sink 影响。）lib.rs 新增：
```rust
/// 雕刻件回读：从 `byte_offset` 在原 run 内重走到 `need` 字节（重走是确定性的——同样的设备、
/// 同样的规则；run 界由调用方用 `unallocated_runs` 重新定位）。
pub fn read_back(
    dev: &dyn BlockDevice,
    run_end: u64,
    byte_offset: u64,
    kind: Signature,
    need: u64,
) -> Option<Vec<u8>>
```
其中 `kind` 由 `ext` 反推（"jpg"→Jpeg/"png"→Png；lib.rs 提供 `Signature::from_ext`）。测试：`read_back_slices_match_original`（plant → carve → 对 carved 记录 read_back 全量/切片 == 原字节；含 truncated 件）。**run_end 的重新定位在 xd-core::fs_read**：`xd_fs_*::freespace::unallocated_runs` → 找包含 `byte_offset` 的 run → 取其 end。

- [ ] **Step 3: xd-core::fs_read（全代码）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! 条目分片读取（预览/导出共用）：live/删除 → 引擎 `read_file_range`；雕刻 → carving 回读。
//! 契约上限 `MAX_READ = 1MiB` 由 handlers 校验；内部调用方可放宽（导出 4MiB 片）。

use std::ops::Range;
use base64::Engine as _;
use xd_device::BlockDevice;

use crate::api::ScanEntry;
use crate::scan_task::FsKind;

pub const MAX_READ: u64 = 1024 * 1024;

pub enum ReadError {
    TooLarge(u64),
    Internal(String),
}

/// 读取 `[offset, offset+length)`；返回 (bytes, eof)。
/// eof = 交付已到该条目**可得数据的末端**（损坏/短链件可能 < sizeBytes——UI 以此判"可能不完整"）。
pub fn read_entry_range(
    dev: &dyn BlockDevice,
    fs: FsKind,
    entry: &ScanEntry,
    offset: u64,
    length: u64,
) -> Result<(Vec<u8>, bool), ReadError> {
    let bytes = match entry.byte_offset {
        Some(bo) => {
            let runs = unallocated_runs(dev, fs).map_err(|e| ReadError::Internal(e.to_string()))?;
            let run = runs
                .iter()
                .find(|r: &&Range<u64>| r.contains(&bo))
                .ok_or_else(|| ReadError::Internal("carved offset outside free space".into()))?;
            let kind = xd_carving::Signature::from_ext(&entry.ext)
                .ok_or_else(|| ReadError::Internal("unknown carved ext".into()))?;
            xd_carving::read_back(dev, run.end, bo, kind, offset + length)
                .map(|all| {
                    let s = (offset as usize).min(all.len());
                    let e = ((offset + length) as usize).min(all.len());
                    all[s..e].to_vec()
                })
                .unwrap_or_default()
        }
        None => {
            let want = offset + length; // 引擎内自行 clamp；大文件分片不物化
            match fs {
                FsKind::Fat => {
                    let e = to_fat_entry(entry);
                    xd_fs_fat::read::read_file_range(dev, &e, offset, length)
                        .map_err(|e| ReadError::Internal(e.to_string()))?
                }
                FsKind::Exfat => {
                    let e = to_exfat_entry(entry);
                    xd_fs_exfat::read::read_file_range(dev, &e, offset, length)
                        .map_err(|e| ReadError::Internal(e.to_string()))?
                }
            }
            .into_iter()
            .take(length as usize)
            .collect()
        }
    };
    let end = offset + bytes.len() as u64;
    let eof = bytes.len() as u64 < length || end >= entry.size_bytes;
    Ok((bytes, eof))
}

pub fn to_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
```
`to_fat_entry`/`to_exfat_entry`：从 `ScanEntry` 反构造引擎条目（字段齐全：fat 需 name/path/size_bytes/first_cluster/deleted/is_dir/quality/ext；exfat 需 + data_length/contiguous——**ScanEntry 缺 data_length/contiguous！** 反构造只能 `data_length = size_bytes, contiguous = ???`——拓扑信息在 ScanEntry 里丢了！
**裁定**：`ScanEntry` 增 `contiguous: Option<bool>`（v1.2 增量，skip_serializing_if，与 byteOffset 同款）——雕刻/无意义件为 None；quick 扫描落库时写实值。`data_length` 不进契约（读取用 `size_bytes` 为上界；exfat 交付上界 = min(VDL,DL)= size_bytes 本来就是交付长度 ✓ `read_file_range` 用 `entry.size_bytes` 作 size、`data_length` 仅参与簇数计算——反构造时 `data_length = size_bytes` 会把簇数算少吗？need = dl.div_ceil(cb)；dl=VDL≤DL 时 need 可能小于真实 need（尾部 [VDL,DL) 不交付 ✓ 无碍：交付长度按 size，簇数只影响上界请求，少了 1 簇只可能少读"永不交付"的尾部 ✓ 安全）。
**但 contiguous 必须真值**（拓扑裁定开关）→ ScanEntry.contiguous 必要性成立，M1b 的映射函数补写、store 补列（schema v5！）/读写/测试更新。

- [ ] **Step 4: store schema v5 + 映射更新**
`entries` 加列 `contiguous INTEGER`（可空；迁移模版同前，user_version=5）。`ScanEntry` 加字段 `#[serde(default, skip_serializing_if="Option::is_none")] pub contiguous: Option<bool>`；M1b 的 `fat_to_entry`/`exfat_to_entry` 填 `Some(e.contiguous)`（fat 无 contiguous 概念 → **fat 恒 Some(true)?** 不——fat 的读取拓扑由 deleted 决定，ScanEntry.contiguous 对 fat 无意义 → fat 填 `None`（即"无此概念"，读取时 fat 分支不用它）✓ 语义记入 README 字段注释）；carved 映射填 None。既有 golden 不受影响（缺省省略）✓。store 读写列；测试：contiguous roundtrip（Some(true)/Some(false)/None 三态）。

- [ ] **Step 5: fs_read 测试（xd-core，全代码要点）**
1. `reads_live_and_deleted_via_engine`：exfat 夹具（live A.TXT + 删除 DEL_ME.JPG）→ 建 task/entries（走真实 scan：`ScanManager` completed 后取条目）→ `read_entry_range` 切片 == 已知字节（小文件整读 + 中段切片 + offset 越尾空 + eof 语义逐一）。
2. `reads_carved_entry_back`：T6 深扫夹具（mini_jpeg 埋点）完成深扫 → carved 条目 → 全量回读 == 原 mini_jpeg 字节。
3. `eof_false_mid_file_true_at_end`。
4. `unknown_ext_carved_errors`（反向构造 ext="xyz" 的 carved 条目 → Internal）。

- [ ] **Step 6: 门禁与提交**
```bash
cargo test --workspace --locked && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates Cargo.toml Cargo.lock
git commit -m "feat(core): 分片读取全链（引擎 read_file_range/carving 回读/fs_read 路由，schema v5 contiguous）"
```

---

### Task 3: 恢复导出（父侧 ExportManager + `--export-worker` 降权子进程）

**Files:**
- Create: `crates/xd-core/src/export.rs`（+lib.rs 注册；Cargo.toml +`rustix`（features `["fs","process"]`；`cargo add` 定版））
- Create: `crates/xd-daemon/src/export_worker.rs`（+main.rs 分派 `--export-worker`）
- Modify: `crates/xd-core/src/{store.rs（+`entry(task,idx)`、+`open_read_only`）, handlers.rs（export.start/cancel 路由 + fs.read 路由）}`
- Modify: `docs/security/linux-privilege-model.md`（导出降权模型节）
- Create: `crates/xd-daemon/tests/export_ipc.rs`

**降权子进程模型（写入 security 文档）：** 父（可能是 root）`spawn 自身 --export-worker` → 子**先按父权限开源设备 fd/做目标盘校验** → root 且 `PKEXEC_UID` 存在时 `setresuid(PKEXEC_UID)`（gid 从 /etc/passwd 尽力解析；解析不到只降 uid 并 stderr 留痕）→ **此后所有文件写入均以普通用户身份**。父只转发子 stdout 的 JSON 行；子崩溃/被杀 = 导出终止，已写文件保留。取消 = SIGTERM 子进程。

- [ ] **Step 1: 父侧 export.rs（关键全代码）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! 恢复导出（父进程侧）：校验 → 起 `--export-worker` 子进程 → 转发进度/条目/终报。
//! 目标盘三重校验（存在/异设备/余量）在父侧先做（同步错误码），子在降权前复核（权威）。

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::api::ScanEntry;
use crate::notify::notification;
use crate::scan_task::NotifyFn;
use crate::store::Store;

pub const MAX_IDXS: usize = 100_000;
pub const MAX_REPORT_ITEMS: usize = 1000;

#[derive(Debug)]
pub enum ExportError {
    TaskNotFound(u64),
    EntryNotFound(u64),
    NoEntries,
    TargetOnSource(String),
    TargetNotWritable(String),
    InsufficientSpace(u64),
    Internal(String),
}

pub struct ExportStarted {
    pub export_id: u64,
    pub file_count: u64,
    pub estimated_bytes: u64,
}

struct Job {
    canceled: Arc<AtomicBool>,
    child: Mutex<Option<Child>>,
}

pub struct ExportManager {
    store: Arc<Store>,
    notify: NotifyFn,
    jobs: Mutex<HashMap<u64, Job>>,
    next_id: AtomicU64,
}

impl ExportManager {
    pub fn new(store: Arc<Store>, notify: NotifyFn) -> Self { ... }

    /// `source_rdev`: 源为物理块设备时 `Some((major, minor))`（镜像源为 None，不做同盘校验——
    /// 写目标是文件生态，不触碰镜像内容）。
    pub fn start(
        &self,
        task_id: u64,
        idxs: &[u64],
        target_dir: &str,
        source_rdev: Option<(u64, u64)>,
        db_path: Option<&std::path::Path>,
        export_id_next: &AtomicU64, // 由 CoreCtx 持有（daemon 单例计数器放 manager 内即可——实现放 self.next_id）
    ) -> Result<ExportStarted, ExportError> { ... }
}
```
实现要点（计划即规范，实施者按此补全函数体与 `#[cfg(unix)]` 界限）：
1. `idxs` 去重非空、≤ MAX_IDXS，否则 `NoEntries`/`Internal`（handlers 侧把空/超限映射为 -32602）。
2. 逐 idx `store.entry(task_id, idx)` → 缺任一 → `EntryNotFound`；`estimated = Σ size_bytes`。
3. 目标校验（unix）：
   - `PathBuf::from(target_dir)`：不存在/非目录 → `TargetNotWritable(dir)`。
   - `source_rdev = Some(rdev)` 时：`rustix::fs::stat(target)` 的 `st_dev`（拆 major/minor）== rdev → `TargetOnSource(dir)`。
   - `rustix::fs::statvfs(target)` → `f_bavail * f_frsize < estimated` → `InsufficientSpace(estimated)`。
4. 起子进程：`Command::new(std::env::current_exe()?)` `.arg("--export-worker").arg("--db").arg(db_path).arg("--task").arg(task_id.to_string()).arg("--export-id").arg(id).arg("--target").arg(target_dir)`，`pkexec_uid` 时 `.env("PKEXEC_UID", uid)`（继承即可——子自行读）；stdin piped（写 idxs JSON 数组后 `drop(stdin)`），stdout piped，stderr 继承（留痕）。
5. 转发线程：`BufReader::new(child.stdout).lines()` → 按 `type` 分派：
   - `"progress"` → 节流 ≥250ms 转发 `export.progress`（字段透传）；
   - `"item"` → 收进 `items`（≤MAX_REPORT_ITEMS，超出置 `items_truncated`）；
   - `"fatal"` → 记 reason；子 exit 后终报 `failed = total - (succeeded + degraded)`。
6. 子退出后（或收到 `"finished"`）发 `export.finished`（含 canceled 标志）；`jobs` 移除。
7. `pub fn cancel(&self, export_id) -> Result<&'static str /*state*/, ExportError>`：置 canceled → `child.kill()`；未知 id → 借用 `TaskNotFound(export_id)`? 用独立 `ExportNotFound`？——**裁定：未知 exportId → -32003 复用"Task not found"不合适**；新增无？——为省码位：未知 exportId 在 handlers 映射为 -32008 `EntryNotFound`？也不合适。**最终**：`export.cancel` 未知 id → `-32602`（参数指向不存在的运行中导出，重试语义即"已完成/不存在"）；已终态 → 幂等 `{"state":"completed"}`；运行中 → kill → `{"state":"canceled"}`。记入 README。

- [ ] **Step 2: 子进程 export_worker.rs（关键全代码）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! `--export-worker`：导出执行体。**权限序**：开源设备（父权限）→ 校验 → root 降权 → 写文件。
//! stdout 只出 JSON 行（progress/item/fatal/finished），stderr 留痕；退出码 0=正常（含逐件失败），2=致命。

pub fn run(args: ExportArgs) -> i32 {
    let store = match Store::open_read_only(&args.db) { ... };
    let row = store.task(args.task)?; // 取 device_id/fs
    let dev = open_device_by_id(&row.device_id)?;           // ① 父权限
    let fs = FsKind::from_str(&row.fs)?;
    let entries: Vec<ScanEntry> = /* stdin 读 idxs → store.entry 逐取 */;
    check_target(&dev, &row, &args.target, entries sum)?;   // ② 复核（同父侧三重）
    if effective_uid()? == 0 && let Some(uid) = pkexec_uid() { drop_to_user(uid)?; } // ③ 降权
    let mut out = std::io::stdout().lock();
    let mut ok = 0u64; let mut degraded = 0u64; let mut failed = 0u64; let mut written = 0u64;
    let mut used: HashMap<String, u32> = HashMap::new();
    for e in &entries {
        let name = unique_name(&mut used, file_name_for(e));
        let path = PathBuf::from(&args.target).join(name);
        match export_one(&dev, fs, e, &path) {
            Ok(w) if w == e.size_bytes => ok += 1,
            Ok(w) => { degraded += 1; item_line(&mut out, e, "degraded", "short read"); }
            Err(reason) => { failed += 1; item_line(&mut out, e, "failed", &reason); }
        }
        written += ...; progress_line(&mut out, ...);
        if parent_died(&mut out) { /* stdout EPIPE → 父没了：退出 */ return 0; }
    }
    finished_line(&mut out, ok, degraded, failed, false);
    0
}
```
`export_one`：`create` 文件（`OpenOptions::new().write(true).create_new(true)`，name 已去重）→ 4MiB 片循环 `xd_core::fs_read::read_entry_range(dev, fs, e, off, 4MiB)` 写盘 → 短交付（`eof && 已写 < size_bytes`）→ `Ok(已写)`（degraded）；读错误 → `Err`。**VDL/损坏语义直接来自 read_entry_range 的 eof** ✓。`file_name_for`：名字空（雕刻）→ `carved_{idx:06}.{ext}`；否则 `sanitize(name)`（拒绝 `/`、`\`、`..`、控制字符 → 替换 `_`）。`drop_to_user`：`rustix::process::setresuid(uid,uid,uid)` + `/etc/passwd` 解析 gid `setresgid`（解析不到则只降 uid + stderr 警告）。
`Store::open_read_only`：`Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)`。
`open_device_by_id`：`image:` → `ImageFileDevice::open`；`unix:` → `LinuxBlockDevice::open`（cfg linux）——**注意此路径不经 RPC，设备 id 来自自家 store，无越权面**（写进函数头注）。

- [ ] **Step 3: handlers 路由**
- `"fs.read"`：params 校验（length 1..=MAX_READ 否则 -32602）→ 任务行 → `entry = store.entry` 缺 → -32008 → size > 64MiB → -32009 → resolve_device(row.device_id)（复用懒打开；EACCES → -32001）→ `read_entry_range` → `{bytesBase64, eof}`。
- `"export.start"`：idxs 空/超限 → -32602；任务/条目校验 → 错误映射（-32006/-32007/-32010/-32008）；成功 → `{exportId, fileCount, estimatedBytes}`。**source_rdev** 由 `resolve_device` 返回的设备算出：给 `BlockDevice` 增**默认方法** `fn source_rdev(&self) -> Option<(u64, u64)> { None }`（xd-device trait 加默认实现；LinuxBlockDevice 覆写为 stat /dev 节点）——**不动既有实现**（默认 None ✓ 铁律不破）。
- `"export.cancel"` → 状态机见 Step 1 第 7 条。
- CoreCtx 增 `exports: Arc<ExportManager>`（`with_scan` 一并注入；`CoreCtx::new` 默认内存库 + no-op notify）。

- [ ] **Step 4: export_ipc.rs 集成测试（daemon 全代码要点）**
1. `exports_all_bytes_exactly`：exfat 夹具（2 文件）→ scan → export.start 到 tempdir → 轮询 export.finished → 断言 `succeeded==2`；**逐字节比对**两文件内容 == 原始数据；文件名 == 原名。
2. `degraded_reported_for_damaged_deleted`：删除文件 + 篡改位图（簇被复用）→ 导出 → `degraded==1` + item.reason=="short read" + 写出的文件长度 == 实交付长度（短于 sizeBytes）。
3. `target_checks`：不存在目录 → -32007；余量不足（targetDir 指向 tmpfs 小盘？改用 estimated 巨大夹具不便——**用 `targetDir` 指向 1KiB 的 tmpfs？CI 无权限** → 改为：mock 不可行，**校验函数单测**（xd-core::export 内 `#[cfg(test)]` 对 `check_*` 纯函数注入 statvfs/rdev 假值）覆盖 -32010 与 -32006；集成只测 -32007 与 happy path。（**-32006 的真集成留给 e2e-loop.sh：环回设备挂载后导出到挂载点 → -32006**，见 T9。）
4. `cancel_stops_export`：大夹具（十几 MiB）→ export.start → 立即 cancel → 终报 canceled==true（允许 `{canceled, 已完成}` 二态的竞态容忍写法，同 pause 先例）。

- [ ] **Step 5: 门禁与提交**
```bash
cargo test --workspace --locked && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo fmt --check
git add -A crates docs/security Cargo.toml Cargo.lock
git commit -m "feat(export): 恢复导出——父侧校验/转发 + --export-worker 降权子进程（逐件报告/取消/三重目标校验）"
```

---

### Task 4: Dart 传输层 v1.2（参数化调用 + 通知流 + pkexec 启动）

> **T1 移交（spec-m1d-t1 观察 C + qual 细化）**：README 声明「两侧逐字断言」，但新 13 golden 目前 Dart 侧仅集合测试（零值级）。`protocol_v12_test.dart` 需补**四类**：(i) 五码 -32006..-32010 的 `expectRpcError` 逐字（与既有旧错误页同款）；(ii) `fs_read.response` 的 `bytesBase64`/`eof` 值级解码；(iii) `export_start`/`export_cancel` response 字段；(iv) 两个 export 通知的无 id 信封 + params 形状。

**Files:**
- Modify: `ui/lib/core_client/{protocol.dart, core_client.dart, ipc_transport.dart}`
- Create: `ui/test/fake_core_client.dart`（测试公用 Fake，实现全部接口）
- Modify: `ui/test/{protocol_v1_test.dart（如存在则并入 protocol_v12_test.dart 新文件）, ipc_integration_test.dart, home_page_test.dart}`
- Create: `ui/test/protocol_v12_test.dart`

- [ ] **Step 1: protocol.dart —— v1.2 模型（全代码要点）**
```dart
class ScanEntry {
  const ScanEntry({required this.idx, required this.name, required this.path, required this.ext,
    required this.sizeBytes, required this.deleted, required this.isDir, required this.quality,
    required this.firstCluster, this.byteOffset, this.contiguous});
  final int idx; final String name; final String path; final String ext;
  final int sizeBytes; final bool deleted; final bool isDir;
  final String quality; // complete | maybeDamaged | carved
  final int firstCluster; final int? byteOffset; final bool? contiguous;
  factory ScanEntry.fromJson(Map<String, dynamic> j) => ...;
  String get displayName => name.isEmpty ? 'carved_${idx.toString().padLeft(6, '0')}.$ext' : name;
}

class ScanStartResult { final int taskId; final String fs; final int totalBytes; }
class ScanStatusResult { final int taskId; final String state; final int readBytes; final int foundCount; final int elapsedMs; }
class ScanResultsPage { final int total; final List<ScanEntry> entries; }
class FsReadResult { final Uint8List bytes; final bool eof; } // base64Decode
class ExportStartResult { final int exportId; final int fileCount; final int estimatedBytes; }
class ExportReportItem { final int idx; final String name; final String status; final String? reason; }
class ExportFinished { final int exportId; final int succeeded; final int degraded; final int failed;
  final bool canceled; final String targetDir; final List<ExportReportItem> items; final bool itemsTruncated; }
```

- [ ] **Step 2: core_client.dart 抽象扩展（全代码）**
```dart
abstract class CoreClient {
  Future<PingResult> ping();
  Future<List<DeviceInfo>> listDevices();
  Future<ScanStartResult> scanStart(String device, {String mode = 'quick'});
  Future<ScanStatusResult> scanStatus(int taskId);
  Future<ScanResultsPage> scanResults(int taskId, {int offset = 0, int limit = 200, bool deletedOnly = false});
  Future<void> scanPause(int taskId);
  Future<void> scanResume(int taskId);
  Future<void> scanCancel(int taskId);
  Future<FsReadResult> fsRead(int taskId, int idx, {int offset = 0, int length = 1048576});
  Future<ExportStartResult> exportStart(int taskId, List<int> idxs, String targetDir);
  Future<void> exportCancel(int exportId);
  /// 服务端通知（无 id 行）：scan.progress/scan.finished/export.progress/export.finished。
  Stream<Map<String, dynamic>> get notifications;
  Future<void> close();
}
```

- [ ] **Step 3: ipc_transport.dart —— 参数化 `_call` + 通知流 + pkexec（全代码要点）**
```dart
  final StreamController<Map<String, dynamic>> _notifications =
      StreamController<Map<String, dynamic>>.broadcast();
  @override
  Stream<Map<String, dynamic>> get notifications => _notifications.stream;

  Future<Map<String, dynamic>> _call(String method, [Object? params]) { ... encodeRequest(id, method, params: params) ... }

  void _onLine(String line) {
    ... jsonDecode ...
    final id = message['id'];
    if (id is! int) { _notifications.add(message); return; }   // 无 id = 通知（契约）
    ...
  }

  /// pkexec 启动（EACCES 引导路径）：polkit 弹窗认证后以 root 拉起同参数 daemon。
  /// 未验证（需真机 polkit + 安装后的 policy 文件）；开发树直连路径不受影响。
  static Future<IpcCoreClient> startPrivileged({
    required String daemonPath, List<String> extraArgs = const [],
  }) async {
    final process = await Process.start('pkexec', [daemonPath, ...extraArgs]);
    return IpcCoreClient._(process);
  }
```
`close()` 增 `_notifications.close()`。
既有 `_call('ping')` / `_call('device.list')` 调用点适配新签名（`params: null` → 省略）。

- [ ] **Step 4: fake_core_client.dart（测试公用）**：内存态实现：可注入 fixtures（devices/entries/progress 脚本）、记录调用序列、可手动 `emitNotification(...)`；`exportStart` 返回可配置结果。既有 home_page_test 换用 Fake（替代内联假客户端）。

- [ ] **Step 5: protocol_v12_test.dart**：解码 M1d 全部 golden（typed 模型逐字段）；`displayName` 对雕刻件命名；`FsReadResult` base64 解码 == `hello, xiaodun!`。`ipc_integration_test.dart` 增：`scan_*`/`fs.read` 真 daemon 往返（沿用其既有的 XD_DAEMON_BIN 守卫模式）。

- [ ] **Step 6: 门禁与提交**
```bash
cd ui && /home/erik/flutter/bin/flutter test --no-pub && /home/erik/flutter/bin/flutter analyze
git add -A ui
git commit -m "feat(ui): 传输层 v1.2——参数化调用/通知流/pkexec 启动 + v1.2 模型与 Fake"
```

---

### Task 5: 扫描页（模式选择 / 真实进度 / 暂停恢复取消 / EACCES 引导）

> **T4 移交（qual-m1d-t4 错误语义实测）**：transport 文案保持诊断原样；**展示层映射**：`on StateError` → 「核心服务已退出，请重启应用」（-15 是用户在途退出的正常路径，直接 `'$e'` 会渲染 `Bad state: daemon exited with code -15`）；`TimeoutException` → 「核心服务无响应」（close 后的新调用是挂 10s 超时，非 StateError）；其余沿用现有文案。另 `"id":null` 应答行会被当通知入流（契约片段如此）→ **分发器必须容忍 `method==null`**。

**Files:**
- Create: `ui/lib/features/scan/scan_page.dart`（+`scan_controller.dart`）
- Modify: `ui/lib/home_page.dart`（设备项 onTap → 进入 ScanPage）、`ui/lib/main.dart`（路由）
- Create: `ui/test/scan_page_test.dart`

**ScanController（ChangeNotifier；Dart 只做状态机——设计 §6 铁律）：** 状态 `idle → starting → scanning ↔ paused → completed/canceled/failed`；订阅 `client.notifications` 分派 `scan.progress`/`scan.finished`（按 taskId 过滤）；**通知 + 轮询兜底**（通知丢失/迟到不影响正确性：扫描中每 1s `scanStatus` 对账）；`percent = totalBytes==0 ? null : readBytes/totalBytes`（quick 也拿到了 totalBytes——显示确定进度；深扫为真百分比）。

- [ ] **Step 1: scan_page.dart 结构（规范级）**
```
Scaffold(appBar: 设备名 + 返回)
├── 模式选择：SegmentedButton [快速扫描 / 深度扫描]（扫描中禁用）
│     深度模式副文案：「在未分配空间按文件签名找回（照片/图片）；无文件名，结果标注『仅雕刻』」
├── 主按钮：开始扫描（starting/scanning 时禁用）
├── 进度区（scanning/paused）：
│     LinearProgressIndicator(value: percent)   # percent 可空 → 不确定态
│     行：已扫 readBytes / totalBytes · 已找到 foundCount · 用时 elapsedMs
│     按钮行：[暂停]/[恢复] · [取消]（取消二次确认对话框）
├── 完成态：[查看结果 (N)] 主按钮 → ResultsPage(taskId)
└── 失败/取消态：状态文案 + [重新扫描]
```
- [ ] **Step 2: EACCES 引导（ScanController）**：`scanStart` 抛 `RpcException(-32001)` → UI 对话框：「需要管理员权限访问该设备」[取消] [授权后重试]；重试 = `IpcCoreClient.startPrivileged(daemonPath: 原路径, extraArgs: 原参数)` 重启客户端（`onClientReplaced` 回调让 main.dart 换用新 client）→ 重试 scanStart。**集成测试无法覆盖 pkexec（需真机 polkit）——widget 测试用 Fake 注入 -32001 → 断言对话框出现与重试调用序列**。
- [ ] **Step 3: scan_page_test.dart（Fake 驱动，全代码要点）**
1. 初始 idle：模式可切换；点开始 → Fake 记录 `scanStart(dev, mode:'deep')`（断言 mode 透传）。
2. 注入 progress(30%) + status 对账 → 进度条 value≈0.3、计数文本正确；注入 finished(completed) → [查看结果] 出现且计数==foundCount。
3. 暂停→恢复：按钮文案切换、调用序列 `scanPause`/`scanResume` 各一次。
4. 取消：确认对话框 → `scanCancel` 被调、状态 canceled。
5. -32001：对话框出现；[授权后重试] → Fake 记录了一次 privileged 重启 + 再次 scanStart。
6. 通知过滤：注入**其他 taskId** 的 progress 不得影响本页状态。
- [ ] **Step 4: 门禁与提交**（flutter test+analyze；commit `feat(ui): 扫描页（模式/进度/暂停取消/EACCES 引导）`）

---

### Task 6: 结果浏览页（虚拟化分页 / 过滤 / 多选 / 质量徽标）

> **T4 移交铁律（spec-m1d-t4 观察 b）**：`ScanEntry.displayName`（空名→`carved_%06d.%ext`）**仅供展示**——它不做 sanitize、与 worker 落盘名可能不同（ext 注入等）。**本页与 T8 报告页一律以 `ExportReportItem.name` 为实际落盘名**；任何写路径（导出/打开文件/预览另存）不得消费 displayName。
>
> **T5 移交（unawaited 陷阱 + 错误文案）**：① broadcast 流订阅的 `await sub.cancel()` 在 flutter_test fake async 下**永不收敛**（Dart null-future）——订阅取消一律 `unawaited(...)`（dispose 中同理），本页/T8 都别 await 它；② 展示层错误文案：`RpcException` 只显示 `message`（契约文案），StateError→「核心服务已退出，请重启应用」、TimeoutException→「核心服务无响应」；③ T5 的桩 `ResultsPage({required int taskId})` 由本页整体替换 body（签名兼容）。

**Files:**
- Create: `ui/lib/features/results/{results_page.dart, results_controller.dart, entry_tile.dart}`
- Create: `ui/test/results_page_test.dart`

**ResultsController：** 分页状态机——`pageSize=200`，`loadMore()` 在滚动到 80% 时触发（`ScrollController`）；`total` 来自首页响应；`deletedOnly` 与 `quality` 过滤切换时**重置分页**（offset=0 清列表）；`selected: Set<int>`（idx）多选；服务端排序即 `idx` 序（无本地排序）。

> **qual-m1d-t5 移交（实施前必读）**：① **契约 `scan.results` 只有 `{taskId, offset, limit, deletedOnly?}`——没有 quality 参数**（proto/v1/README:27）；`deletedOnly` 走服务端（重置分页重拉），**`quality` 只能对已加载页做客户端过滤**——必须写清 `loadMore() × 客户端 quality 过滤` 的组合语义（过滤只作用当前已加载集合；继续滚动加载更多，过滤集合增量扩大；`total` 显示语义写明是"已加载/过滤命中"而非全量），**禁止扩 v1.2 契约**（golden 冻结）。② 多选 `selected` 跨"过滤切换/重置分页"的存留语义要显式（建议：过滤切换即清选择）。③ O3 记录（M4/产品）：扫描中返回/切页不打断 daemon 任务（PopScope 二次确认归 M4）；`_confirmCancel` await 后无 mounted 复检（离页即作废意图，M4 裁定）。

- [ ] **Step 1: 页面结构（规范级）**
```
Scaffold(appBar: '扫描结果' + 计数 'N 项')
├── 过滤行：FilterChip[全部|仅删除] · FilterChip[完整|可能损坏|仅雕刻]（可组合；切换即重置）
├── ListView.builder（itemCount = entries.length + 1；末项 = 加载指示/已到底）
│     EntryTile：leading=类型图标（jpg/png→image 图标）· title=displayName · 
│       subtitle='路径 · 大小' · trailing=质量徽标（完整=绿/可能损坏=橙/仅雕刻=蓝灰）
│       雕刻件：subtitle 无路径，徽标旁注「仅雕刻 · 可能不完整」
├── 多选模式（长按进入）：Checkbox + 底部 [恢复所选 (K) → RecoverPage(taskId, idxs)]
└── 点击条目 → PreviewPage(taskId, entry)
```
- [ ] **Step 2: 文案铁律检查表（写进 results_page.dart 头注，widget 测试逐条断言）**
  1. 删除+**连续**（exFAT）**且 `quality == complete`**：「已删除 · 簇未被占用（完整性高）」；**（T6 执行精化/F1：连续但质量非 complete（含未知档）→ 落保守臂「已删除 · 恢复质量见分级」——位图证据不足或簇已被占时徽标已示"可能损坏"，行文案不得称完整性高；`null` 臂改 `_` 满足穷尽性；`(false, complete)` 软冲突 by-design over-warn 记录备查）**
  2. 删除+**非连续**（`contiguous==false`）：「已删除 · 按删除链恢复，可能不完整」；
  3. **任何文案不得出现"连续假设"**（grep 断言测试：源码不含该四字）；
  4. carved → 仅「仅雕刻 · 可能不完整」；
  5. live+complete → 不加任何警示。
- [ ] **Step 3: results_page_test.dart**
1. 首屏 200 条（Fake 提供 450 条）→ 滚动触发 loadMore → 断言 `scanResults(offset:200)` 被调、第三页合计 450。
2. 切换「仅删除」→ 重置调用 `scanResults(offset:0, deletedOnly:true)`、列表替换。
3. 质量过滤徽标与文案逐条对上（Step 2 五条 —— 五组构造条目注入 Fake，断言 tile 文案）。
4. 多选：选 2 条 → 底部按钮文本「恢复所选 (2)」→ 点击导航参数 `(taskId, [idx...])`。
5. 空结果/加载失败：空态文案 + 重试按钮。
- [ ] **Step 4: 门禁与提交**（commit `feat(ui): 结果页（分页/过滤/多选/质量徽标与铁律文案）`）

---

### Task 7: 预览页（图片/文本/信息；雕刻件回读）

**Files:**
- Create: `ui/lib/features/preview/{preview_page.dart, preview_controller.dart}`
- Create: `ui/test/preview_page_test.dart`

**PreviewController：** 按 ext 分派：图片（jpg/jpeg/png）→ **分片拉全量**（1MiB/次循环到 eof，上限 32MiB——超限显示"文件过大，暂不支持预览"）→ `Image.memory`（含 `errorBuilder`：数据坏时显示"数据损坏，无法预览"而非红屏；**T7 归档硬化 `cacheWidth: 2048`**（防大图按原始分辨率解码 OOM））；文本（txt/log/md/json…）→ 前 256KiB → `SelectableText`（utf8 allowMalformed）；其它 → 仅信息卡。信息卡恒显：名称/路径/大小/删除状态/质量徽标/`byteOffset`（雕刻件展示"偏移"）；**exFAT VDL 说明**：若 `sizeBytes < 实际可读` 无从得知（契约不含 DL）→ 不显示猜测，仅对短交付显示「实际数据短于声明大小」。

- [ ] **Step 1: 页面结构（规范级）**：AppBar=displayName；body=加载态→内容；底部信息卡；[恢复此文件] 按钮 → RecoverPage(taskId,[idx])。
- [ ] **Step 2: preview_page_test.dart**
1. 图片：Fake 分两片返回 TINY_PNG（**67B**——T7 归档勘误：原 68B 系笔误，实测 67B 且真可解码；Rust 夹具 70B 为另一物）→ 断言两次 `fsRead`（offset 0/67 第二片 length 到 eof）、`Image.memory` 出现（`find.byType(Image)`）。
2. 损坏图片：返回随机字节 → errorBuilder 文案出现。
3. >32MiB：Fake 报 sizeBytes 大 → 不调用 fsRead、显示"文件过大"。
4. 文本：返回 UTF-8 中文 → SelectableText 内容匹配。
5. 雕刻件：displayName==carved_*、信息卡含"偏移"字段与"仅雕刻"。
6. eof 提前（短交付）：「实际数据短于声明大小」提示出现。
- [ ] **Step 3: 门禁与提交**（commit `feat(ui): 预览页（图片/文本/信息，雕刻回读与损坏兜底）`）

---

### Task 8: 恢复页（目标选择 / 导出进度 / 报告）

> **T4 移交**：报告页「打开目标文件夹」与任何精确定位落盘文件的动作，一律用 `ExportReportItem.name`（实际落盘名）而非 `displayName`（展示名，见 T6 铁律）；成功件不在 items 里（契约如此）——「打开文件夹」只按目录打开，不按名定位成功件（M5b 注记）。
>
> **T5 移交**：通知/进度订阅取消一律 `unawaited(...)`（flutter_test fake async 下 `await sub.cancel()` 不收敛——见 T6 注记）；错误文案映射同 T6（RpcException 只显示 message）。

**Files:**
- Modify: `ui/pubspec.yaml`（+`file_selector`（官方，desktop 支持））
- Create: `ui/lib/features/recover/{recover_page.dart, recover_controller.dart, report_view.dart}`
- Create: `ui/test/recover_page_test.dart`

**RecoverController：** `selectTarget()` 用 `file_selector.getDirectoryPath()`；

> **qual-m1d-t5 移交（测试缝，实施前必读）**：`file_selector.getDirectoryPath()` 是平台插件，**widget 测试直接调会挂**——测试缝推荐 `FileSelectorPlatform.instance` 注入 fake（不改页面签名；`TestDefaultBinaryMessenger` 平台通道 mock 为备选）。`start()` → `exportStart(taskId, idxs, dir)`；订阅 `export.progress/finished`（按 exportId 过滤）；状态机 `picking → exporting → done(report)|failed|canceled`。目标目录展示预估大小 = `estimatedBytes`；报告页：`succeeded/degraded/failed` 三计数 + 降级/失败清单（reason 文案）+「打开目标文件夹」按钮（`Process.start('xdg-open', [dir])`——桌面 Linux；其他平台 no-op + 文案）。

> **T8 归档注记（lead）**：① `RecoverPage` 落码增必填 `client`（preview/results 两调用点同步传入，两枚上游钉测零改动）；② **finished 抢跑寄存回放加固**落地——T3 底盘竞序（daemon 可先发 finished 再回响应）而导出**无轮询兜底**，丢一条即永久卡「导出中」；计划外但 lead 追认，测试 7 钉死；③ O1（`start()` 重置 `_exportId/_done/_total/_writtenBytes`）+ O3（回放独立 try/catch）修复轮（qual-m1d-t8）+ 25 枚补测（`recover_page_supp*.dart`）；④ -32006/-32010 专用文案与 `itemsTruncated` 提示行已落；⑤ dev_dep `file_selector_platform_interface` + 6 件已跟踪生成物随提交（pub get 必需）；⑥ 未测三项（真 GTK 弹窗/真 xdg-open/真 daemon 全流程）归 T9。

- [ ] **Step 1: 页面结构（规范级）**
```
Scaffold('恢复文件')
├── 卡片：源任务(T) · 已选 K 项 · 预计 K_b 字节
├── 目标目录行：路径或「选择目标文件夹…」[选择]（选择后显示 remount 提示：勿选源设备所在盘）
├── [开始恢复]（未选目录禁用）
├── 进度区：LinearProgressIndicator(done/total) + '已完成 done/total · writtenBytes'
│     [取消恢复]
└── 报告视图（done）：三计数块 + 清单（status 图标/名称/reason）+ [打开目标文件夹] [完成]
```
- [ ] **Step 2: recover_page_test.dart**
1. 未选目录禁用；Fake 返回目录 → 启用；点开始 → `exportStart(taskId, idxs, dir)` 参数断言。
2. 注入 progress(1/2) → 进度 50%；注入 finished(succeeded1/degraded1/items=[...]) → 报告三计数与清单内容逐字；canceled=true → 「已取消」标题且计数保留。
3. 错误码映射：Fake 抛 `RpcException(-32006)` → 文案「目标不能是源设备所在的盘，请换一个文件夹」；-32010 → 「目标盘剩余空间不足」。
4. 取消按钮 → `exportCancel` 调用。
- [ ] **Step 3: 门禁与提交**（commit `feat(ui): 恢复页（目标选择/导出进度/报告与错误引导）`）

---

### Task 9: 全链路集成测试 + 出口验收

**Files:**
- Create: `ui/test/export_flow_integration_test.dart`
- Modify: `.github/workflows/ci.yml`（flutter job 增 `cargo build -p xd-daemon` 前置 + `XD_DAEMON_BIN` 环境）
- Modify: `scripts/e2e-loop.sh`（环回设备挂载后：export 到挂载点 → 断言 -32006；export 到 tmp → 文件字节比对）
- Modify: 设计文档/README/计划执行记录

- [ ] **Step 1: export_flow_integration_test.dart（真 daemon，XD_DAEMON_BIN 守卫，全流程）**
构造 exfat 镜像（Dart 无法建 exfat → **改用既有的测试夹具镜像**：测试从 `crates/xd-fixtures` 预生成的镜像文件？CI 无该文件——**方案：测试前置调用 daemon 自身的……不行。裁定：集成测试把「生成镜像」交给一个 Rust 小工具**：`cargo run -p xd-fixtures --example make_carve_fixture -- <path>`（M1d 增一个 example：生成含 live/删除/雕刻埋点的标准镜像）。测试流程：生成镜像 → 起 daemon → scan(quick) → results → fsRead 首条 → 与实际字节比对 → export ≥1 条到 tempdir → 文件存在且字节精确 → 报告 succeeded≥1。**雕刻链路**：scan(deep) → results 过滤 quality==carved → fsRead 回读 == 埋点原字节。
- [ ] **Step 2: e2e-loop.sh 增段**：`mount /dev/loopN /mnt/xd-test` → export.start targetDir=/mnt/xd-test/rec → 断言 `-32006`；umount；再 export 到 `$(mktemp -d)` → 断言文件可比对。CI 已有该脚本（Linux 作业）✓。
- [ ] **Step 3: 全量门禁**：`cargo test --workspace --locked`（debug+release）/clippy/fmt + `cd ui && flutter test && flutter analyze` + `bash scripts/e2e.sh` + `bash scripts/e2e-loop.sh` + 集成测试（daemon binary + XD_DAEMON_BIN）。
- [ ] **Step 4: 变异抽检**（kill 即通过；逐条记录）：1) `fs_read` 的 `eof` 恒 true/恒 false → 测试 kill；2) `read_file_range` 的 `skip_bytes` 忽略（从 0 读）→ `ranged_read_matches_full_read_slices` kill；3) 雕刻 `read_back` 的 run 界忽略（用设备尾）→ 截断件回读测试 kill；4) 导出重名去重删 → 重名测试 kill；5) 名字 sanitize 删（`..` 直通）→ sanitize 单测 kill；6) 降权调用删（root 直写）→ **单测无法在非 root 验证**：`drop_to_user` 抽纯函数（目标 uid 决策）+ e2e 记录「未验证（需真机 root/pkexec）」；7) `export.cancel` 的 kill 删 → cancel 测试 kill。
- [ ] **Step 5: 文档与合入**：security 文档（降权子进程模型 + 未验证边界）、README 功能表（三页 + 导出打勾）、设计文档 §4.5 实现注记、计划执行记录；合入 main + push + **CI 逐 job 验证**（`gh run view <id> --json jobs`）；provenance 与发版归用户触发的发布流程。

---

## 验收定义（M1d Done 的判据）

1. 契约 v1.2 双侧断言（13 新 golden + 5 错误码）；Dart typed 模型全覆盖。
2. 预览/导出全程**分片**读取（引擎 `read_file_range` + 雕刻 `read_back`），无整文件物化路径（>64MiB 预览拒绝有码；导出流式无上限）。
3. 导出：逐字节精确（集成断言）、降级/失败逐件报告、取消可用、目标盘三重校验（存在/异设备/余量）——异设备校验在 e2e-loop 真环回设备上验证 `-32006`。
4. 降权子进程模型：非 root 路径全测试覆盖；root/pkexec 路径**标注未验证（需真机）**并与 security 文档一致。
5. UI 三页 + 报告页：widget 测试覆盖状态机与铁律文案（含"连续假设"零出现的 grep 断言）；集成测试全流程（镜像 → 扫描 → 预览 → 导出）在 CI 可复跑。
6. 全量门禁绿（rust debug+release、clippy、fmt、flutter test+analyze、e2e.sh、e2e-loop.sh）。

---

## 执行记录

### T1（契约 v1.2）—— impl-m1d-t1。提交沿革：`4c7194f`（主）→ `42455fa`（qual 补强）。DONE → spec **PASS** → qual ISSUES → 补强有牙（T1 关闭，382/0）

- **计划缺陷 1**：Step 1 括注「`hello, xiaodun!` 16 字节」实为 **15**（base64 无 padding 实证）——golden 字面量未动（`eof:true` 在"请求 16>可得 15 短交付"语义下自洽）。
- **集合 36 同步三点**：Rust 收口测试 + Dart 集合测试 + README 清单（计划 Files 漏了 Dart 侧，实施者按前置指示补上）；**spec 诱饵检验**（第 37 个文件）两侧均红——活守卫实证。
- **spec 亮点**：13 golden 逐字节；params 反钉（snake_case/缺字段/错类型含 `length:-1`、`idxs:[0,-1]`）；错误五码与构造器逐字。
- **qual 变异 12 条**：6 KILL；**同族弱钉实证（本任务最有价值发现）**——五构造器各只被"单值==golden 内嵌值"调用，任何**去参数化**实现全不可分（M8/8b；clippy `useless_format` 只挡纯字面量子类）；`idxs: Vec<u64>→Vec<i64>`（负值被接受，M4）。补强落地：负值/别名反钉 + 五构造器异值断言（`entry_too_large(67108865)` 破 64MiB 边界巧合）+ **README「params 演进规则」3 行**（新增字段必须可选/不得删改/不得 deny_unknown_fields）——字段级治理从无到有。有牙实证：去参数化后 **golden 测试仍绿、参数化断言红**（M8 族现场）。
- **观察 C（双侧承诺）+ qual 细化 → T4 任务书**：Dart 需补四类值级覆盖（五码 expectRpcError / fs_read 值级 / export_start+cancel response / 两通知形状）——否则 11 个 non-error 新 golden 在 Dart 侧零值级覆盖。`idxs 去重后` 措辞两处对齐。
- 记录不修：golden 单点守卫（同 PR 双改不会被第二道网拦——v1 既有约定）；`alias` 放宽（等价无害）。

### T2（分片读取全链）—— impl-m1d-t2。提交沿革：`e7e16d9`（主，18 文件）→ `753897d`（qual 补测）。DONE → spec **PASS**（2691 窗口差分）→ qual **APPROVED** → 五处有牙（T2 关闭，407/0）

- **计划 vs 仓库裁定差异 4 条（按"仓库 read_file 为规格"落地）**：range 版必须同源 M1c 三道界卫 + (a) 早分支（计划片段是裁定前旧稿）；live 回访截断（计划缺）；**不物化簇序列**（流式滑窗）；fat 删除=连续回退。差分测试比计划强：5×9 / 6×8 → spec 扩到 **2691 窗口**（独立 oracle：自给簇序+设备原始 read_at，先对 oracle 再对切片）。
- **collector 化不採 sink（B 项，重要设计裁定）**：计划 sink 形态会**静默丢字节**（`Cursor::skip` 段体字节从不流经 sink；jpeg 段体含 `FF D9` 类标记字节时必漏——spec 探针实证"重读不止等价、是规格必需"）；改 `collect_*` = 裁决区间确定性重读（`read_prefix_at`）。M1c 47 测试零回归。
- **计划片段编译缺陷 3+1 条**：死绑定 `let want`（-D warnings 必红）、`ReadError` 缺 `#[derive(Debug)]`、`map_err` 类型不成立（ScanError 无 Display）、`(offset as usize)` 32 位截断——全部实修（`saturating_add` 防溢出）。
- **qual 变异 10 条：7 KILL；关键覆盖发现**——**range 分支的可达界卫/回访查重逃逸全部 401 个仓库测试**（repo 夹具盲区），仅差分探针捕获 → 两场景（`ranged_read_deleted_unreachable_is_empty`/`..._revisit_is_empty`）搬进 read_tests 并以有牙实证；`saturating_add` 裸 `+`（debug panic）与 `run.end→u64::MAX`（交付 5000≠4096）两缺口同样补测闭合；`read_prefix_at` 钳位=等价突变（补可选取值契约测试）。
- **裁定/移交**：(a) read_back 两遍 TOCTOU 可接受（源盘只读+确定性重读）；(b) `complete` 与交付长解耦（零消费者，doc 已注）；(c) **雕刻件"完整/截断"信号不落库**（quality 恒 "carved"）——UI 保守文案（"仅雕刻·可能不完整"）已覆盖，持久化列列为 **v1.3 契约候选**。(d) 性能快速路径的 ponytail 建议不成立（坏读/零读/短读三停点都可在窗口前发生——已改注"升级须以差分网格为裁判"）。
- 过程：实施者误用 `git checkout` 清掉未提交文档改动 → 自查重写并复核（终提交纯文档差异）；"只追加不 amend"本轮遵守。

### T3（恢复导出：降权子进程）—— impl-m1d-t3。提交沿革：`e90a79b`（父侧）→ `bc4a8e6`（worker）→ `4d3e40d`（路由+集成）→ `b041617`（盘级祖先）→ `6725c2d`（qual 八项修复）→ `55924e4`（测试底盘竞序）→ `9d4b5ea`（scan_ipc 同法收敛）。DONE → spec **PASS** → qual **ISSUES** → 修复有牙（T3 关闭，444/0）

- **★ 安全：#2 提权面封堵（实施者抓，计划断言在库层不成立）**：计划"worker 设备 id 来自自家 store、无越权面"对 RPC 层成立、**对库层不成立**（库文件属主即可被改写，伪造 `image:/etc/shadow` → 提权 worker 沦为任意 root 可读文件读取器）→ worker 的 `image:` 分支复用 `--image` 同闸（O_NOFOLLOW + 属主==PKEXEC_UID）；非 root 不加闸（语义正确）。**★ 盲区 #7（lead 裁定本轮修）**：整盘 `/dev/sdb`(8,16) vs 分区 `sdb1`(8,17) rdev 不等 → 同盘判定升 **盘级祖先**（sysfs 走链 + 注入根测试 + 真 /sys 冒烟）；fail-open（解析失败退回 rdev 相等 + stderr）经裁定；mountinfo 不解析有内核事实等价论证（采纳）。
- **spec 独立核验**：端到端导出逐字节（含删除件/雕刻件）；命名注入 `../evil` 等落盘安全；worker 协议故障矩阵（空数组/坏 JSON/坏目标→fatal exit 2；EPIPE→exit 0；SIGTERM→无 finished、已写保留）；伪造库行（`unix:/etc/shadow` 等）非 root 全拒。
- **qual 变异 12 条 + 4 测试缺口修复**：cancel 测试**两态恒真**（删 kill 也全绿）→ 补 `succ+deg<total` 真断言；MAX_PREVIEW **自指盲区** → 绝对锚 + 字面量；failed 件 E2E（cleanup+reason+无残骸）；EPIPE 转正。**实现级三修**：cancel 持 child 锁会阻塞全 daemon RPC（worker D 态）→ `Job.pid` 直发 SIGTERM；**雕刻导出 ∝size² 读放大**（峰值≈offset+4MiB）→ 单次全量回读（≤64MiB 上限）+内存切片（预览路径留 ponytail 注归 M4/M2）；`unique_name` O(n²)→name→counter map；**ext 未净化可在成功路径越出目标目录**（伪造行）→ ext 过 sanitize + 注入测试。
- **★ 测试底盘竞序（teeth 复验抓出）**：`scan.start`/`export.start` 先起后台线程再回响应 → `finished` 通知可抢在响应前（150 次插桩 6 次）；旧读法丢弃通知→永等。**`Wire` 寄存读口**（两序容忍、通知寄存不丢）收敛 export_ipc + scan_ipc 全量；确定性靶 + 变异版 6× 负载 2 FAIL vs 修复版 60/60（真抢跑负载诱发约 3%/跑，集中 cancel 响应侧）。
- **记录不修**：M6（items>1000 截断无构造）、M5a（--export-id 错值纯诊断）、I1 µs 残窗（终态×计数同锁发布归 M4）、stale-pid（M4 pidfd）、`{stem}_N` 规则已补 README v1.2。
- **未验证（需真机 root/pkexec）**：降权链与 root-mode `image:` 门；-32006 真环回断言归 T9/scripts。

### T4（Dart 传输层 v1.2）—— impl-m1d-t4。提交沿革：`88ff380`（主）→ `f35f7a1`（qual 补牙）。DONE → spec **PASS** → qual ISSUES(轻微) → 补牙有牙（T4 关闭；34+1 / 带 daemon 35/0）

- **交付**：v1.2 模型（`ScanEntry+byteOffset/contiguous/displayName`、`FsRead`/`Export*` 模型）；`CoreClient` 11 方法 + 通知流；`_call(params)`；`startPrivileged`（标注未验证）；`FakeCoreClient`（测试公用，home_page_test 全部换用）；`ipc_transport_test.dart`（`/bin/sh` 假 daemon 钉住"无 id=通知"唯一真分支——CI 真 daemon 无法产通知）。
- **spec 独立核验（强）**：独立 dart 解码器 + python 假 daemon；11 请求 golden 编码逐字 + 真 `IpcCoreClient` 发 23 条线上线文与 golden **逐字节相同**；**displayName 两端对齐表**（ext="" 两端同回退 bin；ext 注入形态 Dart 不 sanitize → 仅展示层——**铁律已入 T6/T8**：写路径只传 idx、报告用 `ExportReportItem.name`）。
- **qual 变异 10 条**：5 KILL；3 缺口补牙（broadcast 多监听/close onDone/Fake 分页 off-by-one——均实测有牙）；等价 1（null-id 行）;接受 1（startPrivileged argv 归真机清单）。**借文件热关 Fake 保真 P1/P2**（limit 1..=1000 校验文案逐字 + idx 升序）；P3/P4 记录。
- **错误语义修正（qual 实测）**：close 后**新**调用是挂 10s `TimeoutException`（非 StateError）；`StateError(-15)` 仅在途调用——T5 展示层映射已入计划。
- helper 重复不合并（YAGNI，第 4 个消费文件出现时再提取）；`"id":null` 行入通知流 → 分发器容忍 `method==null`（已入 T5）。

### T5（扫描页）—— impl-m1d-t5。提交沿革：`c654047`（主）→ `1a1b219`（qual 修复）。DONE → spec **PASS** → qual ISSUES → 修复有牙（T5 关闭；49+1 跳过 / 带 daemon 50/0）

- **★ unawaited 陷阱（实施者抓，spec 机制级定位）**：broadcast 订阅 `await sub.cancel()` 在 flutter_test fake async 下永不收敛（`Future._nullFuture`=root-zone 已完成 future，续体在 fake 时区外执行）→ **非 broadcast 专属**；EACCES 重试链曾当场卡死；修 `unawaited(...)`（取消同步生效，安全）。**已移交 T6/T8**。附注：`ipc_transport.close()` 内 `await _sub.cancel()` 是 M1b 既有，真实时区无碍。
- **接口 `CoreClient.restartPrivileged()`**（注入缝设计，替代三层参数穿透）；spec 以**桩 pkexec 亲测 argv 逐项保持**（`[daemonPath, --image, x.img, --verbose]`）+ 旧进程先死透。
- **spec 13 场景状态机探针**：通知全丢仅轮询收敛、daemon 死→failed 文案、percent 语义、终态先到先得、8 种畸形通知注入零影响。
- **qual 变异 12 条 + 2 补**：5 KILL；**O2 实证为真竞态**（在途 scanStatus 过期响应把暂停态拉回 scanning——探针 H 在 HEAD 红）→ 一行守卫修复（`id/state` 双查）；`describeScanError` 的 RpcException 前缀（我那"只显示 message"的裁定此前只在注记里）→ 落码 + 断言；7 有牙（含 D 的 `fresh!` 行为杀修正与"独立删除 dismissElevation=等价"的如实转录）。
- **裁定**：O1 保持单次失败即终态（容错等真机抖动证据，M5）；O3 记录（离页不打断 daemon / 连点并发窗 / `_confirmCancel` 无 mounted 复检——M4/产品）。
- **T6/T8 移交（已入计划）**：quality 过滤**只能客户端对已加载页**（契约无 quality 参数，禁扩冻结契约）；`file_selector` 测试缝=`FileSelectorPlatform.instance` fake；取消态无「查看结果」入口=主动收窄（契约可查属实）。

### T6（结果浏览页）—— impl-m1d-t6。提交沿革：`6ebe341`（主）→ `a7dbfbc`（qual 修复）。DONE → spec **PASS** → qual ISSUES → 修复有牙（T6 关闭；65+1 跳过 / 带 daemon 66/0）

- **交付**：结果页 13→16 枚测试；分页 200/页 80% 阈值；`deletedOnly` 服务端 + **quality 客户端过滤**（组合序列 `[(0,f),(0,t),(0,t),(200,t)]`，offset 恒=已加载数）；三层计数语义（AppBar total/已加载/过滤命中+提示行）；`_generation` 过期响应守卫（双向）；total 虚高封口；两下游桩（Preview/Recover，签名兼容 T7/T8）；`util/errors.dart` 上收（T5 的 describeScanError 迁入）。
- **spec 独立核验**：18 探针（[0,200,400] 零网络尾、79%/82% 双侧阈值、失败序列 [0,200,200]、封口同 offset 不重拉、铁律五组+禁词 lib/** 非空扫 canary、FAT null 三义）；**禁词「连续假设」全仓 0**（Rust 注释中字样属引擎文档，不在断言范围）。
- **★ F1（spec+qual 双抓，高危真缺口）**：`entry_tile` 的 `true` 臂不看 quality → 删除+连续+`maybeDamaged`（**删后被部分复用=必经场景**）文案「完整性高」与徽标「可能损坏」打架，且与引擎「位图逐簇全空才算 Complete」冲突 → 门控 `true when quality=='complete'` + `null`→`_`（穷尽性，spec 的"一行"实为两处）+ 测试（含未知档前向兼容）。
- **qual 变异 12 条**：8 KILL；3 缺口补测（G1 `_generation` 仓库零回归网→移植 P8a/P8b 脚本化 client；G2 未知档整记录断言；G3 降序点选）；`false,complete` 软冲突 by-design over-warn（记录）。**FAT 删除文案裁定：维持保守「恢复质量见分级」**——更正后的理由：fat 分级虽逐簇查 FAT 表，但「假设 run 空闲」≠文件真实簇序（碎片化不可考），且 null 混迁移前 exFAT 旧行（链读），无措辞对三义皆成立。
- 过程趣闻：spec 曾报"并发写者告警"——实为 qual 的变异作业（改-测-还原），确认后已把「qual 变异振荡属正常」记入团队记忆。

### T7（预览页）—— impl-m1d-t7。（跨会话续跑：实现 `c4078cc` 在上会话完成，本会话评审关闭——上会话中断遗留 qual 变异残骸 `offset +=`，lead 还原后重启管线。）提交沿革：`c4078cc`（主）→ `1f27824`（-32009 同文案追补）→ `425b6f9`（qual 补测八枚）→ `a3db50e`（P2 硬化）。DONE → spec **PASS** → qual ISSUES（14 存活变异）→ 补测有牙 + P2 修复 → qual 增量 **APPROVED**（T7 关闭；80+1 跳过 / 带 daemon 81/0）

- **交付**：预览页 7→15 枚测试（计划六枚 + 导航 + 8 补测）；ext 三分派（jpg/jpeg/png 图片、txt/log/md/json 文本、其余仅信息卡）；1MiB 分片循环到 eof（上限 32MiB：恰界放行、+1 零读取拒绝）；文本 256KiB 前缀 utf8(allowMalformed)；信息卡恒显（byteOffset「偏移」仅非空显示——"不猜 VDL"）；短交付判定 `eof && 实收<声明`（实收>声明不报）；`QualityBadge`/`entryQualityNote` 由 entry_tile 提取为上收件（零行为变化）；-32009 与本地 cap 同文案「文件过大，暂不支持预览」且原始 `Entry too large` 不泄漏 UI。
- **P2 硬化（qual 抓，lead 裁定本轮落地）**：`Image.memory(cacheWidth: 2048)` 封大图原分辨率解码 OOM 面（`ResizeImage` 默认 allowUpscaling=false，小图零代价）；钉测 `ResizeImage.width==2048`；残余 PNG 解码瞬态归 M4/M5。
- **spec 独立核验**：8 枚探针全绿；两条契约外防御分支"两读"（删防御①非 eof 零交付 → 探针红；删防御②越上限 → 探针红）；残留变异 `offset +=` 独立 kill（探针 3 红 + 仓库套件挂死=死循环防线本体）；cap 恰界/短交付四边界/错误映射双路/信息卡字段。**计划 68B 系笔误**（实测 67B、Rust 夹具 70B）→ 本归档修订任务书。
- **qual 变异 24 条**：10 KILL → 14 SURVIVE 全为测试缺口（分派臂 png/json、cap `>`/`>=`、短交付去 `eof &&`、allowMalformed、null 偏移照显、`_disposed` 守卫、循环防御行、ext toLowerCase 等）→ 8 枚补测成品（+161 行）**14/14 KILL 归因干净**；P2 落码后仓库侧 **24/24 KILL**（新增 M20 删 cacheWidth=KILL）。
- **工件更替记录**：qual 权威件 v1（`4ebeae…`）CI format 门禁不过 → qual 落地期重写 v2（`32629c…`/399 行）；impl 对 v1 的机械 format 结果与 v2 **逐字节相同**（cmp exit 0；6 处纯空白/换行逐处记录）。**裁定：绑定 v2**（`32629c98…`），v1 废止。
- **记录级偏差（裁定归档）**：① 提交信息非逐字（T6 先例）；② 超 Files 清单改 2 文件（QualityBadge 提取 + results_page 调用点透传=编译必需，无夹带）；③ 68B 笔误修订。
- **P3/P4 记录不修（归 M4/M5）**：P3 文本 >256KiB 无截断提示；P3 同位重建 State 复用（**T8/T9 勿在同槽位重建 PreviewPage**）；P4：eof 检查先于越限（撒谎件 ≤33MiB 界内多收 1 片）、takeBytes 2× 峰值 ≤66MiB+base64 瞬态、失败臂无重试按钮、QualityBadge 第三消费者出现时按 util/errors 先例上收。
- **移交 T8/T9**：T8=RecoverPage 接导出链路时同步 `preview_page.dart:51` 调用点（传 client）；错误映射复用 `describeCoreError`；-32006/-32010 文案按计划 773。T9=-32009 集成须用文本件（图片被 32MiB 本地截先行）；雕刻件预览断言 `fsRead(idx)`+字节；qual 24 条变异并入 T9 抽检池。未验证：真 daemon 分片/eof 行为、-32009 真路径、大图真解码耗时/内存、eof+cap 同片多收。

### T8（恢复页）—— impl-m1d-t8。提交沿革：`94c61fb`（主）→ `2928b19`（qual 两修复 + 25 枚补测）。DONE → spec **PASS** → qual ISSUES（O1/O3 必修）→ 修复有牙（T8 关闭；115+1 跳过 / GATE2 待二进制刷新，见★）

- **交付**：恢复页桩→正式（controller/page/report_view 三件）；状态机 `picking → exporting → done|failed|canceled`；`file_selector` 目标选择（测试缝 `FileSelectorPlatform.instance`）；`exportStart(taskId,idxs,dir)`；`export.progress/finished` 按 exportId 过滤；-32006/-32010 专用文案、其余 describeCoreError；报告三计数+清单（落盘名一律 `ExportReportItem.name`）+ `itemsTruncated` 提示；[打开目标文件夹] 仅按目录；`RecoverPage` 增必填 client、两调用点同步；测试 10→35 枚。
- **★ finished 抢跑寄存回放（impl 计划外加固，lead 追认）**：T3 底盘竞序——daemon 可先发 finished 再回响应；导出**无轮询兜底**，丢一条即永久卡「导出中」→ 未知 exportId 在途寄存、响应到达后双检回放；spec 穿透（删寄存/删回放→测试 7 红；异 id 双检拒、非在途丢弃、单槽三处清零=无界增长不存在、串扰自愈、重复幂等）。
- **★ O1/O3 修复轮（qual 修正因果链）**：O1=二次导出在途窗口 `_exportId` 残留 → 新导出自己的抢跑 finished 被双拒 → 永久「导出中」（实测取消指旧 id、旧计数残留）→ `start()` 重置四字段；O3=畸形 finished 回放落外层 catch → failed+TypeError 上屏而导出在跑 → 回放独立 try/catch。回退变异 m30/m31 → 恰 3 枚红（与 impl 自证一致）。
- **qual 变异 51→54 枚**：v1 44 KILL/7 SURVIVE → 25 枚补测落地后 **47 KILL/6 等价 SURVIVE**；repo 单跑杀不掉的 20 枚全转 KILL；**m26 修正为 KILL**（v1「等价」判定有误——新补测正好观测到）。工艺披露：m30/m31 首跑 harness 缺陷自修、m31 单独复跑补齐。
- **spec 独立核验**：15 探针；6 项偏离逐项实证接受（dev_dep lint 必要性 / 生成物逐字节重生成 + lock enforce / 寄存回放重点穿透 / itemsTruncated 字段对齐 / 骨架必要偏差 / 未测三项）；两枚上游钉测 diff 零改动。
- **O2 异议（qual 胜）**：formatBytes 边界实测正确（0/1023/1024/1048575/1MiB/1TiB），spec「进位毛刺」不成立 → 不改码 + 3 枚边界断言。
- **记录不修（归 M4/M5）**：`start()` 不清 `_report`（单引用被下次 finished 替换、exporting 不渲染 → 无泄漏/无陈旧显示，清理代价大于收益）；非 Linux SnackBar 臂无测试缝；清单渲染非懒加载（≤1000 量级可接受）；cancel 后 `_error` 瞬态残留。
- **★ 环境前置（T9 首步）**：GATE2（XD_DAEMON_BIN 集成）红——target 中 daemon 二进制过期（构建早于 T3 起全部引擎改动），报 `-32601 Method not found: fs.read`；父提交 94c61fb 同红，**非 T8 回归**。T9 首步 `cargo build`（debug+release）刷新后复跑确认。
- **移交 T9**：集成须覆盖真 daemon exportId 过滤与至少一次真抢跑时序；-32006/-32010 可直接断言 UI 文案；不得引入真 xdg-open 进程断言；变异池 54 枚可抽检（`matrix2_result.json`）；-32009 集成须用文本件（>64MiB）；雕刻件预览断言 `fsRead(idx)`+字节。未验证：真 GTK 弹窗、真 xdg-open、真 daemon 全流程（GATE2 绿态待刷新复跑）。

---

© 2026 erik · https://erik.xyz · erik@erik.xyz
