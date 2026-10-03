# 小盾（Xiaodun）数据恢复工具 · 设计文档

- 日期：2026-10-02
- 状态：已定稿（待实施计划）
- 技术栈：Flutter（UI）+ Rust（核心与业务逻辑）

## 1. 项目概述

小盾是一个全平台数据恢复工具：UI 用 Flutter 一份代码覆盖全端，
底层引擎与业务逻辑用 Rust 实现。

**已确认的决策：**

| 决策点 | 结论 |
|---|---|
| 平台范围 | 真·全端：Windows / macOS / Linux / Android / iOS（Web 排除，无法访问本地磁盘） |
| 目标用户 | To C 起步（免费扫描预览、付费恢复导出），架构预留专业版能力 |
| 团队规模 | 5+ 人，按 4 条工作流并行 |
| 架构选型 | 方案 B：core 纯库 + 桌面特权 daemon + 移动端内嵌（frb） |

## 2. 支持范围与能力边界（诚实约束）

### 2.1 支持的介质

| 介质 | 典型文件系统 | 阶段 |
|---|---|---|
| U盘 | FAT32 / exFAT / NTFS | M1（FAT/exFAT）→ M2（NTFS） |
| SD / TF / 相机卡 / 行车记录仪 / 无人机卡 | FAT32 / exFAT | M1 |
| 移动硬盘（USB HDD/SSD） | NTFS / exFAT / APFS / ext4 | M2-M3 |
| 读卡器接入的任何卡 | 同介质本身 | 同介质 |
| 手机（Android 内部存储） | ext4 / f2fs（需 root） | M4 |
| iPhone | 非块设备 → 走备份解析 | M4 |

前提：设备在系统中表现为块设备即可，与内置盘同一条通路——Windows
`\\.\PhysicalDriveN`、macOS `/dev/rdiskX`、Linux `/dev/sdX`。

U 盘与相机卡是 To C 最高频场景，因此 M1 的首个出口标准就是「真 U 盘删照片
→ 扫到 → 预览 → 恢复」。

### 2.2 软件层面无解的情况（文案必须诚实）

1. 介质物理损坏（控制器故障、掉固件、芯片级问题）——需开盘/芯片级服务。
2. 硬件加密盘（带加密芯片的移动硬盘/SSD）——不通过厂商工具无法读取。
3. TRIM 已执行——多数 U 盘/存储卡无 TRIM（利于恢复）；部分 SSD 与支持
   TRIM 的卡删除后主控已擦块，物理不可逆。
4. 覆盖写入过多——产品页需引导用户「发现丢失后先停止写入该介质」。

### 2.3 平台能力边界

- **桌面三平台**：完整磁盘恢复能力（块设备级扫描）。
- **Android**：root 模式下扫块设备，复用 ext4/f2fs 引擎；无 root 时仅做
  备份/迁移辅助，不做虚假宣传。
- **iOS**：与桌面**不是同一技术路线**。未越狱设备走 iTunes/Finder 本地备份
  解析（增量备份中残留的删除记录、应用缓存、缩略图）+ AFC 设备直连的有限
  缓存扫描；越狱设备可跑块设备扫描。产品文案必须严格区分能恢复什么。
- **Web**：不支持。

## 3. 总体架构

### 3.1 架构形态

```
Flutter UI（普通权限）
  ├─ 桌面：spawn xiaodun-daemon（特权进程）← JSON-RPC over stdio / 本地 socket
  └─ 移动：进程内嵌 xd-ffi（frb 绑定，iOS 不允许 spawn 子进程）
        │
        ▼
xd-core（纯 Rust 库：零 IPC、零平台耦合）
```

理由：

1. iOS 不允许 spawn 子进程 → 移动端只能库内嵌。
2. 桌面提权是刚需（Windows 管理员 / macOS root+全盘访问 / Linux root），
   daemon 让 UI 保持普通权限。
3. 扫盘代码要解析任意损坏数据，崩溃隔离是实打实的需求；daemon 崩溃可重启
   并从检查点续跑。
4. 接口冻结后，UI / 桌面 / 引擎 / 移动 4 条线可真正并行。
5. 同一 core 可直接复用为专业版 CLI。

### 3.2 仓库结构（monorepo）

```
xiaodun/
├── crates/
│   ├── xd-core/          # 编排、任务状态机、对外契约类型、预览/导出模块（纯库，零 IPC）
│   ├── xd-device/        # 块设备抽象：只读流式读取、扇区缓存、坏道跳过、镜像文件后端
│   ├── xd-fs-ntfs/       # NTFS：MFT 解析、删除记录恢复（Windows）
│   ├── xd-fs-apfs/       # APFS（macOS，含快照枚举）
│   ├── xd-fs-ext4/       # ext4（Linux / Android）
│   ├── xd-fs-fat/        # FAT/exFAT（U盘、相机卡——To C 高频场景，最先做）
│   ├── xd-carving/       # 深度扫描：文件签名雕刻（照片/视频/文档优先）
│   ├── xd-mobile/        # 移动专有：iOS 备份解析、Android root 块设备、AFC
│   ├── xd-daemon/        # bin：桌面特权进程 + IPC 服务
│   ├── xd-ffi/           # cdylib：frb 绑定（移动端内嵌）
│   └── xd-cli/           # 预留：专业版/调试/CI 用
├── proto/                # 唯一 IPC 契约：JSON-RPC 方法 + 消息 schema
├── ui/                   # Flutter app（全平台同一套）
├── fixtures/             # 合成测试镜像 + 生成脚本
└── ci/
```

- 引擎**按文件系统拆 crate**（非按功能拆）：每个 FS crate 有独立 owner、
  独立测试镜像、独立 fuzz target，CI 可按 crate 并行。
- `proto/` 是唯一接口定义：daemon 映射为 JSON-RPC，ffi 映射为 frb 调用；
  **契约第一周冻结 v0**，之后演进走版本协商。
- `xd-fs-fat` 最先做：规范最简单，最快跑通端到端证明架构；U盘/相机卡本身
  是 To C 最高频场景。

### 3.3 工作流切分

| 线 | 人员 | 范围 |
|---|---|---|
| 引擎线 | 2-3 人 | 每人包 1-2 个 FS crate + carving |
| 平台线 | 1-2 人 | device、daemon、提权、签名公证、CI 矩阵 |
| UI 线 | 1-2 人 | Flutter、CoreClient、页面流、预览渲染 |
| 移动线 | 1 人+ | xd-ffi、Android 集成、iOS 备份解析 |

引擎线与 UI 线只通过 proto 契约对接，可完全并行。

## 4. 引擎设计

### 4.1 一次扫描的完整链路

```
UI 选设备
 → daemon 枚举块设备（Win: \\.\PhysicalDriveN | mac: /dev/diskX | Linux: /dev/sdX）
 → core 探测文件系统（读 boot sector / superblock 签名）
 → 快速扫描：解析 FS 元数据，产出两类结果
     · 存活文件（目录结构完整）
     · 已删除文件（名字/大小/时间戳在，数据区可能已被覆盖）
 → 深度扫描（用户触发）：扫未分配空间做签名雕刻
 → 结果流式推送 → UI 增量渲染
 → 勾选 → 预览（按需读源盘片段）→ 恢复到目标盘
```

### 4.2 快速扫描：各文件系统的恢复原理

| FS | 删除后元数据状态 | 恢复要点 |
|---|---|---|
| NTFS | MFT 记录标记"未使用"但仍存在 | 扫 `$MFT` 找未使用记录；查 `$Bitmap` 判断数据簇是否被覆盖 → 产出可解释的恢复成功率 |
| ext4 | inode 的 dtime 置位，extent 树通常还在 | 扫 inode 表按 dtime 过滤 |
| FAT/exFAT | 目录项首字节置 `0xE5` | 目录项直接可用；簇链重建是难点（碎片化文件） |
| APFS | 对象号 + 快照 | 先枚举 APFS 快照（直接读历史版本，成功率最高），再做普通删除恢复 |

「恢复质量分级」（完整 / 可能损坏 / 仅雕刻）由引擎直接产出——这是 To C
产品的核心付费点（免费预览成功率的底气）。

### 4.3 深度扫描（雕刻）

- `xd-device` 顺序读未分配空间，坏道跳过不中断。
- 多线程并行块扫描 + 签名匹配 + 边界验证（头部解码确认，压误报）。
- 分片重组：JPEG 靠 EOI、MP4 靠 moov box、视频靠 GOP 边界。
- 产出无文件名结果 + 估计完整度。

### 4.4 五条硬约束

1. **只读铁律**：源设备句柄一律只读打开；导出路径在 core 层强制校验
   「不落回同一物理设备」。写错一次毁掉用户数据 = 产品死刑，实现与测试
   均为第一优先级。
2. **崩溃隔离**：daemon 崩 → UI 断连感知 → 自动重启 + 检查点续跑。
   `catch_unwind` 包在**每块解析**外而非每任务外，单块损坏只丢一块。
3. **任务状态机 + 检查点**：`Idle → Scanning → Paused → Completed/Failed`；
   检查点 = 已扫偏移 + 结果 flush 点，落本地 SQLite。可暂停、可关 UI、
   可隔天继续。
4. **流式结果 + 持久化**：几十万条结果增量写 SQLite，UI 分页拉取，
   内存只留当前页。app 关闭后结果仍在，可直接预览/导出。
5. **进度必须真实**：进度 = 实扫字节 / 目标字节，ETA 用滑动窗口。

### 4.5 恢复导出

目标盘校验（异设备 + 剩余空间）→ 逐文件恢复 + 校验（大小/文件头/可选哈希）
→ 恢复报告（成功/降级/失败清单）。「先镜像后恢复」为专业版预留，但
`xd-device` 从第一天按「可同时读源 + 写镜像流」设计接口。

**实现注记（M1d，契约 v1.2）**：

- **执行体**：导出由 daemon `spawn 自身 --export-worker` 的**独立子进程**完成（父只转发
  子 stdout 的 JSON 事件行），root 且 `PKEXEC_UID` 存在时降权到调用者；模型与威胁面见
  `docs/security/linux-privilege-model.md` §6。
- **校验**：目标目录三重（存在 / 异设备 / 余量）——`-32006`（点在源设备上）/`-32007`/`-32010`；
  同盘判定两道（`st_dev` 与源 `st_rdev` 相等 + sysfs 盘级祖先），sysfs 不可用时 fail-open 留痕。
  读取侧 `fs.read` 分片、上限 64MiB（`-32009`，先于设备解析拒绝）；条目不存在 `-32008`。
- **写盘**：4MiB 分块流式（无整文件物化）；落盘名过净化（`..`/分隔符/控制字符）后重名去重，
  雕刻件 `carved_{idx:06}.{ext}`；条目标记删除且簇被复用 ⇒ 短交付计 degraded 并保留半成品，
  失败件清残骸。
- **事件**：`export.progress`（done/total/writtenBytes）+ `export.finished`（succeeded/
  degraded/failed/canceled/targetDir/items——**成功件不进 items**）；响应可**后于** finished
  通知到达（T3 底盘竞序），UI 侧在途寄存回放保证终报不丢。
- **报告**：UI 三计数 + 逐件清单（落盘名一律取 `items[].name`）；`itemsTruncated` 提示。

### 4.6 性能底线

- 扫描吞吐 ≥ 磁盘顺序读性能的 70%。
- 百万级结果下列表流畅翻页（虚拟化渲染 + SQLite 分页）。

## 5. 平台层

### 5.1 提权模型

| 平台 | 设备访问 | 提权方式 |
|---|---|---|
| Windows | `\\.\PhysicalDriveN`、卷 | daemon 经 UAC 提权启动（`ShellExecute runas`），UI 保持普通权限 |
| macOS | `/dev/rdiskX`（raw 设备，比 buffered 快） | root + Full Disk Access；用 `SMAppService` 注册 LaunchDaemon helper，首次运行引导授权 |
| Linux | `/dev/sdX`、`/dev/nvmeXnY` | polkit 授权 daemon 或 udev 规则；打包 deb/rpm/AppImage |

IPC：stdio 为主（spawn 时建立），本地 socket 作为重连/CLI 复用通道。

> **实现注记（M1e-tail，2026-10）**：M1 落地与上文目标形态的差异（均已如实标注，见 security 文档 §7-§12 与 README）：
> - **提权会话通道 = TCP 回环 + 令牌**（`--listen 127.0.0.1:0 --port-file <0600 令牌文件>`；非 pipe/句柄继承——Windows UAC 后父进程 stdio 不可继承）→ §7/§10。
> - **Windows**：SetupAPI 枚举 + `CreateFileW` 只读；UAC 经 `Start-Process -Verb RunAs`（`-EncodedCommand`）；**应用内入口 M2 T6 已接入**（首页空列表「以管理员身份重启引擎」；非提权枚举为空 ⇒ 首页无设备问题解除）→ §8/§13。
> - **macOS**：`/dev/diskN`（非 rdisk）+ `osascript do shell script … with administrator privileges`（**≠ FDA**）；`SMAppService` LaunchDaemon helper 归 M2 → §9。
> - **Linux**：uaccess + polkit 按原案；daemon 生命周期 = `--owner-pid` 监督 + 空转自退（无 shutdown RPC）→ §10。
> - **打包**：Linux deb（CI 全自动）；Windows 便携 zip / macOS staged zip（**未签名**，公证归 M2；CI 手动 job `packaging=true`）→ §11。

### 5.2 移动端边界

- **Android**：无 root 仅 MediaStore/SAF 可见范围；root 模式经
  `/dev/block/by-name/*` 完整复用 ext4/f2fs 引擎。
- **iOS**：未越狱 = 备份解析 + AFC 有限缓存扫描；越狱 = 块设备扫描。
  独立 crate（`xd-mobile`），不依赖块设备抽象。

## 6. UI 结构（Flutter）

```
lib/
├── core_client/        # CoreClient 抽象 + IpcTransport(桌面) / FfiTransport(移动)
├── features/
│   ├── device_select/  # 盘/卷/备份文件选择
│   ├── scan/           # 扫描控制 + 进度 + 结果浏览器（虚拟化）
│   ├── preview/        # 照片/视频/文本/文件信息
│   ├── recover/        # 目标选择 + 恢复进度 + 报告
│   └── settings/
└── l10n/               # 中英双语起步
```

- 向导式页面流：**选设备 → 扫描 → 勾选预览 → 恢复**（行业标准心智，不自创）。
- 结果三种视图：类型 / 目录结构 / 恢复质量；照片网格是大头。
- 铁律：Dart 层只做 UI 状态机和 transport，任何扫描/恢复逻辑不进 Dart。

## 7. 路线图

| 阶段 | 时间 | 内容 | 出口标准 |
|---|---|---|---|
| **M0 地基** | 1-3 周 | workspace + proto 契约 v0 冻结；`xd-device` 先实现镜像文件后端（引擎开发与测试全在镜像上做）；daemon 最小 IPC；CI 矩阵 | UI 能列出设备（含镜像），端到端握手通 |
| **M1 首个端到端** | 4-8 周 | FAT/exFAT 快速扫描 + 恢复导出；carving v1（JPEG/PNG）；扫描/预览/恢复三页；Windows 提权打包 | 真 U 盘删照片 → 扫到 → 预览 → 恢复成功，可演示 |
| **M2 桌面三平台** | 9-16 周 | NTFS（含 $Bitmap 覆盖率评估）+ ext4 引擎；macOS helper + 公证；Linux 打包；carving 扩到 MP4/PDF/OFFICE | 三平台安装包，三引擎可用 |
| **M3 产品化** | 17-24 周 | 免费预览/付费导出、许可激活、更新机制；APFS（含快照）；性能打磨；杀软白名单申报 | 可上架销售的桌面 1.0 |
| **M4 移动线** | 与 M2/M3 并行 | Android root 引擎 + 无 root 辅助；iOS 备份解析引擎 | Android/iOS 上架 |
| **M5 专业版** | 不排期 | 磁盘镜像、扇区编辑、批量工单、CLI | 方向预留 |

关键排序逻辑：**M0 的镜像文件后端是整个计划的地基**——引擎测试、UI 集成
测试、CI 全部脱开真实硬件，这是项目能并行的前提。

## 8. 测试策略

1. **合成镜像 fixtures**：脚本生成（mkfs + 已知文件集 + 随机删除/碎片化/
   截断）进 CI；每个引擎配一套基准镜像。
2. **恢复率回归门禁**：CI 断言各镜像恢复率不低于基线——数据恢复工具的
   "性能指标"就是恢复率，任何 PR 不许弄低。
3. **Fuzz 全解析器**：cargo-fuzz 喂损坏镜像，不许 panic（与 catch_unwind
   双保险）。
4. **只读验证**：CI 断言扫描后镜像哈希不变；「导出不落回源设备」有专门单测。
5. **真机矩阵**：真 U 盘/SD 卡 + 三平台 VM，定期半自动执行。

## 9. 风险清单

| # | 风险 | 对策 |
|---|---|---|
| 1 | 恢复率达不到用户预期（差评主因） | 质量分级 + 文案管理预期；先打磨相机卡照片高频场景 |
| 2 | macOS 公证/权限流程复杂卡壳 | M1 就打通最小流程，不拖到 M3 |
| 3 | iOS 线"看起来能恢复其实不行" | 限定为备份解析，文案严格区分 |
| 4 | 杀软误报（读物理磁盘行为敏感） | 代码签名 + 微软/主流 AV 白名单申报，M3 前排期 |
| 5 | 扫描性能/内存失控 | 流式 + SQLite 分页从第一天做对，M1 建立性能基线 |
| 6 | 合规（工具双刃性、隐私） | ToS 明确合法用途；不做远程/隐藏数据功能 |

---

© 2026 erik · https://erik.xyz · erik@erik.xyz
