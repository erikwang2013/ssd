# 小盾 M4「移动线」实施计划（立项级）

> **状态：立项级草稿——执行前必须细化轮。** 细化轮产出物：每任务拆到
> `docs/superpowers/plans/2026-10-02-xiaodun-m1e-tail-platforms.md` 粒度（Files 清单 / 步骤 /
> 门禁与提交拆分 / 验收判据 / 「未验证（需真机）」标注纪律），并把本稿 §2 的预研结论**回填为裁定**
> （§1 候选逐条落定），此后才可进入执行。
>
> 本稿含 **§6 七项开放项**，其中 O1/O2 不裁定则 M4 范围无法冻结。
>
> 日期：2026-10-03 · 仓库基线：M1 收口（v0.3.0，契约 v1.2）· 上游：M2/M3 计划（并行起草中）

**Goal:** 小盾在 Android 与 iOS 上架，并把移动端真实能力边界如实交付——Android root 块设备
恢复（ext4/f2fs）与非 root 可见范围（MediaStore/SAF）、iOS 备份解析（+ 越狱块扫描为可选档）；
**不虚假宣传**（设计文档 §2.3 / §5.2 / 风险 #3）。

**出口标准（设计文档 §7）：Android/iOS 上架。**

---

## 0. 现状与前置（事实引用）

| 面 | 现状 | 影响 |
|---|---|---|
| `crates/xd-ffi/` | 占位：仅 `core_version()`（`Cargo.toml` 无依赖、无 cdylib 声明） | T1 从零接线 |
| `ui/` | 平台目录仅 `linux/ macos/ windows/`；`CoreClient` 抽象已留注释位「移动端 = FfiCoreClient（M4）」 | 移动端无任何构建面 |
| 契约 | `proto/v1` 冻结（v1.2 增量，36 golden）；`DeviceInfo.kind/fs` 值域**不含**移动源 | 移动端源类型需契约增量（见 §3 契约影响面） |
| M1e 遗产 | TCP 回环 + 令牌 + `--port-file` **属主交还**（T4 关键修复：root daemon 写 0600 文件 chown 给目录属主）+ `--owner-pid` 监督 + 空转自退 | **Android root 助手直接复用**（§1 A1） |
| 引擎 | FAT/exFAT/carving 已交付；**ext4 归 M2**、APFS 归 M3（未动工）；**f2fs 全新** | 排期依赖 §5 R5 |
| CI | `workflow_dispatch` 手动；无 Android/iOS leg | T9 需新增构建 leg |
| 测试纪律（既有） | 合成镜像 fixtures、恢复率门禁（不许弄低）、fuzz 不许 panic、只读哈希实证、真机标注「未验证（需真机）」 | M4 全任务沿用 |

---

## 1. 关键裁定候选（细化轮前必须落定）

### A1 移动端进程模型：**双轨**——非 root 进程内嵌 FFI；root/越狱走复用 M1e 的提权助手进程

- **非 root（两端）**：进程内嵌 `xd-ffi`（frb）。iOS 不允许 spawn 子进程（设计 §3.1 理由 1）；
  Android 非 root 下 spawn 与 app 同 UID，无意义。
- **root/越狱**：**不是进程内**——复用 M1e 提权会话模型：Android 以 `su -c` 启动
  xd-daemon（编译为 `aarch64-linux-android`，以 `jniLibs` 内可执行 `.so` 形态打包，libsu 同款手法），
  走 `--listen 127.0.0.1:0 --port-file <app 私有目录>`；**port-file 属主交还机制原样复用**
  （M1e「计划缺陷①」的修复——root 写的 0600 文件 chown 回 app UID，否则 UI 读不到令牌、整链断）。
  iOS 越狱同模型（helper 经越狱 root 通道启动）；**若 P5 预研证伪，则 root 档降级为进程内 + 能力缩减**。
- 收益：a) 崩溃隔离（扫描解析任意损坏数据，daemon 崩可重启续跑——设计 §3.1 理由 3 在移动端同样成立）；
  b) 扫描路径与桌面同一条**已被 470 测试与 12 轮 CI 验过的通路**；c) UI 侧 `SocketCoreClient` 已存在。
- 代价：打包复杂度（可执行 .so + su 授权流）；待 P5 实证。

### A2 FFI 契约映射：**控制面走信封，数据面留类型化例外**

- 控制面：FRB 暴露单一 `rpc_line(String) -> String` + 通知流 `Stream<String>`；Dart 侧
  `FfiCoreClient extends LineCoreClient`（M1e T1 已抽出公共基类，注入 write/lines 即可）。
  收益：**契约零复制**——proto 唯一事实源在移动端原样成立，Rust/Dart 双侧 golden 测试无须分叉。
- 数据面例外候选：`fs.read` 预览分片经信封 = base64 膨胀 33% + 多一次拷贝；FRB 原生支持
  `Vec<u8>`。候选：移动端 `fs.read` 走类型化 FRB 函数（保持信封语义等价，golden 仍以信封形态钉）。
  **裁定时机：P4 原型实测后**。
- 错误映射：JSON-RPC 错误码原样保留；`-32001`（EACCES）在移动端语义变体：Android → 引导 su 授权
  （T4 流），iOS 非越狱 → 如实文案「本机不可读，非越狱能力边界见说明」。

### A3 iOS 未越狱产品定位（依赖开放项 O1）

iOS app 沙箱**读不到**宿主机的 iTunes/Finder 备份目录；AFC 只能在宿主侧（USB 直连）做。
候选：
1. **桌面为主**：备份解析引擎（`xd-mobile`）落桌面 app（Mac/Windows 读本地备份 + AFC）；
   iOS app 极小（「导入备份文件夹解析」入口 + 能力说明）。
2. 端内备份导入为主：引导用户经 Files/文件共享把备份拷入 app 容器再解析（流程笨重但端内闭环）。
3. iOS app 不上架，iOS 支持仅桌面侧——**与 M4 出口标准冲突，须 lead 裁定**。
建议候选 1+2 组合；**细化轮前必须由产品裁定**。

### A4 root/越狱增强的合规边界（依赖开放项 O2）

- **Google Play**：root 工具不禁止，但须如实申报；非 root 路径若走 SAF/MediaStore 可绕开
  `MANAGE_EXTERNAL_STORAGE`（All files access）申报；Data safety 表必须如实。
- **App Store**：检测越狱才启用的能力有拒审风险（审核机必为非越狱）；备份解析定位须自洽。
- 候选：上架包仅含非越狱能力；越狱/root 增强档的 iOS 侧走 TestFlight/自签分发，或干脆不做。
  **细化轮前必须裁定。**

---

## 2. 技术预研任务（P1-P7；产出物可直接回填为裁定与执行计划）

| # | 预研 | 关键问题 | 产出物 |
|---|---|---|---|
| P1 | **f2fs 引擎可行性** | superblock/checkpoint/NAT/SIT/SSA/node/segment 布局；删除语义（node block 释放与 orphan）；恢复路径候选：raw node 扫描 vs 目录树重建；Android 变体（`mkfs.f2fs` 版本差异、inline data、compression flag） | 格式笔记 + 最小原型（读 sb/checkpoint/NAT 列已删条目）+ fixture 方案（`f2fs-tools` + loop+mount，CI 照 e2e-loop 免密 sudo 模式自动跳过）+ **恢复率预期与文案边界结论** |
| P2 | **iOS 备份格式解析** | iOS 10+ `Manifest.db`（fileID = sha1(domain-relativePath)，blob 按前 2 位十六进制分桶）与 iOS 9- `Manifest.mbdb`；**增量残留**（设备删除后 blob 仍滞留备份目录 = 恢复核心假设，必须实证）；加密备份（`Manifest.plist` 标志 → PBKDF2-SHA256 + AES-256-CBC per-file key）流程与密码交互 | 格式笔记 + 脱敏夹具方案（真备份不可进 CI）+ 可恢复内容清单（含「能恢复什么」的硬边界） |
| P3 | **FBE/FDE 内容加密边界实测** | Android 7+ FBE（fscrypt per-file key，删除即 key 销毁）+ Android 10+ metadata encryption 下，raw 块读拿到的是明文还是密文；FDE（dm-crypt）旧机对比 | **能力边界表**（元数据/文件名/内容各档），直接决定 root 功能取舍与产品文案——**此项结论可能推翻「root 块设备恢复」的现有宣传口径** |
| P4 | **FFI 边界原型** | frb 版本钉（当前主线 v2 系，细化轮钉死小版本）；`rpc_line` + 通知流；线程模型（扫描后台线程 + 取消）；双平台构建链（`cargo-ndk` / iOS 交叉编译 + Xcode）；崩溃隔离缺失的补偿口径 | 可运行原型（双平台 hello + 镜像设备 ping/device.list/scan 一条龙）+ 构建脚本 + `fs.read` 数据面裁定输入 |
| P5 | **root 助手模型 PoC** | 真机（Magisk/KernelSU）下 `su -c` 启动 `.so` 形态 daemon；port-file 属主交还复用；SELinux 与厂商 ROM（MIUI/EMUI 等）兼容性 | PoC 记录 + 兼容性备忘（含失败时的降级方案 A1-fallback） |
| P6 | **Android 非 root 能力面实测** | MediaStore 回收站（`is_trashed`）第三方 app 可见性；SAF 树的可见范围；`READ_MEDIA_*` 粒度权限行为 | 非 root 功能清单（可承诺的硬边界，供 T3 与商店文案） |
| P7 | **上架合规预研** | Play 政策条文取证（root/敏感行为/All files access 申报口径）；App Store 审核指南对越狱特性的判例；数据安全表/隐私标签材料清单；移动 IAP 与桌面许可（M3）的关系 | 合规备忘 + 申报材料清单 + 对 O2/O4 的决策建议 |

---

## 3. 任务粗分解（T1-T10；细化轮拆执行步骤；依赖标注）

| # | 任务 | 依赖 | 备注 |
|---|---|---|---|
| T1 | `xd-ffi` 接线 + `FfiCoreClient`：frb 生成、`rpc_line`/通知流、错误与取消、契约测试复用 golden | P4 | 与非 root 功能并行；Dart 侧复用 `LineCoreClient` |
| T2 | 移动端 UI 适配：手势/布局（结果网格/预览）、运行时权限流（Android 13+ 媒体权限；iOS 照片/文件）、移动端目标目录选择（SAF/UIDocumentPicker） | T1 | 触发契约 `targetDir` 语义扩展（见下） |
| T3 | Android 非 root 能力：MediaStore 源 + SAF 源进 `device.list`/`scan.start`；能力边界文案落 UI | P6, T1, T2 | 硬边界以 P6 实测为准 |
| T4 | Android root 通道：helper 打包（jniLibs）、su 授权流、ext4 接入 | P3, P5, M2-ext4, T1 | 只读哈希实证 + 会话生命周期复用 M1e |
| T5 | `xd-fs-f2fs` 新引擎：解析器 + 删除恢复 + fixture + 恢复率门禁 + fuzz + 只读门禁 | P1, T4 | 按引擎线纪律（独立 crate/owner/镜像/fuzz） |
| T6 | `xd-mobile`（iOS 备份解析）：备份目录作为扫描源 + 残留扫描 + 加密备份密码流 + 导出 | P2, T1 | 引擎与服务宿主无关（桌面/iOS 共用） |
| T7 | iOS 端接入：备份导入流（端内）；越狱 helper（若 O2 裁定做） | O1, O2, T6 | 范围由开放项裁定 |
| T8 | 桌面侧 AFC（宿主 USB 直连缓存扫描） | P2 | **可移出 M4 出口**（O3）；与 iOS 备份解析同 crate |
| T9 | 打包与上架：AAB（Play App Signing）+ iOS archive/TestFlight + 商店材料 + 申报 | T3, T4, T6, T7 | 出口判据的主要取证点 |
| T10 | 出口验收：真机矩阵 + 未验证边界总表（沿用 M1e §12 体例）+ README / 设计文档 §5.2 实现注记 | 全部 | 见 §4 |

**契约影响面（T1/T2/T3/T6 汇聚，细化轮统一落）**：
- `DeviceInfo.kind` 新增移动源类型（MediaStore 范围 / SAF 树 / iOS 备份目录等）；
- `fs` 值域扩展（`ext4`/`f2fs`/`backup`…，`scan.start` 响应现有 `"fat"|"exfat"` 硬编码）；
- `export.start.targetDir` 在移动端 = SAF URI/书签而非绝对路径（Android 无法以绝对路径写外部存储）；
- 遵循 v1 既有「字段级治理」规则（只增不改、可选缺省）；是否递增 `protocol` 号由细化轮按「是否破坏性」裁定。

---

## 4. 出口标准（M4 Done 判据；对应「移动端上架」）

1. **Android 上架**：AAB 过 Play 审核（正式轨或封闭测试轨起步），含非 root 能力闭环；
   root 能力如实标注（商店描述 + app 内 + README 三处一致，零过度承诺）。
2. **iOS 上架**：App Store 审核通过——非越狱功能自洽可演示（审核机无越狱）；
   越狱增强是否随包由 O2 裁定，若随包须有拒审预案（Appeal 文案/降级开关）。
3. **真机实证矩阵（不是「未验证」清单）**：≥2 台 Android root 真机（ext4 / f2fs 各一，
   标注 FBE/FDE 形态）+ ≥1 台非 root 真机 + 真 iPhone 备份（加密与非加密各一）；
   越狱机若纳入范围则同验。P3 能力边界表在真机上复现。
4. **诚实边界文档**：能力边界表进 README + app 内说明 + 商店描述（P3/P6/P2 的结论直接引用）。
5. **CI**：移动构建 leg 绿（Android 交叉编译可在 ubuntu runner；iOS 编译需 macOS runner）；
   引擎线既有门禁（恢复率/fuzz/只读哈希）对 f2fs 与 xd-mobile 同样成立。
6. **只读铁律移动端实证**：Android root 扫描前后分区哈希不变；iOS 备份解析后备份目录哈希不变。

---

## 5. 风险与依赖

| # | 风险 | 对策 |
|---|---|---|
| R1 | **FBE 使 root 恢复内容面大幅缩水**（P3 若证实：删除数据的 key 随 inode 销毁 ⇒ 密文不可恢复） | 能力边界如实分级：元数据/文件名一档、内容一档；必要时把卖点从「root 恢复内容」调整为「能力边界表 + 非 root 回收站」。**此项是 M4 最大产品风险** |
| R2 | f2fs 删除恢复成功率低（node 释放语义比 ext4 更激进） | P1 先出结论；不行则降级为「元数据 + carving 辅助」，文案同步 |
| R3 | App Store 拒审（越狱特性 / 定位不自洽） | O2 裁定；候选上架包纯非越狱 |
| R4 | Play 申报（All files access / root 行为）被拒 | P7 预研；非 root 路径优先 SAF/MediaStore 绕开申报 |
| R5 | **上游排期依赖**：ext4（M2）与 APFS（M3）未动工；M4 与 M2/M3 并行 | 细化轮与 plan-m2/plan-m3 对齐接口与 owner；T5/T6 可先行的部分先做；接口冻结前 T4 用桩 |
| R6 | 真机矩阵获取成本（root/FBE/越狱/真 iPhone） | 出口判据 3 的机器清单在细化轮就绪；越狱档若不可得则如实出范围 |
| R7 | frb 版本/维护面（移动端唯一硬依赖） | P4 钉版本 + 锁定升级窗口 |
| R8 | 隐私与法律（读用户 iPhone 备份 = 高度敏感数据） | 本地处理、不上传（隐私标签/Data safety 如实）；加密备份密码本机使用不落盘 |

---

## 6. 开放项（需产品/合规决策；O1/O2 不裁定则范围冻结不了）

- **O1** iOS 未越狱产品定位：桌面为主 / 端内备份导入为主 / iOS app 不上架（三选一，A3）。
- **O2** root/越狱增强是否进上架包（Play 与 App Store 分别裁定，A4）。
- **O3** AFC（宿主 USB 直连）是否留在 M4 出口内，或移 M5/桌面线（T8）。
- **O4** 移动端付费形态：IAP 与桌面许可（M3 免费预览/付费导出）的关系；App Store 抽成约束下的定价。
- **O5** 加密 iPhone 备份的密码支持范围与免责文案（忘记密码无解，须前置说明）。
- **O6** 「需已 root 设备」的免责与商店文案（root 行为风险自担的表述尺度）。
- **O7** M4 是否要求「一人在移动线」的实际人力（设计 §3.3 移动线 1 人+）——pre-research 并发度决定排期。

---

## 7. 细化轮要求（执行前）

1. §1 四条候选逐条落定为裁定（附理由与替代方案），P1-P7 结论回填。
2. 每任务拆为 m1e-tail 粒度的可执行计划（Files / 步骤 / 门禁命令 / 提交拆分 / 验收判据）；
   每任务含「未验证（需真机）」清单纪律。
3. 契约增量（§3 契约影响面）在细化轮一次冻结，golden 双侧测试同步规划。
4. 与 plan-m2（ext4）、plan-m3（APFS/付费）对齐接口、owner 与排期；冲突上抛 lead。
5. CI 移动 leg 方案定型（Android 交叉编译 runner / iOS macOS runner 的构建与缓存成本）。
6. 出口判据 3 的真机清单与获取方式落定。

---

## 执行记录

（细化轮后按 m1e-tail 体例逐任务记录：提交沿革 / spec 核验 / qual 变异 / 未验证清单。）

---

© 2026 erik · https://erik.xyz · erik@erik.xyz

---

## 用户裁定（2026-10-03）

- **O1 = 候选 1「桌面为主」**：备份解析引擎（`xd-mobile`）落桌面 app（Mac/Windows 读本地备份 + AFC）；iOS app 仅「导入备份文件夹解析」入口 + 能力说明。
- **O2 = 上架包仅含非越狱能力**：Android root 如实申报（Data safety）；iOS 越狱增强**不进商店包、M4 出口不含**（必要时细化轮再议 TestFlight/自签档）。
- **预研首波已回填**：P1 f2fs **条件可行**（470 行 stdlib 原型对 2 镜像 × 4 文件 md5 全 OK；DFRWS 2025「i_addr 清零」与内核源码矛盾 → T5 loop-mount 夹具定案；估 ~3500-5000 行 Rust）；P3 **FBE 口径分层降级确认**——内容级=FDE(≤9)/便携 SD；元数据级=FBE+root+AFU(7-10)；无=离线 raw(11+)/重置后；**Android 10+ 不承诺内容级与文件名恢复**；T4 加真机最小验证 slot（建加密文件→删→raw 确认密文）；能力边界表（报告 §4 草稿）并入 M4 出口文档。
