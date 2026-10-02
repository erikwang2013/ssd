# 小盾 IPC 契约 v1

v1 是 v0 的**超集**：新增 `scan.*` 方法（扫描任务事件流）、`scan.progress`/`scan.finished`
通知、`ScanEntry`/`ScanProgress` 结果类型、`-32001`/`-32002`/`-32003`/`-32004`/`-32005`/`-32603`
错误码，以及 `DeviceInfo.transport` 可选字段。**v0 封存**：`proto/v0/**` 不再改动，
仅作历史快照（v0 golden 里 `protocol` 仍为 0）；唯一例外：
`error_method_not_found.response.json` 的示例方法名随 `scan.start` 转正修正（M1b T5），其余不动。
**破坏性变更才递增协议号；本版为新增**——v1.1（M1c 雕刻）与 v1.2（M1d 预览/导出）
同样为纯增量，不递增 `protocol`。

- 传输（stdio、每行一条 JSON）、信封（JSON-RPC 2.0）、`id` 原样回显、空行处理、
  `<VERSION>` 占位约定、golden 规则（`examples/*.json` 为唯一事实源，Rust/Dart 双侧
  断言 decode 与 encode，改契约必须同步改 golden 与两侧测试）：**同 v0**，见
  `proto/v0/README.md`，本版不重复。
- 版本：`protocol = 1`。daemon 与 UI 不匹配时由 `ping` 比对 `protocol` 检出。
- 只读铁律：扫描全程只读打开设备；无权限设备不静默降级，`scan.start` 返回 `-32001`
  由 UI 引导提权（M1d 消费）。

## 方法

| method | params | result |
|---|---|---|
| `ping` | null | `{"pong": true, "version": "<VERSION>", "protocol": 1}` |
| `device.list` | null | `{"devices": [DeviceInfo]}` |
| `scan.start` | `{"device": "<id>", "mode"?: "quick"\|"deep"}` | `{"taskId": <u64>, "fs": "fat"\|"exfat", "totalBytes": <u64>}` |
| `scan.status` | `{"taskId": <u64>}` | `{"taskId", "state", "readBytes", "foundCount", "elapsedMs"}` |
| `scan.results` | `{"taskId", "offset", "limit", "deletedOnly"?}` | `{"total": <u64>, "entries": [ScanEntry]}` |
| `scan.pause` | `{"taskId"}` | `{"taskId", "state": "paused"}` |
| `scan.resume` | `{"taskId"}` | `{"taskId", "state": "scanning"}` |
| `scan.cancel` | `{"taskId"}` | `{"taskId", "state": "canceled"}` |
| `fs.read` (v1.2) | `{"taskId", "idx", "offset", "length"}` | `{"bytesBase64": "<base64>", "eof": <bool>}` |
| `export.start` (v1.2) | `{"taskId", "idxs": [<u64>], "targetDir": "<绝对路径>"}` | `{"exportId": <u64>, "fileCount": <u64>, "estimatedBytes": <u64>}` |
| `export.cancel` (v1.2) | `{"exportId": <u64>}` | `{"exportId", "state": "canceled"\|"completed"}` |

- `scan.start`：`mode` 缺省 `"quick"`；`"deep"` 自 M1c 起有效（未分配空间雕刻）。
  设备无权限（EACCES）→ `-32001`；未知或不支持的文件系统 → `-32002`；其它 `mode` 值
  → `-32602`（未知模式不得静默降级）；`"deep"` 前置的空闲空间枚举失败 → `-32005`。
- `scan.status`：`readBytes` 为已扫描字节、`foundCount` 为已发现条目数、`elapsedMs`
  为任务开始至此刻的墙钟（含暂停时间）；净扫描时长 M1c 再议。
- `scan.results`：`offset`/`limit` 分页；`deletedOnly` 缺省 false，为 true 时只看删除项；
  `total` 为**应用 `deletedOnly` 过滤后**的总数（分页用）。
- `scan.pause`/`scan.resume`/`scan.cancel`：仅对活动任务有效——任务不存在 → `-32003`；
  状态不允许该操作（如对 completed 任务 pause）→ `-32004`；对已暂停任务再 pause → 幂等 Ok；
  取消系持久语义，
  终态 `canceled` 后结果仍可查。

## 状态

`state` ∈ `pending` | `scanning` | `paused` | `canceled` | `completed` | `failed`。

## 通知（daemon → UI）

JSON-RPC 2.0 通知：**无 `id` 字段**，不期待响应；UI 按 `method` 分发（路由实现归 M1d）。

| method | params |
|---|---|
| `scan.progress` | `{"taskId", "state", "readBytes", "foundCount", "elapsedMs"}` |
| `scan.finished` | `{"taskId", "state", "foundCount", "elapsedMs"}` |
| `export.progress` (v1.2) | `{"exportId", "done", "total", "writtenBytes", "elapsedMs"}` |
| `export.finished` (v1.2) | `{"exportId", "succeeded", "degraded", "failed", "canceled", "targetDir", "items", "itemsTruncated"}` |

- **节流**：`scan.progress` 满足「距上一条 ≥250ms 或 `readBytes` 增量 ≥1MiB」才发送；
  客户端不可假定按固定频率收到。
- `scan.finished` 在任务进入终态（`completed` | `canceled` | `failed`）后发送，每任务至多一条。

## ScanEntry

```json
{"idx": 0, "name": "IMG_0001.JPG", "path": "/DCIM", "ext": "jpg", "sizeBytes": 12000,
 "deleted": true, "isDir": false, "quality": "complete", "firstCluster": 6}
```

- `idx`：任务内序号（0 起，分页排序键）；`name`：目录项原名（删除项也一字不差）；
  `path`：所在目录（根为 `"/"`，无尾斜杠）；`ext`：扩展名（小写；无扩展名为 `""`）。
- `deleted`：是否删除项；`isDir`：是否目录。
- `quality`：恢复质量 ∈ `complete` | `maybeDamaged`（删除项沿簇链判定；`maybeDamaged`
  表示链已失效或无法判定，见 M1b 安全裁定 (a)「链只走不猜」）。
- `firstCluster`：起始簇号（0 = 无簇/未知）。
- `byteOffset`（v1.1 增量）：雕刻条目的未分配空间内起始字节坐标，缺省省略——见「v1.1 增量」小节。

## DeviceInfo

v0 字段不变，**新增 `transport`**：

```json
{"id": "unix:/dev/sdb", "name": "USB Disk", "kind": "physical",
 "sizeBytes": 3907029168, "removable": true, "fsGuess": null, "transport": "usb"}
```

- `transport`：值域 `usb` | `mmc` | `nvme` | `sata` | `virtio` | `other`；
  **缺失 = 未知**——镜像设备恒缺失，序列化时省略 `null` 键（v0 golden 与既有序列化
  输出不受影响）。仅作 UI 分组提示，不作任何过滤依据。

## 错误

| code | 含义 |
|---|---|
| -32700 | 无法解析的 JSON，或反序列化后不构成合法 Request（v0 继承；`id` 回显 null） |
| -32601 | 方法不存在（v0 继承；`message` 为 `Method not found: <method>`） |
| -32602 | 参数不合法（v0 保留码）；v1 起用于参数校验与 `Cannot open device: <id>` |
| -32001 | 设备权限不足（`Device permission denied: <id>`；UI 引导 pkexec 提权，M1d 消费） |
| -32002 | 不支持的文件系统（`Unsupported file system`） |
| -32003 | 任务不存在（`Task not found: <taskId>`） |
| -32004 | 任务状态不允许该操作（`Task not active: <taskId>`） |
| -32005 | 空闲空间不可判定（`Cannot determine free space`；`mode:"deep"` 前置：exfat 位图 / FAT 表不可读——「无空闲」是合法空扫，不得与此混同） |
| -32006 | 恢复目标落在源设备上（`Target is on the source device: <dir>`；`st_dev(目标) == st_rdev(源块设备)`，root 降权前判定；镜像源不适用） |
| -32007 | 目标不可写（`Target not writable: <dir>`；不存在/非目录/无权限） |
| -32008 | 条目不存在（`Entry not found: <idx>`） |
| -32009 | 条目过大（`Entry too large: <sizeBytes>`；`fs.read` 预览上限 64MiB，**导出不受此限**） |
| -32010 | 目标盘余量不足（`Insufficient space on target: need <n> bytes`；`n` = 预估交付字节上界） |
| -32603 | 内部错误（`Internal error`） |

错误 `message` 文案属契约一部分，两侧测试逐字断言。特例：`-32602` 的 `message` 形如
`Invalid params: <reason>`（`reason` 为诊断文本，**不属契约**）；`Cannot open device: <id>`
为固定文案。

## v1.1 增量（M1c 文件雕刻）

**纯增量，读旧客户端不受影响**（不递增 `protocol`）：`byteOffset` 序列化时省略 `null`
键，`quality` 既有取值不变，既有 21 个 golden 一字不动；本版新增 golden 2 个（雕刻结果页、
`-32005` 错误页）。

- `scan.start.mode` 值域 `"quick"（缺省） | "deep"`：`deep` 自 M1c 起有效（未分配空间雕刻）；
  其它值仍 `-32602`。
- `scan.start` 结果 `totalBytes` 语义：**本任务的目标扫描字节数**——`quick` = 设备大小
  （M1b 既有语义，不变）；`deep` = 未分配空间总字节（Σ 雕刻区间）。
- `ScanEntry.quality` 值域增 `"carved"`：雕刻件（无目录项）。结构重组成功度见 `sizeBytes`：
  找到 EOI/IEND = 结构完整；未找到 = 诚实截断前缀（不长不短不猜）。
- `ScanEntry.byteOffset`：u64，**缺省 = null**（序列化省略）。FS 条目恒缺；雕刻条目为
  未分配空间内的起始字节坐标（`firstCluster` 对雕刻件恒 0——无簇概念）。

## v1.2 增量（M1d 预览与恢复导出）

**纯增量，读旧客户端不受影响**（不递增 `protocol`）：既有 23 个 golden 一字不动；
本版新增 golden 13 个（`fs.read`/`export.start`/`export.cancel` 请求与响应、
`export.progress`/`export.finished` 通知、五个错误页）。

- `fs.read`：条目**分片读取**（预览/导出共用）——`length` ∈ `1..=1048576`（超限 → `-32602`）；
  返回 `bytesBase64`（base64 标准字母表）与 `eof`。`eof` = 已交付到该条目**可得数据的末端**
  （损坏/短链件可能 < `sizeBytes`——UI 以此判「可能不完整」，不猜测 VDL）；`offset` 越尾 = 空交付。
  条目不存在 → `-32008`；条目 `sizeBytes > 64MiB` → `-32009`（**导出不受此限**：导出内部 4MiB 片流式）。
- `export.start`：`idxs` 去重后非空且 ≤100000（否则 `-32602`）；`targetDir` 必须为绝对路径；
  **目标三重校验**（在 daemon 侧做）：不存在/非目录/不可写 → `-32007`；目标与源为同一设备
  （`st_dev(目标) == st_rdev(源块设备)`，镜像文件源不做此校验）→ `-32006`；
  余量 < `estimatedBytes` → `-32010`。`estimatedBytes = Σ sizeBytes` 为**上界**（降级件实际可能更短）。
- `export.cancel`：运行中 → 终止 → `{"state": "canceled"}`；已终态 → 幂等原样返回其终态
  （`"canceled"`/`"completed"`）；未知 `exportId` → `-32602`（重试语义即「已完成/不存在」）。
- `export.progress`：节流同 `scan.progress`；`export.finished`：`items` 仅含**降级/失败**条目
  （`status` ∈ `"degraded"|"failed"`，≤1000 条，超限截断置 `itemsTruncated: true`）——
  成功条目不回传（UI 从 `succeeded` 计数展示），避免十万条通知爆流。

**params 演进规则**（字段级治理，后续增量照此，T2/T3 加字段的据）：新增字段必须**可选**
（`#[serde(default)]` 或 `Option<T>`，语义为「缺失 = 旧行为」）；**不得**删除或重命名既有字段；
**不得**加 `deny_unknown_fields`——golden 只增不改（沿用本版先例）。

## golden 文件（36）

examples/ 下：`ping.request.json`、`ping.response.json`、`device_list.request.json`、
`device_list.response.json`、`scan_start.request.json`、`scan_start.response.json`、
`scan_status.request.json`、`scan_status.response.json`、`scan_results.request.json`、
`scan_results.response.json`、`scan_pause.request.json`、`scan_pause.response.json`、
`scan_resume.request.json`、`scan_resume.response.json`、`scan_cancel.request.json`、
`scan_cancel.response.json`、`scan_progress.notification.json`、`scan_finished.notification.json`、
`error_device_permission.response.json`、`error_unsupported_fs.response.json`、
`error_task_not_active.response.json`、`scan_results_carved.response.json`、
`error_unallocated_unavailable.response.json`、`fs_read.request.json`、`fs_read.response.json`、
`export_start.request.json`、`export_start.response.json`、`export_progress.notification.json`、
`export_finished.notification.json`、`export_cancel.request.json`、`export_cancel.response.json`、
`error_target_on_source.response.json`、`error_target_not_writable.response.json`、
`error_entry_not_found.response.json`、`error_entry_too_large.response.json`、
`error_insufficient_space.response.json`。

Rust 侧断言：`crates/xd-core/tests/contract_v1.rs`；Dart 侧：`ui/test/protocol_v1_test.dart`。

---

© 2026 erik · https://erik.xyz · erik@erik.xyz
