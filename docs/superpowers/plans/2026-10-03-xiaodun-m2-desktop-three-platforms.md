# 小盾 M2「桌面三平台」实施计划：NTFS + ext4 引擎 · 平台收口 · 三平台安装包

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 兑现 README 路线图 M2 行——**NTFS 只读引擎（含删除文件恢复 + `$Bitmap` 覆盖评估）与 ext4 只读引擎（快速扫描 + 深扫复用 `xd-carving`）**，把 M1e 尾段遗留的 12 项平台边界逐条收口（§12 总表 + 各任务移交），并交付**三平台安装包**：Linux deb（已有）· Windows 便携 zip · macOS **签名 + 公证** zip（arm64 + x64 双物证）。出口标准：**三平台安装包，三引擎（FAT/exFAT·NTFS·ext4）可用**；CI 3 OS 全矩阵 + packaging job 全绿。

---

## 关键裁定（本计划即规格；以下为起草期已定，实施不得静默偏移）

### R1. ext4 删除恢复口径 = journal 覆盖层（**设计 §4.2 前提勘误，本裁定以本计划为准**）

起草期实证（文献级，见「理论与实证参考」）：**现代 ext4 删除时内核把 inode 的 extent 指针清零**（i_block 60 字节被清），且目录项的名字在删除时从目录块索引中移除（dirent 的 inode 字段置 0、rec_len 并入前项）。因此设计 §4.2 的「inode 的 dtime 置位，extent 树通常还在 → 扫 inode 表按 dtime 过滤」**不足以支撑"文件名/大小/时间戳完整找回"**——只扫 dtime 会得到「无名、无数据指针」的空壳。业界唯一快速通道是 **jbd2 journal**（extundelete / ext4magic / ext4dfr 同路线）：journal 里存有删除前一刻的目录块与 inode 表块副本。

- **v1 口径**：快速扫描 = live 全量（目录树）+ **journal 覆盖层删除恢复**（名字 ← 旧目录块；大小/时间戳/extent ← 旧 inode 表块；两源交集且以 inode 号互证）。
- **诚实边界（必须写进文档）**：journal 环形覆盖后（删除久远/写入量大）或 journal 被清空 ⇒ 无快速恢复，**深扫（雕刻）是兜底**；quick 扫描**不产出任何猜测性条目**（无 journal 命中就是没有）。
- **Cut 线（本计划最大不确定项）**：T5（journal）若排期不济，可整体后移 M3——届时 ext4 = live + 深扫，出口降级为「ext4 引擎可用（删除恢复归 M3）」并需 lead 书面接受。
- T5 Step 1 是**内核删除语义实证 gate**（真 loop 挂载 + 删除 + 快照）：若实证推翻「extent 清零」（保留），删除分支**追加**「直接扫 dtime inode」快路（更省），journal 仍保留（取名字）。

### R2. NTFS 恢复口径 = `$MFT` 全表扫描 + `$Bitmap` 覆盖分级 + 记录号读取

- 遍历 `$MFT`（**经其自身 `$DATA` runlist 读，不假设连续**）：`FILE` 记录 → USA fixup → 属性表 → `$FILE_NAME`（Win32 命名空间、多名字取 LSN 最高者）→ 父引用链重建路径；`in-use` 位清 = 删除项。
- **可解释的恢复成功率**（设计 §4.2 要求）：对删除项逐簇查 `$Bitmap`——0 簇被复用 = `complete`；≥1 簇被复用 = `maybeDamaged`。live 项恒 `complete`（读不出来时才降级）。
- 读取：`recordId` = MFT 记录号 → 重解析记录 → 常驻 `$DATA` 直接切片 / 非常驻走 runlist（稀疏 run 补零；`initialized_size`(VDL) 之后 `real_size` 之前补零——NTFS 规范语义，不是猜测）。
- **v1 不做（如实过滤 + stderr 计数 + 文档）**：压缩（`$DATA` flags 0x0001）与 EFS 加密文件；命名数据流（ADS，只认无名 `$DATA`）；硬链接只报一条 `$FILE_NAME`；系统文件（记录 0-15 及根链经过系统区的记录）不列。
- 文件名 UTF-16LE 解码（孤立代理 → U+FFFD），管道同既有契约（`path` 无尾斜杠、根 = `/`）。

### R3. 契约 v1.3 = 纯增量（不递增 `protocol`），照 proto/v1 README「params 演进规则」

1. `scan.start` 结果 `fs` 值域增 `"ntfs" | "ext4"`（既有 `"fat" | "exfat"` 不变）。
2. `ScanEntry.recordId`（可选 u64，序列化省略缺省）：**FS 元数据记录号**——NTFS = MFT 记录号；ext4 = inode 号；fat/exfat/雕刻件缺失。读取路径以它重定位（与 `byteOffset` 分工明确：后者恒为雕刻件）。
3. 新方法 `daemon.shutdown`（params null → `{"accepted":true}`，随后清理 port-file 并 exit 0；stdio 下亦接受，等价 stdin EOF）。
4. **quality 值域不变**（`complete|maybeDamaged|carved` 足够表达两引擎分级）；**不新增错误码**（不可判定沿用 -32002/-32005/-32603 语义）。
5. golden 只增不改：新增 4 枚（`scan_results_ntfs.response.json`、`scan_results_ext4.response.json`、`daemon_shutdown.request.json`、`daemon_shutdown.response.json`）；既有 36 枚一字不动；Dart 侧 `protocol.dart`/`protocol_v1_test.dart` 同步。

### R4. 平台同源校验（-32006）统一为「源设备身份」并补齐 Win/macOS 盲区

- `xd-core::export::check_target` 的 `source_rdev: Option<(u64,u64)>` 参数改为 `source: &SourceRef` = `{ id: &str, rdev: Option<(u64,u64)> }`（`id` = 任务行 `device_id`，如 `win:\\.\PhysicalDrive2` / `unix:/dev/disk3`）。
- **Windows（②）**：源 = `\\.\PhysicalDriveN` 盘号；目标目录 → `GetVolumePathNameW` → 打开卷（`\\.\C:`）→ `IOCTL_STORAGE_GET_DEVICE_NUMBER` → 盘号；**盘号相等 ⇒ -32006**。物理盘扫描在 Windows 必经提权、导出 worker 是其子进程（同权）⇒ 卷句柄可达。打开卷失败 ⇒ warn + 放行（fail-open + 留痕，与 unix sysfs 缺位同口径）。
- **macOS（③）**：精快路径保留（unix 同式 dev_t），**追加 IOKit 归属道**：目标目录 `statfs.f_mntfromname`（如 `/dev/disk3s5`）→ IOKit 沿 `IOMedia` 父子链归一为整盘（`disk3`）；源 `unix:/dev/diskN` 取整盘名；相等 ⇒ -32006。同时以 IOKit 补 `transport`/`removable` 映射（`kIOMediaRemovable` + 设备特征 → `usb|sata|nvme|other`），与 Linux/Windows 同一值域。
- 实现落点：平台代码**只进 `xd-device` 平台模块**（`windows.rs`/`macos.rs` 增纯函数 + 薄 FFI），`xd-core` 只做分派调用；`macos` 侧 IOKit 用 `#[link(name = "IOKit", kind = "framework")]` 原生 FFI（**零新 crate 依赖**，与 windows-sys 先例同级）。
- 顺带收口（起草期新发现的 Windows 导出功能缺口，同一文件面）：**Windows 导出取消**（现 `-32603 PlatformUnsupported`）与 **Windows 余量预检**（现 warn 跳过）——分别用 `OpenProcess(PROCESS_TERMINATE)+TerminateProcess` 与 `GetDiskFreeSpaceExW` 落实现。二者不修则「三平台安装包可用」名不副实。

### R5. 提权会话加固方向（⑤⑥⑦）

- **shutdown 方案**：`daemon.shutdown` RPC（R3-3）+ UI 在**换 client / 退出 / 会话独有**时显式调用；替换旧会话后旧 daemon **立即**退出并清 port-file（收窄现有 ≤3s+1tick 空转自退的双 daemon 窗口）。owner-pid 监督与空转自退**保留**为兜底。
- **所有权取代推断**：port-file 写前校验**调用者 uid**（新 `--owner-uid <uid>`，UI 恒传自身 uid）：unix 侧要求目标目录 `lstat` 为目录、非符号链接、**属主 == owner-uid、mode 0700**，通过后才 `.tmp-<pid>` + `rename` + `chown(owner-uid)`；不通过 ⇒ exit 2（不写、不留痕）。取代 §10.4「按目录属主推断」这一环（fd 传递的等价收紧，成本更小）。Windows 侧同理收紧 ACL（下条）。
- **Windows port-file ACL（⑦)**：创建即带仅当前用户（+SYSTEM）DACL 的 `SECURITY_ATTRIBUTES`（`SetEntriesInAclW`/`SetNamedSecurityInfoW`），并把直写升级为 `.tmp` + `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` 原子替换。
- **限流**：并发连接上限（8）、未认证连接握手超时（10s）、认证失败固定小延迟（250ms）、accept 错误退避（100ms，不紧旋）。
- **⑥ 中危四项**：TCP/stdio 行长上限 1 MiB（超限 ⇒ -32700 + 断开，日志不计内容）；accept 退避（同上）；握手超时（同上）；`lock().unwrap()` 中毒策略统一为 `unwrap_or_else(|e| e.into_inner())`（中毒不 panic，状态一致性由各临界区自守）。

### R6. 签名/公证链（④）：先诚实、可缺省、可回滚

- **macOS**：`scripts/notarize.sh` 实现为真链（逐嵌套可执行 `codesign --force --options runtime --timestamp` → `xcrun notarytool submit --wait` → `xcrun stapler staple`），由 `package-macos.sh` 在**凭据齐备时**调用；凭据走 CI secrets（`APPLE_CERT_P12_BASE64`/`APPLE_CERT_PASSWORD`/`APPLE_TEAM_ID`/`APPLE_ID`/`APPLE_APP_PASSWORD`），**缺凭据 ⇒ 跳过并打印 `skip:` 具名行 + artifact 名不变**（绝不产半签包）。**F2 修复**：`Release.entitlements` 的 `com.apple.security.app-sandbox` 置 **false**（数据恢复与沙箱不兼容：/dev/diskN、spawn 提权 helper），保留 Flutter 所需 JIT entitlement；签名后**必须校验**（`codesign --verify --deep` + `spctl -a` 因未公证必然拒——改为断言「seal 有效」= `codesign --verify` 通过）。
- **Windows**：`package-windows.ps1` 增 `signtool` 钩子（证书经 `WINDOWS_CERT_PFX_BASE64`/密码 secrets；缺 ⇒ 具名 skip）。**SmartScreen 现实**：无 EV 证书则提示必然存在——购买证书是业务决策（开放问题 O4），代码侧只保证「有证书即可签、签了即生效」。
- **⑨ 顺手**：`Runner.rc` 版本资源（CompanyName/FileDescription/ProductName/OriginalFilename/LegalCopyright 现为 com.example/xiaodun_ui）与 macOS `Info.plist` 元数据一并收口为「小盾（Xiaodun）」。

### R7. x64 物证（⑧）= `macos-15-intel` runner

- packaging 的 macos job 改**矩阵**：`macos-15`（arm64）与 `macos-15-intel`（x64，GitHub 最后一代 Intel 镜像，**2027-08 退役**——时效写进文档）；artifact 名分列 `xiaodun-macos-zip`（arm64）/`xiaodun-macos-x64-zip`；两腿各自跑冒烟。
- 现有 `package-macos.sh` 按宿主架构产物不变（矩阵天然覆盖两架构）。

### R8. 范围与 cut 线（需 lead 裁定，见「开放问题」）

- **carving 扩展（MP4/PDF/OFFICE）**：设计 §7 M2 行有此项，但本任务书基线（README M2 行 + 移交项清单）未含，M2 出口标准「三引擎可用」不依赖它 ⇒ 列为**可裁剪 Task 13**（默认排在收尾，时间不济即移 M3，届时更新设计 §7）。
- **SMAppService LaunchDaemon helper**（设计 §5.1 明文「归 M2」）：本计划**默认不纳入**（不阻塞出口；macOS 原生新代码、CI 不可验、FDA 归属需真机），仅在 T9 内做 osascript 路径的 FDA 归属文档 + 引导文案强化；若 lead 要求纳入，按 T9 附录「helper 任务骨架」单列任务并在出口文档兑现 §5.1。
- 真机手测轮（⑩）为**独立 Task 12**（验收路径 = §5 Linux 清单 + §12 总表 + M2 新增边界）。

---

## 纪律（每任务共同约束）

1. **只读铁律（类型系统级）**：两新引擎 crate 只依赖 `xd_device::BlockDevice`（无写接口）；全文件 write 面 API grep 零命中；每引擎配「扫描/读取后镜像 sha256 不变」测试。
2. **契约演进**：只按 proto/v1 README 规则（可选字段/不删改/不 deny_unknown_fields）；golden 只增不改；Rust 与 Dart 双侧 golden 断言同步。
3. **崩溃隔离**：引擎在既有 worker `catch_unwind` 边界内；每块解析不得 panic——损坏/截断镜像测试是每引擎必配项（fuzz 不固化，沿用 M1c「YAGNI + 随机构型只走浅路径」裁定；cargo-fuzz 归 M3 候选）。
4. **诚实标注**：CI 能验的必须验（编译/单测/枚举冒烟/打包产物冒烟/IOKit 真查询）；真机路径（真 UAC/授权框/TCC/真介质）一律代码注释 + 文档标「未验证（需真机）」。
5. **管线**：单写者 + 每任务 impl→spec→qual 命名代理；任务内 commit 自洽（门禁绿）；里程碑出口合并 main 前跑全矩阵 + packaging。
6. **性能底线**（设计 §4.6）：每引擎 e2e 附吞吐基线记录（读字节/秒，debug 与 release 各一），不达标须在出口文档明示。

## 前置

- M1 已完整合入 main（`2c5fc47`，v0.4.0 发版中）；契约 v1.2 / store schema v5 / TCP 提权会话 / 三平台平台层与打包脚本均在设计文档 §5.1 与 security 文档 §7-§12 记录在案。
- 起草期可用的外部 oracle 工具（开发机已具备）：`mkfs.ext4`/`debugfs`/`dumpe2fs`（e2fsprogs）、`mkntfs`/`ntfscp`/`ntfsls`/`ntfs-3g`（ntfs-3g）、`fsck.exfat`（exfatprogs，M1 先例）。

## 依赖与并行

```
T1（契约 v1.3 + 骨架）──┬─→ T2 → T3    （NTFS 线，引擎）
                        └─→ T4 → T5    （ext4 线，引擎）
T6 → T7 → T8 → T9                       （平台收口线；与引擎线文件面不相交）
T10 → T11                               （打包/签名线）
T12（真机手测 + 出口验收）← 全部
T13（可裁剪，carving 扩展）
```

- 三条线文件面互斥（引擎 = `crates/xd-fs-*`+`xd-fixtures`；平台 = `xd-daemon`+`ui`+`xd-device` 平台模块；打包 = `scripts`+`ci.yml`），**可在独立 worktree 并行**（CLAUDE.md 单写者/文件所有权规则）；`xd-core` 与 `proto/v1` 为共享热点：**T1 必须先合入**，此后 T2-T5 触碰 `xd-core` 的窗口（FsKind 分派/fs_read/scan_worker）与 T9 触碰 `export.rs` 的窗口须串行 re-base。
- `ci.yml` 由 T5（oracle 工具安装）、T11（packaging 矩阵）先后触碰；T12 做最终整合。

## 文件结构（M2 结束时的增量）

```
crates/
├── xd-fs-ntfs/                  # 新 crate：boot/mft/attr/index/bitmap/scan/read/freespace
├── xd-fs-ext4/                  # 新 crate：superblock/group/inode/dirent/extent/bitmap/journal/scan/read/freespace
├── xd-fixtures/
│   └── src/{ntfs.rs, ext4.rs}   # 两合成镜像 builder（+ journal builder）；examples/gen_{ntfs,ext4}_image.rs
├── xd-core/src/                 # api.rs(+record_id) / scan_task.rs(probe+FsKind) / scan_worker.rs / fs_read.rs / export.rs(SourceRef)
├── xd-device/src/               # windows.rs(+卷号比较/ACL 辅助) / macos.rs(+IOKit) / lib.rs(SourceRef 相关类型)
└── xd-daemon/src/               # transport.rs(加固+shutdown) / portfile.rs(owner-uid+ACL) / main.rs(--owner-uid)
proto/v1/README.md + examples/   # v1.3 增量 + 4 新 golden
ui/
├── lib/core_client/             # protocol.dart(+recordId, +shutdown) / elevation.dart(会话收口) / ipc_transport.dart
├── lib/home_page.dart           # 提权入口（①）
└── lib/features/scan/scan_controller.dart  # 复用 retryWithPrivileges 抽取
scripts/                         # notarize.sh(实现) / package-macos.sh(签名门控) / package-windows.ps1(签名钩子)
.github/workflows/ci.yml         # oracle 工具步 + packaging 矩阵（macos-15 + macos-15-intel）
docs/security/linux-privilege-model.md   # §13+ 各任务新节 + §12 表刷新
```

---

### Task 1: 契约 v1.3 + probe/FsKind/store v6 + 两引擎 crate 骨架

**Files:**
- Modify: `proto/v1/README.md`（v1.3 增量小节）、`crates/xd-core/src/api.rs`、`crates/xd-core/src/store.rs`（v6 迁移）、`crates/xd-core/src/scan_task.rs`（probe/FsKind）、`crates/xd-core/src/{scan_worker.rs,fs_read.rs}`（骨架分派臂）
- Create: `proto/v1/examples/{scan_results_ntfs.response.json, scan_results_ext4.response.json, daemon_shutdown.request.json, daemon_shutdown.response.json}`
- Create: `crates/xd-fs-ntfs/`、`crates/xd-fs-ext4/`（骨架：`Cargo.toml` + `src/lib.rs`：Error 类型 + `scan_with_observer`/`read_file_range`/`unallocated_runs` 三签名，v1 恒 `Err(Unsupported)`）
- Modify: 根 `Cargo.toml`（members）；`crates/xd-core/Cargo.toml`（+两 dependency）
- Modify: `crates/xd-core/tests/contract_v1.rs`、`ui/lib/core_client/protocol.dart`、`ui/test/protocol_v1_test.dart`

- [ ] **Step 1: README v1.3 小节**（追加，语义见 R3；含 recordId 三态说明：缺省=null，0 是合法记录号/ inode 号——沿用 byte_offset 的「不得用 0 表未知」注释口径）。
- [ ] **Step 2: golden 4 枚**（`scan_results_ntfs`：1 条 `deleted:true`、`quality:"maybeDamaged"`、`recordId:42`；`scan_results_ext4`：1 条 live + 1 条 journal 恢复（`recordId` = inode 号）；shutdown 请求/响应）。
- [ ] **Step 3: api.rs 字段**
```rust
    /// FS 元数据记录号（v1.3）：NTFS = MFT 记录号；ext4 = inode 号；fat/exfat/雕刻件恒 None。
    /// 读取路径以它重定位（与 byte_offset 分工：后者恒为雕刻件）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_id: Option<u64>,
```
- [ ] **Step 4: store v6 迁移**（照 v1→v5 先例：`PRAGMA user_version` 闸门 + `pragma_table_info` 列探测；`ALTER TABLE entries ADD COLUMN record_id INTEGER`；insert/读取补列；既有迁移测试链加 v5→v6 一枚）。
- [ ] **Step 5: probe 扩展**（唯一 1 次读放大到 2048B，三签名的判定顺序写死）：
```rust
pub fn probe(dev: &dyn BlockDevice) -> Result<FsKind, ProbeError> {
    let mut buf = [0u8; 2048];
    let n = dev.read_at(0, &mut buf).map_err(|_| ProbeError::Unsupported)?;
    if n >= 11 && &buf[3..11] == b"EXFAT   " { return Ok(FsKind::Exfat); }
    if n >= 11 && &buf[3..11] == b"NTFS    " { return Ok(FsKind::Ntfs); }   // OEM ID（VBR 偏移 3）
    // …既有 FAT BPB 粗筛（零改动）
    // ext4：超级块 magic 0xEF53 位于卷内 1024+56 = 1080（0x438）。
    // （T1 lead 裁定 2026-10-03：ext4 殿后——防 FAT16 卷 FAT1 区恰撞 0xEF53 被抢判；
    //   真 ext4 过不了 FAT 闸，真实设备判定结果不变）
    if n >= 1082 && buf[1080] == 0x53 && buf[1081] == 0xEF { return Ok(FsKind::Ext4); }
}
```
  `FsKind` 加 `Ntfs`/`Ext4` 二变体 + `as_str()`/`FromStr`（`"ntfs"`/`"ext4"`；未知仍 `ProbeError::Unsupported`）。
- [ ] **Step 6: 骨架接线**：`scan_worker` 快扫臂/`unallocated_runs_of`/`fs_read::read_entry_range` 与 `unallocated_runs` 的 match 各加两臂 → 调 crate 骨架（恒 Err → 任务 failed / -32603，**不留 `unimplemented!()`**）；`handlers` 无需改（fs 串自动透传）。
- [ ] **Step 7: 双侧契约断言**：`contract_v1.rs` 4 枚新 golden 往返（含 `record_id == Some(42)`、缺省省略）；Dart `protocol.dart` ScanEntry 加 `recordId`（`int?`）+ `protocol_v1_test.dart` 同 golden 解码断言。
- [ ] **Step 8: 门禁与提交**：`cargo test --workspace --locked && cargo clippy … && cargo fmt --check`；`cd ui && flutter analyze && flutter test`。
  commit `feat(proto,core): 契约 v1.3 增量（fs:ntfs/ext4、recordId、daemon.shutdown）+ probe 两 FS 识别 + 引擎骨架`

**测试清单**：`probe_detects_ntfs_by_oem_id`、`probe_detects_ext4_by_superblock_magic`、`probe_short_read_does_not_false_positive`（<1082 字节输入不得误判 ext4）、`record_id_roundtrips_and_defaults_null`、`v5_database_migrates_to_v6`、`existing_36_goldens_unchanged`（比对文件字节）、`unsupported_fs_skeleton_fails_cleanly`（骨架臂 → finished state=failed 不 panic）。

---

### Task 2: NTFS 引擎 A —— 夹具 builder + 引导区/MFT/属性/runlist + 目录索引 + live 扫描

**Files:**
- Create: `crates/xd-fixtures/src/ntfs.rs`（`NtfsImageBuilder`）+ `crates/xd-fixtures/examples/gen_ntfs_image.rs`
- Create: `crates/xd-fs-ntfs/src/{boot.rs,mft.rs,attr.rs,index.rs,scan.rs}`（`lib.rs` 填实；`Error` 类型与 M1 两引擎同构 `non_exhaustive`）
- Create: `crates/xd-fs-ntfs/tests/roundtrip.rs`、`crates/xd-fs-ntfs/tests/oracle_mkntfs.rs`（工具缺失自动跳过）
- Modify: `crates/xd-core/src/scan_worker.rs`（`ntfs_to_entry` 映射 + 真分派）

**夹具几何（builder 与测试共享，写进 builder 头注）**：扇区 512B、簇 4096B（8 扇区）、卷 1 MiB（256 簇）；VBR + 末尾备份 VBR；**$MFT = 簇 4 起 32 条记录**（1024B/条，含 0-15 系统记录 + 用户文件）；$MFTMirr = 簇 16；$Bitmap = 簇 2（256 位，32B，**常驻**——小卷合法）；根目录 = 记录 5（常驻 `$INDEX_ROOT`）；`$UpCase`/`$Secure`/`$Extend` 仅占位；文件簇从 32 起。**USA fixup 按规范写**（每扇区尾 2 字节 = USN，USN 存于 USA[0]）——引擎必须实现替换还原，oracle 真镜像会直接检验它。

- [ ] **Step 1: builder（含删除/复用构型）**：`add_file(path, data)`（自动分配 run、更新 `$Bitmap` 与父目录索引）、`add_file_in_clusters(data, runs)`（碎片/稀疏 run）、`add_resident_file`（<700B 常驻）、`add_dir(path)`、`delete(path)`（**清 in-use 位 + 索引项移除 + $Bitmap 位清**——模拟真实删除）、`reuse_clusters(path, other)`（把已删文件的部分簇分配给新文件并置位——制造 `maybeDamaged` 构型）、`bad_record(idx)`（magic 改 `BAAD`）。
- [ ] **Step 2: boot.rs**：VBR 解析（OEM/bytes-per-sector/sectors-per-cluster/total-sectors/$MFT LCN/$MFTMirr LCN/`clusters_per_mft_record` 有符号字节：负 = 2^-n 字节/记录）；几何合理性校验（簇 ≥ 扇区、记录大小 ∈ {512…4096}）；**结构优先判定**（不做 NTFS 版本探测）。
- [ ] **Step 3: mft.rs（USA + 记录头）**：
```rust
/// USA fixup：每扇区末 2 字节须 == USA[0]，替换为 USA[i+1]（不匹配 → 该记录废弃，不 panic）。
fn apply_usa(rec: &mut [u8], sector: usize) -> bool { /* usa_off@4, usa_cnt@6 */ }
```
  记录头：magic(`FILE`/`BAAD`)、in-use@22(0x01)、dir@22(0x02)、used_size@24、base_ref@32、record_no@44（**以扫描序号为权威**，字段仅交叉校验）。
- [ ] **Step 4: attr.rs**：属性迭代（type@0/len@4/non-resident@8/name@9/flags@12）；`$STANDARD_INFORMATION`(0x10 时间戳)、`$FILE_NAME`(0x30：parent_ref@0、时间戳、real/alloc size、flags、name_len@64、namespace@65、UTF-16LE @66；**多名字取 Win32 命名空间且 LSN 最大**)、`$DATA`(0x80 常驻 value / 非常驻 runlist@32 + alloc@40 + real@48 + init@56)；runlist 解码（头字节低半字节=长度字段字节数、高半字节=偏移字段字节数；**偏移为相对前一 LCN 的有符号增量**；偏移字段 0 字节 = 稀疏 run）。
- [ ] **Step 5: index.rs（目录索引）**：`$INDEX_ROOT`(0x90 常驻) + `$INDEX_ALLOCATION`(0xA0 非常驻，`INDX` 块 + USA) 的 `FILE_NAME` 索引项遍历（B+ 树：本引擎只做**全叶遍历**，不做查找——目录树重建只需枚举全部项；根目录记录 5 起）。
- [ ] **Step 6: scan.rs（live 枚举 + 契约映射）**：以「**从根走目录树**」产 live 条目（路径天然正确）；`$FILE_NAME` 的 real size 与 `$DATA` real size 不一致时取 `$DATA`（无 `$DATA` 如目录/空文件回退 `$FILE_NAME`）；`ext` = 最后一个 `.` 后小写；契约条目 `record_id = Some(mft 记录号)`、`first_cluster` = 首个非稀疏 run 的 LCN（无则 0）、`contiguous` 缺省省略、`quality = "complete"`；跳过系统区（记录 0-15）与 `$Extend` 子树；压缩/加密：**过滤 + stderr 计数**。
- [ ] **Step 7: core 映射**：`ntfs_to_entry`（对照 `fat_to_entry` 同构；`byte_offset: None`）。
- [ ] **Step 8: oracle 交叉验证（纪律锚点，非 CI 门禁）**：`oracle_mkntfs.rs`——
```bash
mkntfs -F -q -s 512 -c 4096 -L XD /tmp/xd-ntfs.img   # 8 MiB
ntfscp /tmp/xd-ntfs.img <file> /A.JPG; ntfsmkdir …
ntfsls -l /tmp/xd-ntfs.img                            # 参考实现输出
```
  测试断言：本引擎枚举的（名字, 大小）集合 == `ntfsls -l` 输出；对一个文件**逐字节读回 == 原文件**。工具缺失 ⇒ `eprintln!("skip: …")` 跳过（CI Linux 腿装 ntfs-3g 实跑，其余腿跳过）。产出实证锚点（mkntfs 版本 + 镜像 sha256）写进 builder 头注与 security 文档新节。
- [ ] **Step 9: 门禁与提交**：`cargo test -p xd-fs-ntfs -p xd-fixtures --locked`；commit `feat(fs-ntfs): NTFS 引擎 A——夹具 builder/VBR/MFT(USA)/属性/runlist/目录索引/live 扫描（mkntfs oracle 交叉验证）`

**测试清单**：`usa_fixup_restores_and_rejects_bad_usn`、`runlist_decodes_delta_sparse_and_multi_run`、`enumerates_live_files_with_paths_and_sizes`、`resident_data_small_file`、`fragmented_file_runs_in_order`、`filename_utf16_and_multi_name_lsn_wins`、`system_records_and_extend_are_skipped`、`corrupt_record_magic_is_skipped_no_panic`、`truncated_mft_is_honest_error`、`scan_does_not_modify_image`（sha256）。

---

### Task 3: NTFS 引擎 B —— 删除恢复 + `$Bitmap` 分级 + 读路径 + 深扫 + 端到端

**Files:**
- Modify: `crates/xd-fs-ntfs/src/{scan.rs,read.rs(新),bitmap.rs(新),freespace.rs(新),lib.rs}`
- Create: `crates/xd-fs-ntfs/tests/deleted_recovery.rs`、`crates/xd-fs-ntfs/tests/roundtrip_deep.rs`
- Modify: `crates/xd-core/src/scan_worker.rs`（深扫臂）、`crates/xd-core/tests/contract_v1.rs`（ntfs golden 端到端）
- Modify: `crates/xd-device/tests/loop_e2e.rs` 或 `scripts/e2e-loop.sh`（NTFS 腿，见 Step 6）

- [ ] **Step 1: `$MFT` 全表扫描 + 删除项**：经记录 0 的 `$DATA` runlist 读 MFT（**不假设连续**）；遍历到 `$MFT` 末尾（real size）；`!in-use` ⇒ 删除项；名字/父引用/大小同 T2 口径；**父链重建**（≤256 层 + 环检测）：断链（父记录不可读/越界）⇒ 该段起路径弃用、`path = "/"` 并计入 stderr 统计（名字/大小/数据不受影响）；根链经过系统区 ⇒ 跳过。
- [ ] **Step 2: bitmap.rs + 分级**：`$Bitmap`（记录 6）常驻/非常驻两形态；删除项逐簇查位——**0 簇置位 = complete；任一置位 = maybeDamaged**（`$Bitmap` 不可读 ⇒ 该任务降级：全删除项 `maybeDamaged` + stderr 留痕，**不静默 complete**）。
- [ ] **Step 3: read.rs**：
```rust
pub fn read_file_range(dev: &dyn BlockDevice, mft_rec: u64, offset: u64, length: u64)
    -> Result<Vec<u8>, NtfsError>   // 内部：recordId → 重解析 MFT 记录 → $DATA
```
  常驻 ⇒ 记录内切片；非常驻 ⇒ runlist 定位 + 稀疏补零 + `[initialized_size, real_size)` 补零（VDL 语义）；`offset` 越尾 = 空交付（对齐 M1 口径）；记录已被复用（magic/属性不符原条目）⇒ 诚实短交付/空（不猜）。
- [ ] **Step 4: fs_read/导出接线**：`fs_read.rs` 的 `FsKind::Ntfs` 臂（反构造用 `record_id` 而非 `first_cluster`；`record_id` 缺失的旧行 ⇒ `ReadError::Internal`——绝不按 `first_cluster` 猜读）；导出 worker 零改动即通（`read_entry_range` 单一出口）。
- [ ] **Step 5: freespace.rs（深扫）**：`$Bitmap` 空闲簇 → 有序不相交字节区间（`MAX_RUNS=100_000` 截断同款；`$Bitmap` 不可读 ⇒ `Err` ⇒ -32005，**绝不在未知分配上雕刻**）。
- [ ] **Step 6: 端到端三件**：
  1. `roundtrip_deep.rs`：builder 造「已删 JPG 落在空闲簇」→ 快扫（删除项名字/大小/quality 与 `$Bitmap` 构型逐条断言）→ 深扫（`mode:"deep"`，carved 恰 1 条、逐字节回读 == 埋点）。
  2. `e2e-loop.sh` 增 NTFS 段（Linux 环回，**同现有 FAT 段结构**）：`gen_ntfs_image` 镜像 → `losetup -r` → daemon `scan.start`（`fs:"ntfs"`）→ results 断言删除项 → deep 断言 carved。
  3. 导出全链：真 daemon（`XD_DAEMON_BIN`）扫描 → `fs.read` 预览切片 == 原字节 → `export.start` 导出逐字节比对（复用 M1d 集成测试骨架，加 NTFS 夹具）。
- [ ] **Step 7: 门禁与提交**：commit `feat(fs-ntfs): 删除恢复（$MFT 全表）+ $Bitmap 覆盖分级 + 读路径 + 深扫 + 端到端`

**测试清单**：`deleted_record_recovered_with_name_size_timestamps`、`bitmap_all_free_is_complete_reused_is_maybe_damaged`、`bitmap_unreadable_degrades_honestly`、`parent_chain_cycle_and_depth_terminates`、`orphan_parent_falls_back_to_root_path`、`resident_deleted_file_reads_from_record`、`vdl_zero_fill_between_initialized_and_real`、`sparse_run_delivers_zeros`、`read_after_record_reuse_is_honest_short`、`freespace_sorted_disjoint_and_never_allocated`（不变量同 M1c）、`deep_scan_carves_deleted_jpg_exactly_once`、`export_roundtrip_bytes_equal`、`scan_and_export_do_not_modify_image`。

---

### Task 4: ext4 引擎 A —— 几何/块组/inode/目录树/live 扫描/深扫 + 读路径

**Files:**
- Create: `crates/xd-fixtures/src/ext4.rs`（`Ext4ImageBuilder`）+ `examples/gen_ext4_image.rs`
- Create: `crates/xd-fs-ext4/src/{superblock.rs,group.rs,inode.rs,dirent.rs,extent.rs,bitmap.rs,read.rs,scan.rs,freespace.rs,lib.rs}`
- Create: `crates/xd-fs-ext4/tests/{roundtrip.rs,oracle_mke2fs.rs}`（工具缺失自动跳过）
- Modify: `crates/xd-core/src/{scan_worker.rs,fs_read.rs}`（ext4 分派 + `ext4_to_entry`）

**夹具几何**：块 1024B、卷 8 MiB、`inode_size=256`、`inodes_per_group` 小值、**含 flex_bg + 64bit + extent + metadata_csum 特征位**（现代 mkfs 默认，引擎必须吃得下）；一个块组起步（不够就两组，覆盖跨组 inode）。特征门（**诚实拒绝，不猜**）：`bigalloc`/`encrypt`/`inline_data`/`meta_bg`/无 `extent` 特征 ⇒ 解析期 `Err` ⇒ 任务 failed + stderr 明示（文档记录 v1 边界）。

- [ ] **Step 1: builder**：超级块（偏移 1024，magic@0x38=0xEF53；`s_log_block_size`@24、`s_blocks_per_group`@32、`s_inodes_per_group`@40、`s_inode_size`@88、`s_feature_incompat`@96、`s_desc_size`@254、`s_blocks_count_hi`@0x150）、块组描述符表（1024B 块 ⇒ 块 2 起；`bg_block_bitmap_lo`@0/`bg_inode_bitmap_lo`@4/`bg_inode_table_lo`@8 + hi@32/36/40）、inode 表、块位图/inode 位图、根目录（inode 2）、`add_file/add_dir/delete`（**delete = 目录项 inode 置 0 + rec_len 并入前项 + inode 表项 links=0/dtime=now + inode 位图清 + 数据块位图清 + `i_block` 清零**——R1 语义的夹具侧忠实复刻）、`add_file_in_extents(data, runs)`。
- [ ] **Step 2: superblock.rs/group.rs**：几何（`blocks_count = lo | hi<<32`；组数 = ⌈(blocks−first_data_block)/blocks_per_group⌉）；描述符步长 32/64（按 `s_desc_size`）；**flex_bg 透明**（描述符给的位图/inode 表位置可直接跨组，不做「按组序推位置」的假设）。
- [ ] **Step 3: inode.rs**：`i_mode`@0、`i_size` lo@4 / hi@108（64bit 正规文件）、`i_links_count`@26、`i_blocks`@28、**`i_dtime`@20**、`i_flags`@32、`i_block[60]`@40；inode 定位 = `bg_inode_table[g]·块大小 + idx·inode_size`。
- [ ] **Step 4: extent.rs**：头 `eh_magic@0=0xF30A`/`eh_entries@2`/`eh_max@4`/`eh_depth@6`；叶项 `ee_block@0(u32)/ee_len@4(u16，>0x8000=未初始化)/ee_start_hi@6/ee_start_lo@8`；**depth 0 与 depth 1 都要**（大文件跨叶块，M2 实证 ext4dfr 同款要求）；未初始化 extent 交付零；洞（hole）补零。
- [ ] **Step 5: dirent.rs + scan.rs（live）**：块内链表 `inode@0(u32)/rec_len@4(u16)/name_len@6(u8)/file_type@7/name`；**htree 目录**：只做「全部数据块线性扫描 + 跳过 `inode==0`/`inode>s_inodes_count` 的 dx 元数据槽」——不做 dx 索引查找（重建目录树只需枚举）；live = 从根 inode(2) 走目录树（路径天然正确、htree 不外例外）；契约映射：`record_id = Some(inode 号)`、`first_cluster` = 首个 extent 的起始块号、`ext` 同口径、跳过保留区（inode < `s_first_ino` 且非 2）。
- [ ] **Step 6: read.rs**：inode → extent（depth 0/1）→ 读；`i_size` 截断；洞/未初始化 → 零；`read_file_range(dev, inode_no, offset, length)` 与 NTFS 同签名口径；`fs_read`/导出接线（`record_id` 缺失 ⇒ Internal）。
- [ ] **Step 7: bitmap.rs + freespace.rs（深扫）**：逐组块位图 → 空闲块合并为字节区间（`MAX_RUNS` 截断；位图不可读 ⇒ Err ⇒ -32005）。
- [ ] **Step 8: oracle 交叉验证**：`oracle_mke2fs.rs`——`mkfs.ext4 -b 1024 -I 256 -O 64bit,extent,flex_bg,metadata_csum`（8MiB）→ `debugfs -w -R "write …"`/`"mkdir"` 造文件（免 root）→ `dumpe2fs`/`debugfs -R "ls -l"` 为参考实现；断言枚举集合与逐字节读回。工具缺失 ⇒ skip（CI Linux 腿安装 e2fsprogs 实跑）。
- [ ] **Step 9: 门禁与提交**：commit `feat(fs-ext4): ext4 引擎 A——几何/块组/inode/目录树/extent 读/深扫（mke2fs+debugfs oracle 交叉验证）`

**测试清单**：`superblock_geometry_and_64bit_counts`、`refuses_bigalloc_and_encrypt_honestly`、`descriptor_stride_32_and_64`、`flex_bg_layout_is_transparent`、`extent_depth0_and_depth1_reads`、`uninitialized_extent_and_hole_zero_fill`、`dirent_linear_scan_skips_dx_metadata`、`live_tree_walk_names_paths_and_sizes`、`block_bitmap_free_runs_invariants`、`corrupt_superblock_is_honest_error_no_panic`、`scan_does_not_modify_image`。

---

### Task 5: ext4 引擎 B —— jbd2 journal 删除恢复（**M2 最大不确定项；cut 线见 R1**）

**Files:**
- Create: `crates/xd-fs-ext4/src/journal.rs`、`crates/xd-fs-ext4/tests/{journal_recovery.rs,kernel_delete_e2e.rs}`
- Modify: `crates/xd-fixtures/src/ext4.rs`（journal builder）、`crates/xd-fs-ext4/src/scan.rs`（删除恢复合并）、`crates/xd-core/src/scan_worker.rs`（如需）
- Modify: `.github/workflows/ci.yml`（Linux 腿装饰件；见 Step 5）

- [ ] **Step 1: 内核删除语义实证 gate（先跑，再写码）**：`tests/kernel_delete_e2e.rs`（`#[cfg(target_os="linux")]` + 免密 sudo 探测，缺失 ⇒ `eprintln!("skip: 无免密 sudo")`，CI ubuntu 实跑）：`mkfs.ext4` 小卷 → `losetup` + mount → 写 3 文件 → `rm` → umount → `dd` 快照镜像 → **直接检查被删 inode 的 `i_block`（断言 extent 清零 + dtime 置位）与目录块（断言 dirent inode=0）**。该测试**即是 R1 前提的证据**（ran:/skip: 二值取证口径同 T4 先例）。若清零不成立：删除分支追加 dtime 快路（R1 尾注），journal 仍取名字。
- [ ] **Step 2: journal.rs 解析**：journal inode(8) 的 `$DATA`（常驻/非常驻）→ journal 超块（`h_magic@0 = 0xC03B3998` BE、`h_blocktype@4`、v2 超块 `s_blocksize@12`/`s_maxlen@16`/`s_first@20`/`s_sequence@24`/`s_start@28`）→ 环形顺序遍历：描述符块（type=1，tags: `t_blocknr` BE u32 + `t_flags`：0x1 SAME_UUID / 0x2 LAST_TAG / 0x4 ESCAPE；metadata_csum 下末 4B 为 `t_checksum`——**v1 不校验**，文档记录）→ 提交块（type=2）→ 撤销块（type=5，记录已撤销块号，撤销的 tag 其副本无效）→ 收尾回到 `s_start` 或首块非法即停。
- [ ] **Step 3: 覆盖层（两趟，内存有界）**：
  - **Pass A**：live 扫描先跑（T4 成果）→ 建立「感兴趣块集合」= 各目录的数据块 ∪ 各块组的 inode 表块区间；
  - **Pass B**：journal 环形顺序扫（老→新），tag 命中感兴趣块 ⇒ 存入 `HashMap<u64, Vec<u8>>`（**后写覆盖 = 取最新旧副本**——即删除前一刻版本）；撤销块覆盖时移除；上限（如 4096 块）超出 ⇒ 诚实截断 + stderr 留痕。
  - **Pass C**：对 map 内每个 inode 表块逐槽解析：旧副本 inode 有效（mode/links 合理）且 live 同位 inode 已删（links=0 或 dtime≠0 或位图空闲）⇒ **恢复候选（inode 号 N）**；对 map 内每个目录块逐 dirent 解析：`inode=0` 在 live、`inode=N` 在旧副本 ⇒ **名字 + 父目录**。**两源交集（N 相等）才产条目**——单源不产（避免假阳性）。
- [ ] **Step 4: 分级 + 条目**：`recordId = inode 号`；size/timestamps ← 旧 inode；数据 ← 旧 extent（读路径在 `read.rs` 内**重跑 journal 还原该 inode**——确定性重放，per-slice 成本记 `ponytail:` 注释，同 fs_read 既有口径）；quality：extent 覆盖的块**全部在 live 块位图中空闲** ⇒ `complete`；任一已被复用 ⇒ `maybeDamaged`（与 NTFS `$Bitmap` 分级同语义）。**无命中 ⇒ 空结果，绝不产猜测条目**（R1）。
- [ ] **Step 5: 夹具 + 端到端**：
  1. builder 侧手写一个合成 journal（描述符/提交/撤销各至少一枚 + 覆盖 inode 表与目录块）⇒ 确定性 CI 测试 `journal_recovery.rs`（三种构型：完整命中 / 部分块被复用 → maybeDamaged / 撤销块 → 空）。
  2. `kernel_delete_e2e.rs` 扩展为**恢复断言**：真内核删除 → 本引擎快速扫描 → 断言 3 个文件名 + 大小 + 逐字节数据全部找回（这是 M2 ext4 的「真删除 → 扫到 → 读回」出口证明，Linux CI 实跑）。
  3. `e2e-loop.sh` 增 ext4 段（环回 + 深扫 carved 断言）。
- [ ] **Step 6: 门禁与提交**：commit `feat(fs-ext4): journal（jbd2）覆盖层删除恢复——名字/大小/时间戳/extent 重建 + 内核删除 e2e`

**测试清单**：`journal_superblock_v2_fields`、`descriptor_tags_and_revoke_handling`、`overlay_newest_copy_wins`、`recovered_entry_requires_name_and_inode_intersection`、`reused_blocks_downgrade_to_maybe_damaged`、`revoked_block_yields_no_entry`、`journal_absent_or_stale_is_honest_empty`、`kernel_delete_recovers_names_sizes_bytes`（sudo 门控）、`overlay_memory_bound_truncates_honestly`。

---

### Task 6: Windows 首页提权入口（①）+ 非提权枚举评估

**Files:**
- Modify: `ui/lib/home_page.dart`、`ui/lib/features/scan/scan_controller.dart`（抽取共享提权流）、`ui/lib/main.dart`（client 替换接线）
- Create: `ui/test/home_elevation_test.dart`
- Modify: `packaging/windows/README-安装.txt`（提权入口描述改写）、`README.md`（已知限制行改写）

- [ ] **Step 1: 抽取共享提权流**：把 `scan_controller.retryWithPrivileges()` 的核心（会话目录 0700 → `ElevationPlan` → `spawnElevation` → 轮询 port-file ≤30s/500ms（**半行双形态重试**）→ `connectElevatedSession` → `ping` 探针 → 换 client → 回调）抽为 `core_client/elevation.dart` 内的 `Future<CoreClient> elevateSession({...})`，scan_controller 改为薄调用（行为零变化，既有 15 枚 elevation 测试红线不动）。
- [ ] **Step 2: 首页入口**：设备列表为空且平台支持提权（Win/macOS/Linux 三平台同一分支，`debugDefaultTargetPlatformOverride` 可测）时，展示「**以管理员身份重启引擎**」按钮 + 说明文案；点击 → `elevateSession`（`--owner-pid` = UI pid，`daemonPath` 走 `packagedDaemonPath()`）→ `onClientReplaced` → `_reload()` 重列设备；失败 ⇒ 既有文案「未获得授权（原因）」；macOS 追加「已提权仍失败 ⇒ 完全磁盘访问」提示（复用既有 FDA 文案常量）。**旧 client 处置**：非提权 daemon 为 UI 子进程 ⇒ 显式关闭其 stdin（等价退出），再换新 client。
- [ ] **Step 3: 非提权枚举评估（一步，产出结论不产码）**：查证 Windows 0-access（desired access = 0）打开 `\\.\PhysicalDriveN` + `IOCTL_STORAGE_QUERY_PROPERTY` 是否可在非提权下列盘（含能否拿容量；`IOCTL_DISK_GET_LENGTH_INFO` 需读权限 ⇒ 大概率拿不到）。结论写进 security 文档新节；**可行且代价小 ⇒ 追加实现（列盘 + 容量 0，UX 走提权后才补全）**，否则明确「入口方案 A 定稿」不再评估。
- [ ] **Step 4: 门禁与提交**：widget 测试：空列表 + Windows 平台 ⇒ 按钮出现；点击走假启动器 + 假 port-file ⇒ client 被替换、设备重列；非空列表 ⇒ 无按钮；授权取消 ⇒ 文案 + 不换 client。commit `feat(ui): Windows 首页提权入口（复用提权会话流）+ 空列表引导`

**测试清单**：`empty_device_list_offers_elevation_entry`、`elevation_from_home_replaces_client_and_reloads`、`cancel_keeps_old_client_with_reason`、`nonempty_list_hides_entry`、`scan_controller_still_uses_same_flow`（抽取零回归，既有 15 枚全绿）。

---

### Task 7: 提权会话加固 —— ⑥ 四项 + 限流 + shutdown RPC 消费 + 会话收口（⑤）

**Files:**
- Modify: `crates/xd-daemon/src/{transport.rs,main.rs}`、`crates/xd-daemon/tests/tcp_session.rs`
- Modify: `ui/lib/core_client/{core_client.dart,ipc_transport.dart,elevation.dart}`、`ui/lib/main.dart`、`ui/test/socket_transport_test.dart`

- [ ] **Step 1: 行长上限（1 MiB）**：`serve_lines` 的读取改为**有界读行**（`take`/手写累积，超限 ⇒ 回 `-32700` 后断开；错误日志**不打印内容**，只打印长度）；stdio 与 TCP 同一条路径（一处修改两模式生效）。
- [ ] **Step 2: accept 退避 + 并发上限 + 失败延迟**：accept 错误（EMFILE 等）⇒ sleep 100ms 再试（不紧旋）；已认证连接数上限 8（超出 ⇒ 立即断开，stderr 计数）；认证失败回 -32001 前 sleep 250ms（人为成本压制在线猜测）。
- [ ] **Step 3: 握手超时 10s**：首行 auth 逾时 ⇒ 直接断开（不读、不回）。
- [ ] **Step 4: 中毒策略**：全仓 `.lock().unwrap()` 审计（daemon + core）→ 统一 `unwrap_or_else(|e| e.into_inner())`；各临界区自守性注释（写半状态不可达论证）；中毒注入测试一枚（子线程 panic 毒化 → 主线程仍可读写）。
- [ ] **Step 5: shutdown RPC 落地**：`main.rs` 的 serve 循环收到 `daemon.shutdown` ⇒ 回 `{"accepted":true}` ⇒ 触发既有 `exit_cleaning`（清 port-file、exit 0，stdout 零写）；`daemon.shutdown` 在 `handlers`（或其他方法表）注册。
- [ ] **Step 6: UI 会话收口**：`CoreClient` 增 `Future<void> shutdown()`（IPC 客户端发 RPC 后关连接；`_MissingDaemonClient` 空实现）；调用点：① 换 client 时先 shutdown 旧 client（**替换后旧 root daemon 立即退**，双 daemon 窗口收窄到 ≪3s）；② `XiaodunApp.dispose`（best-effort，超时 2s 不阻塞退出）。
- [ ] **Step 7: 门禁与提交**：commit `feat(daemon,ui): 会话加固（行长上限/退避/握手超时/中毒策略/限流）+ daemon.shutdown 与 UI 会话收口`

**测试清单**：`overlong_line_is_rejected_without_echo`、`handshake_silence_times_out`、`concurrent_connections_capped`、`failed_auth_is_delayed`、`poisoned_lock_does_not_panic`、`shutdown_rpc_replies_then_exits_and_cleans_port_file`（复用 owner-dies 清理测试骨架）、`stdio_mode_also_accepts_shutdown`、Dart：`client_shutdown_sends_rpc_and_closes`、`client_replacement_shuts_down_previous`。

---

### Task 8: port-file 所有权与 ACL（⑤ 所有权 + ⑦ Windows ACL）

**Files:**
- Modify: `crates/xd-daemon/src/{portfile.rs,main.rs}`、`crates/xd-daemon/tests/tcp_session.rs`（unix uid 腿）、新 `crates/xd-daemon/tests/portfile_windows.rs`（cfg(windows)）
- Modify: `ui/lib/core_client/elevation.dart`（构造命令带 `--owner-uid`）、`ui/test/elevation_test.dart`（KAT 加参数断言）

- [ ] **Step 1: unix 所有权校验取代目录属主推断**：`main.rs` 增 `--owner-uid <uid>`（须与 `--listen/--port-file` 同组出现，单独给出 ⇒ exit 2）；`portfile.rs` 写前校验（`openat(O_NOFOLLOW|O_DIRECTORY)` + `fstat`）：目标目录为**目录**、**非符号链接**、**属主 == owner-uid**、**mode & 0o077 == 0**；任一条不满足 ⇒ exit 2（stderr 具名原因，不写任何文件）。通过后 `adopt_owner_of_dir` 的「按目录属主」推断**删除**，`chown` 目标恒为 `owner-uid`（非 root 时此步 no-op，且此时 owner-uid 必须 == 自身 uid，否则 exit 2）。**回退形态（仅当实施期否决 Step 3 子进程取 uid 时启用）**：参数缺省 ⇒ 校验退化为「0700 + 非符号链接 + 属主 == 写前 stat 的目录属主」并在文档记为 M2 接受项；两种形态择一后写死，不得两存。
- [ ] **Step 2: Windows ACL 收紧 + 原子写**：`CreateFileW` 带 `SECURITY_ATTRIBUTES`（DACL = 当前用户 FULL + SYSTEM，经 `SetEntriesInAclW`）；写入升级 `.tmp-<pid>` + `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`（原子替换，读侧不再有半行形态之一——半行重试逻辑保留为纵深）。
- [ ] **Step 3: UI 传参**：`ElevationPlan` 构造增 `--owner-uid <uid>`（unix 三平台；uid 取法 = 提权引导时启动一次 `id -u` 子进程，**已评估并接受**：非热路径、一次性；Windows 跳过该参数，走 ACL）。KAT 钉命令形态；elevation_test 用注入的 fake runner 保证测试不真起子进程）。
- [ ] **Step 4: 门禁与提交**：commit `feat(daemon): port-file 所有权校验（0700/非链接/属主）+ Windows DACL 与原子写`

**测试清单**（unix，CI 实跑）：`port_file_dir_must_be_0700_and_not_symlink`、`port_file_dir_owner_mismatch_is_rejected`（需要 root 造他人属主目录 ⇒ 免密 sudo 门控 + ran:/skip: 口径）、`non_root_write_unchanged_semantics`（回归：属主 == 自身 ⇒ 不 chown、0600、无 .tmp 残留）；Windows：`port_file_acl_is_current_user_only`（`GetNamedSecurityInfoW` 断言或 icacls 解析）、`tmp_rename_replace_is_atomic`。

---

### Task 9: 导出与同源校验平台面收口 —— Windows 卷号（②）+ macOS IOKit（③）+ Win 取消/余量

> 任务偏大（三块：②③ + Windows 功能缺口）：**可拆 T9a（Windows）/T9b（macOS）**，拆分点已按 Step 标注。

**Files:**
- Modify: `crates/xd-device/src/{windows.rs,windows/enumerate.rs(如需)}`、`crates/xd-device/src/macos.rs`、`crates/xd-device/src/lib.rs`（`SourceRef` 或等效类型）
- Modify: `crates/xd-core/src/export.rs`（签名改 `source: &SourceRef`；三平台分派）、`crates/xd-daemon/src/export_worker.rs`（调用点）、`crates/xd-core/src/export.rs` 测试、
- Create: `crates/xd-device/tests/{windows_volume_disk.rs,macos_iokit.rs}`（cfg 门控）
- Modify: `docs/security/linux-privilege-model.md`（§8/§9 缺口行闭合）

- [ ] **Step 1（两平台共用）**：`check_target` 参数 `source_rdev: Option<(u64,u64)>` → `source: &SourceRef { id: &str, rdev: Option<(u64,u64)> }`；调用点（父侧 + `export_worker`）传 `device_id`。unix 既有两道逻辑**逐字不变**（回归红线）。
- [ ] **Step 2（T9a · Windows 同源校验②）**：`windows.rs` 增纯函数 + 薄实现：
```rust
/// 目标目录所在卷 → 盘号；源 = `win:\\.\PhysicalDriveN` 的 N。两者相等 ⇒ 同盘。
/// 打开卷需读权限：物理盘源必为提权上下文（导出 worker 为提权 daemon 子进程）⇒ 可达；
/// 失败（非提权/异常）⇒ warn + 放行（fail-open + 留痕，与 unix sysfs 缺位同口径）。
pub fn target_on_same_physical_disk(source_id: &str, target: &Path) -> Option<bool>
```
  实现：`GetVolumePathNameW` → `\\.\X:` → `CreateFileW(GENERIC_READ)` → `IOCTL_STORAGE_GET_DEVICE_NUMBER`（`DeviceNumber`）；源盘号从 id 解析。`export.rs` 的 `#[cfg(windows)]` 臂调用（**替换现「非 unix 且 source_rdev.is_some() ⇒ PlatformUnsupported」只对 windows 收窄**；macOS 走 unix 臂 + Step 4）。
- [ ] **Step 3（T9a · Windows 取消 + 余量预检）**：`export.rs` `cancel()` 的 `#[cfg(not(unix))]` 臂改实：`OpenProcess(PROCESS_TERMINATE)` + `TerminateProcess`（pid 来自既有 `Job.pid`；stale-pid 窗同 unix 记录在案，pidfd 等价物归 M4）；余量预检 `#[cfg(windows)]` 臂：`GetDiskFreeSpaceExW(target_dir)` → `-32010` 判定（与 unix `statvfs` 同语义同口径）。
- [ ] **Step 4（T9b · macOS IOKit③）**：`macos.rs` 增：
```rust
#[link(name = "IOKit", kind = "framework")]
extern "C" { fn IOServiceMatching(..) -> ..; fn IOServiceGetMatchingServices(..); /* + CoreFoundation 最小面 */ }
/// 挂载卷 → 整盘 BSD 名（IOMedia 父子链上溯至整盘）；`/dev/disk3s5` → `disk3`。
pub fn whole_disk_of_path(path: &Path) -> Option<String>   // statfs.f_mntfromname 起步
/// 源 `unix:/dev/diskN` 与目标卷整盘名相等 ⇒ 同盘（封「源=整盘/目标=其分区」盲区）。
pub fn target_on_same_disk(source_id: &str, target: &Path) -> Option<bool>
/// transport/removable 映射（kIOMediaRemovable + 设备特征字符串 → usb|sata|nvme|other）。
pub fn media_hints(dev: &str) -> (Option<&'static str>, Option<bool>)
```
  `export.rs` 的 macOS 臂 = unix 既有两道 + **追加 IOKit 道**（IOKit 不可解析 ⇒ warn + 放行，两平台同口径）；`macos` 枚举侧接 `media_hints`（`DeviceInfo.transport/removable` 由恒 `None`/`false` 转真值）。**SMAppService helper：默认不做**（R8；出口文档明示 osascript≠FDA 与 FDA 授权建议，T9 只改文案与文档）。
- [ ] **Step 5: 测试（CI 两 runner 真跑）**：
  - Windows：`parses_physical_drive_id`、`volume_disk_number_matches_system_drive`（目标 = `%TEMP%` ⇒ 盘号 == 系统盘且与 `\\.\PhysicalDrive0` 比较结果稳定）、`attach_boot_volume_requires_elevation_warns_otherwise`（非提权臂 ⇒ None + warn 不 panic）；`cancel_terminates_worker`（起假子进程 sleep → cancel → 断言终止）；`free_space_check_rejects_absurd_estimate`（estimate = u64::MAX ⇒ -32010 语义）。
  - macOS：`iokit_resolves_volume_to_whole_disk`（真查询 runner 根卷 ⇒ `diskNsM` 的整盘名，与 `diskutil info` 同值可选手工对照）、`same_disk_true_for_own_disk_false_for_other`、`transport_mapping_table`（纯函数 KAT）、`unparseable_id_is_none_not_panic`。
  - 共用：既有 unix -32006 三道注入测试**全绿红线**（签名改动零回归）。
- [ ] **Step 6: 门禁与提交**：commit `feat(device,core): 同源校验平台面——Windows 卷号比较 + macOS IOKit 归属/transport + Windows 取消与余量预检`

---

### Task 10: 签名/公证链（④）+ F2 entitlements + Runner.rc（⑨）

**Files:**
- Modify: `scripts/notarize.sh`（实现）、`scripts/package-macos.sh`（凭据门控调用）、`scripts/package-windows.ps1`（signtool 钩子）、`scripts/e2e-package-smoke.sh`（断言签名状态如实）
- Modify: `ui/macos/Runner/Release.entitlements`（sandbox=false）、`ui/macos/Runner/DebugProfile.entitlements`（对齐评估）、`ui/macos/Runner/Info.plist`（元数据）、`ui/windows/runner/Runner.rc`（元数据）
- Modify: `.github/workflows/ci.yml`（secrets 注入到 packaging jobs）、`README.md`（macOS 签名段）、`docs/security/linux-privilege-model.md`（§11 更新）

- [ ] **Step 1: notarize.sh 真实现**：入参 = `.app` 路径与产物 zip；步骤 = ① 逐嵌套可执行签名（**先 `Contents/MacOS/xd-daemon`，再 Frameworks，最后整个 `.app`**，`--force --options runtime --timestamp --sign "Developer ID Application: …"`；**不用 `--deep`**）；② `codesign --verify --strict` 断言 seal 有效（**F2 的验收点**）；③ `xcrun notarytool submit --wait`（`--apple-id/--team-id/--password` 或 API key，全走环境变量）；④ `xcrun stapler staple` + `spctl -a -vv` 断言 accepted。凭据缺失 ⇒ `echo "skip: …"` 退出 0（**绝不产半签包**：跳过时清晰打印「本产物未签名/未公证」）。
- [ ] **Step 2: package-macos.sh 接线**：打包后（staged + ditto 前）调用 notarize 流程（顺序：build → codesign → zip → notarize(zip) → staple → 重新 ditto 出最终 artifact，或 staple .app 后 ditto——**按 notarytool 官方顺序实现并如实记录**）。
- [ ] **Step 3: Windows 签名钩子**：`package-windows.ps1` 在 zip 前对 `xiaodun.exe`/`xd-daemon.exe` 跑 `signtool sign /fd SHA256 /tr <ts> /td SHA256`（PfxBase64/密码 secrets；缺失 ⇒ 具名 skip 并打印 SmartScreen 提示句）。
- [ ] **Step 4: entitlements + 元数据**：`Release.entitlements`：`app-sandbox=false`（删该键），保留 `allow-jit`（Flutter 运行时需要）；`DebugProfile.entitlements` 对齐（保 JIT）；`Info.plist` 的 `CFBundleName/CFBundleDisplayName/CFBundleIdentifier`（`com.erik.xiaodun`）/`NSHumanReadableCopyright`；`Runner.rc`：CompanyName「Xiaodun」、FileDescription/ProductName「小盾 (Xiaodun)」、OriginalFilename `xiaodun.exe`、InternalName、LegalCopyright「© 2026 erik · https://erik.xyz」。
- [ ] **Step 5: CI**：packaging jobs 注入 secrets（`APPLE_*`/`WINDOWS_*`，仓库 secrets 由 lead 配置——**开放问题 O5**）；`e2e-package-smoke.sh` 增补一行：签名状态如实打印（`codesign -dv` 结果，签/未签都过但留证据）。
- [ ] **Step 6: 门禁与提交**：本地（无凭据）跑 `bash scripts/notarize.sh /tmp/fake.app` ⇒ 具名 skip 且 exit 0；`bash scripts/package-macos.sh` 无 secrets 路径产物与 M1e 行为一致（回归）；commit `feat(packaging): macOS 签名/公证链 + F2 entitlements 修复 + Windows 签名钩子与版本资源收口`

**测试清单**：`notarize_script_skips_named_without_credentials`、`notarize_script_orders_nested_sign_first`（纯函数/脚本级断言或 mock codesign）、`Release_entitlements_sandbox_is_false`（文件断言）、`runner_rc_metadata_is_xiaodun`、`smoke_reports_signature_state`。**未验证（需真机 + 真证书）**：真 notarytool 提交/公证通过、Gatekeeper 首开、SmartScreen。

---

### Task 11: macOS x64 物证与 packaging 矩阵（⑧）

**Files:**
- Modify: `.github/workflows/ci.yml`（`package-macos` job → `strategy.matrix`）、`scripts/package-macos.sh`（artifact 命名含架构，已是）、`README.md`（打包节）
- Create: （无新文件；产物 = `dist/xiaodun-vX.Y.Z-macos-{arm64,x64}.zip`）

- [ ] **Step 1: 矩阵**：
```yaml
  package-macos:
    if: github.event.inputs.packaging == 'true'
    strategy:
      fail-fast: false
      matrix:
        include:
          - runner: macos-15        # arm64
            arch: arm64
          - runner: macos-15-intel  # x64（GitHub 最后一代 Intel 镜像，2027-08 退役）
            arch: x64
    runs-on: ${{ matrix.runner }}
```
  冒烟两腿各跑；upload-artifact 名 `xiaodun-macos-${{ matrix.arch }}-zip`（**保留** `xiaodun-macos-zip` 到 arm64 腿的兼容映射？——裁定：**不保留**，直接更名并在 README 记录；手动 job 无既有消费者）。
- [ ] **Step 2: docs**：README 打包节 + 已知限制：x64 物证获取路径、Intel 镜像 2027-08 退役时效、跨架构（arm64 构建的包不能在 Intel 上跑）说明。
- [ ] **Step 3: 门禁与提交**：dispatch 一轮 `packaging=true` 实证两腿（含 T10 的签名跳过路径）；commit `ci(packaging): macOS 打包矩阵（macos-15 + macos-15-intel）——x64 物证落地`

---

### Task 12: 真机手测轮（⑩）+ 出口验收

**Files:**
- Modify: `docs/security/linux-privilege-model.md`（§12 总表刷新 + 各节「未验证」清单刷新 + M2 新节）、`README.md`（项目状态/已知限制/平台矩阵）、`docs/superpowers/specs/2026-10-02-xiaodun-design.md`（§5.1 实现注记续写、§4.2 ext4 勘误落定、§7 若 cut 则更新）、`Cargo.toml`/`ui/pubspec.yaml`（版本 0.5.0）
- 计划内「执行记录」小节（本文档尾部）由 lead 在收口时填写

- [ ] **Step 1: 版本与文档**：workspace + pubspec 版本 → `0.5.0`；设计文档 §4.2 以 R1 勘误落定（**ext4 删除恢复 = journal**）；§5.1 实现注记续写 M2 一条。
- [ ] **Step 2: 全矩阵 CI**：dispatch CI（`packaging=true`）逐 job 核验（`gh run view <id> --json jobs`）：rust 三腿（含 oracle 工具实跑、sudo 门控测试 ran: 取证）、flutter、deb、e2e/e2e-loop、packaging 三腿（win + mac arm64 + mac x64）。**本步与 T5/T9/T11 的取证行一并核验**。
- [ ] **Step 3: 真机手测（⑩，按清单执行并逐项记录）**：
  - **Windows 真机**：① 非提权启动 → 首页空列表 → 提权入口 → UAC → 设备出现（①的真机闭环）；② 真 U 盘（NTFS）删除照片 → 扫到 → 预览 → 恢复逐字节；③ 导出目标选源盘分区 ⇒ **-32006 拦截**（②）；④ 导出取消真的停；⑤ `icacls` 核对 port-file ACL（⑦）；⑥ SmartScreen/签名现状；⑦ 无 VC++ 运行库机器首启。
  - **macOS 真机**：① osascript 授权 + FDA 授权链（授权后仍 EPERM 的文案）；② 真盘（APFS 容器下的 ext4/NTFS 不适用，改：真外接盘 NTFS/exFAT 删除恢复）；③ 导出到源盘自身的分区 ⇒ **-32006 拦截**（③ IOKit）；④ 签名/公证包 Gatekeeper 首开（右键-打开仅剩「未公证」场景需复测）；⑤ transport/removable 显示正确。
  - **Linux 真机**：§5 全清单（udev uaccess 两场景 / polkit 非 keep / SSH 硬拒 / euid 参数防御 / 真 U 盘删除照片全链 / 长跑节流 / 导出降权三核对）+ **ext4 真盘删除恢复**（真 USB 移动硬盘 ext4：删除 → 扫到 → 恢复；经免密 sudo 或真 root）+ 环回 e2e-loop 已有项复核。
  - 记录形式：逐项「实测结果 + 证据（命令/截图路径）+ 判定」写入 security 文档 §12 刷新版（**未通过项如实标失败并开修复任务，不得降级为"未验证"**）。
- [ ] **Step 4: 未验证边界总表刷新**：§12 表——M1 遗留 12 行中已闭合者标注「M2 已封（测试/物证指针）」；新增 M2 边界行（NTFS/ext4 真介质、journal 时效边界、x64 包、签名证书缺失时的 SmartScreen、macOS helper 缺口若 cut）。
- [ ] **Step 5: 合入**：全部任务分支 → 里程碑合并（lead 执行）；`main` push；发版流程照 M1 先例。
- [ ] **Step 6: 提交**：commit `docs: M2 出口——真机手测记录/未验证总表刷新/v0.5.0`

**验收取证点**：CI run id（逐 job）+ 两真机清单记录 + 签名状态物证（`codesign -dv`/`signtool verify` 输出）。

---

### Task 13（可裁剪，默认收尾）: carving 扩展 —— MP4 / PDF（OFFICE 归 M3）

> 设计 §7 M2 行列有此项，本任务书基线未含（R8）。**默认排在 T12 之前、可整体后移 M3**；若后移，设计 §7 同步改写并记入出口文档。

**Files:**
- Create: `crates/xd-carving/src/{mp4.rs,pdf.rs}`、`crates/xd-fixtures/assets/{tiny.mp4,tiny.pdf}`、测试入 `carve_e2e.rs`
- Modify: `crates/xd-carving/src/{signatures.rs,lib.rs}`、`proto/v1/README.md`（`quality:"carved"` 的 ext 值域说明非契约枚举，无需改——确认后不动）

- [ ] **Step 1: MP4**：`ftyp` 盒签名 + 盒走链（size@0 大端、type@4；`size==1` ⇒ 64 位 largesize）；结构验证 = 至少 `ftyp` + 一个 `moov` 或 `mdat`，且所有盒长合法（防误报）；交付 = 走到合法链尾或 run 界。
- [ ] **Step 2: PDF**：`%PDF-1.x` 头 + `%%EOF` 尾扫描（窗口内回找，`startxref` 交叉验证加分）；交付 = 头至 EOF（含）或诚实截断。
- [ ] **Step 3: 恢复率门禁**：合成镜像（FAT 与 NTFS/ext4 各一）埋入已知 MP4/PDF ⇒ 深扫 carved 计数与逐字节相等；假阳性 0（随机噪声镜像）。
- [ ] **Step 4: 门禁与提交**：commit `feat(carving): MP4/PDF 雕刻（盒走链/EOF 验证 + 恢复率门禁）`

**测试清单**：`mp4_box_chain_validates_and_rejects_truncated`、`mp4_largesize_box`、`pdf_finds_eof_and_startxref`、`pdf_without_eof_is_honest_prefix`、`carve_rate_gate_mp4_pdf`、`no_false_positive_on_noise`。

---

## 验收定义（M2 Done 的判据）

1. **引擎**：NTFS——live 枚举 + 删除恢复（名字/大小/时间戳）+ `$Bitmap` 分级 + 读/预览/导出 + 深扫，全链在 `cargo test` 与环回 e2e 内字节级成立；ext4——live + 深扫 + **journal 删除恢复**（含内核删除 e2e 实跑）成立；两引擎 oracle 交叉验证（mkntfs / mke2fs）在 Linux CI 腿实跑；只读门禁（sha256 不变 + write 面零命中）全绿。
2. **契约**：v1.3 纯增量落地（36+4 golden 双侧断言；`protocol` 不递增）；store v6 迁移链（v1→v6）逐级可迁移。
3. **平台收口**：§12 十二行逐条有处置指针——①首页提权入口（widget + 真机）、②Windows 卷号 -32006（CI 纯函数 + 真机）、③macOS IOKit 归属与 transport（CI 真查询）、④签名/公证链（脚本实现 + 有/无凭据两径）、⑤shutdown/所有权/限流（CI 实跑）、⑥中危四项（补钉有牙）、⑦Windows ACL（CI 断言）、⑧x64 物证（artifact）、⑨Runner.rc、⑩真机手测轮（记录）。
4. **打包**：三平台产物在 CI packaging（`packaging=true`）全绿：deb（既有）、Windows zip、macOS arm64+x64 zip；签名在凭据缺失时**如实跳过**（不产半签包）。
5. **门槛**：`cargo test --workspace --locked`（三 OS）+ clippy/fmt + `flutter analyze/test` + `e2e.sh`/`e2e-loop.sh` + packaging 三腿全绿；未验证边界总表刷新入文档；`README` 平台矩阵三平台状态更新。

---

## 风险与开放问题

| # | 项 | 处置 |
|---|---|---|
| V1 | **ext4 journal 体量与风险**（M2 最大不确定项）：jbd2 解析 + 覆盖层 + 内核 e2e，估算 = 全里程碑工作量 20-25% | T5 独立成任务 + cut 线（R1）：后移 M3 时 ext4 = live+深扫，出口降级需 lead 书面接受 |
| V2 | macOS SMAppService helper（设计 §5.1 明文归 M2，本计划默认不纳入） | 需 lead 裁定：**建议延后 M3**（不阻塞出口；CI 不可验、FDA 归属需真机）；若纳入按注释骨架单列任务 |
| V3 | carving 扩展（设计 §7 M2 行，基线未含） | T13 可裁剪；后移则改设计 §7 |
| V4 | Windows/macOS 真机面广（UAC/FDA/IOKit/真介质） | T12 手测轮为独立验收路径；所有未过真机项标「未验证（需真机）」直至手测 |
| V5 | CI 时长增长（oracle 工具安装 + sudo e2e + 三腿 packaging） | oracle 安装仅 Linux 腿一步 `apt-get`；packaging 仍手动门控；超时上限矩阵逐 job 观察（现有 15/30 分钟） |
| O1 | Windows 代码签名证书（EV/OV）购买 | 业务决策；代码侧只保证钩子可用（R6）。无证书则 SmartScreen 提示为已知限制 |
| O2 | macOS 签名证书与 Apple ID/团队号 | 同上；CI secrets 配置由 lead 执行 |
| O3 | `--owner-uid` 的 uid 获取（Dart 侧 `id -u` 子进程） | Task 8 Step 3 已给回退方案；若实施期否决，按回退口径并记录 |
| O4 | `docs/security/linux-privilege-model.md` 名称已名不副实（含 Win/macOS 平台模型） | 建议 M2 收口时更名为 `platform-privilege-model.md` 或加副标题——需 lead 定（本计划默认只增节不更名） |
| O5 | 版本号 0.5.0 与发版节奏 | 按 M1 先例（T12 Step 1/5） |
| O6 | 性能基线（设计 §4.6 70% 底线）在 NTFS/ext4 上的首次量化 | 每引擎 e2e 附 debug/release 吞吐记录；不达标记入出口文档并开 M3 优化任务 |

**理论与实证参考**（起草期依据，实施期须独立复核）：ext4 删除清零 extent + journal 恢复路线（extundelete / ext4magic / ext4dfr 文献与手册）；NTFS 删除恢复（MFT 记录保留 + `$Bitmap` 覆盖判定）；GitHub `macos-15-intel` runner（2027-08 退役）。

---

## 执行记录

### T1（契约 v1.3 + probe/FsKind + store v6 + 双引擎骨架）—— impl-m2-t1。提交沿革：`a34b51a`（主）→ `b9d5ed7`（.gitattributes 禁 CRLF——Windows 字节断言红点根因修复）→ `3c3be10`（qual 补测扩版：probe 改序 + 顺序钉 4 例/边界/panic/拒绝层 + 注释毛刺）。DONE → spec **PASS** → qual **APPROVED**（32 变异 24 KILL / 8 SURVIVE→4 缺口补钉落地）→ 收口增复 APPROVED（T1 关闭；workspace **483/0**；flutter 140+2 / 带 daemon 142；CI 全绿 @ `3c3be10`）

- **交付**：契约 v1.3 纯增量（`recordId` 三态 0 合法/缺省省略；fs 域 ntfs|ext4；`daemon.shutdown` 契约面；golden 36→40 只增不改——FNV-1a64 摘要表钉旧 36 字节）；store v6 迁移（模板同构 + 列探测 + v5→v6 测试）；probe 单读 2048B 识别；双引擎骨架（Error non_exhaustive 同构，三签名恒 `Err(Unsupported)` 不 panic）；4 分派点 8 臂穷尽（无 `_` 通配，删臂=编译错）；fs_read 单漏斗「缺 recordId → -32603 不猜读」。
- **★ lead 裁定（改序）**：qual 发现 FAT16（reserved=1）FAT1 区偏移 1080 恰为 `0xEF53` 时被 ext4 判据抢判（~1/65536/卷）→ 裁定 **probe 顺序 exFAT→NTFS→FAT→ext4**（真 ext4 过不了 FAT 闸，真实设备结果不变）；顺序钉 4 例（O-A/B/C 变异各死其定制断言）；引擎侧 fail-closed 仍为 T2/T4 硬要求（纵深）。
- **spec 独立核验**：36 golden sha256+FNV 双独立复算（非自证）；CI 面终条款（Windows 腿测试名直取、release 腿闭环、packaging skip=门控设计）；`.gitattributes` 根因链 + 三态检出复现。
- **记录项（落 M2 出口文档/T12）**：R-1 测试字面量 7 文件 9 处（impl 申报 6 处不准）；R-2 `protocol_v12_test.dart` 36→40 漏列申报；R-3 骨架 4 分派点/8 臂（lead 清单「5 臂」笔误）；S-1 三文件 >500 行系既有债（scan_task 1250 / store 1078 / contract_v1 721——里程碑中不拆，T12 评估）；N-1 FAT 卷带 NTFS OEM 误判面 + 新序残余 ~1e-5（T2/T4 fail-closed 兜底）；cosmetic `scan_task.rs:65` 语序 nit（不修）。
- **移交**：T2=勿重复已就位映射/快扫臂；填引擎时删 `Unsupported`；observer「后序+quality 终值」须镜像或声明偏离。T4=superblock 结构校验（fail-closed 硬要求）。T7=shutdown handler（契约已钉，Dart 分发臂同步）。共享热点=4 分派点穷尽 + golden 集合断言 3 处联动。
- **未验证**：真卷/真设备 probe（T2 oracle 首检）、真块设备 2048B 读语义、Windows 本地检出复现（CI 代）、packaging×`-text` 交互未演练。

---

© 2026 erik · https://erik.xyz · erik@erik.xyz

---

## 用户裁定（2026-10-03）

1. **Windows 代码签名证书**：确认采购（EV/OV；用户侧外部动作，长交付期——M2 期立即启动为妥）。代码侧按 R6 实现 `signtool` 钩子；证书未到位期间保持具名 skip。
2. **Apple 证书与 CI secrets**：**暂不配置** ⇒ macOS 签名/公证走条件路径（缺凭据 ⇒ `skip:` 具名行 + artifact 名不变，绝不产半签包）；T10 的签名链代码照常实现，「缺凭据」分支即为当前验收面。
3. **ext4 journal（R1 Cut 线）**：**撤销**——全量保留（T5 不移 M3、出口不降级）。
4. **版本号**：M2 = **0.5.0** 确认。
