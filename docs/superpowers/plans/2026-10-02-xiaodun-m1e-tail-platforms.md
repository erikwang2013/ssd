# 小盾 M1e 尾段：Windows/macOS 平台层 + 提权流 + 打包实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 补齐 M1 的平台收口——Windows（`\\.\PhysicalDriveN` 枚举/只读打开 + UAC 提权路径 + zip 打包）与 macOS（`/dev/diskN` 枚举/只读 + osascript 提权路径 + 打包 staged，公证归 M2）；新增 **TCP 回环传输**（提权会话的唯一可移植通道，跨平台可在 CI 全验）。

**核心裁定：提权会话走 TCP 回环 + 令牌，而非 pipe/句柄继承。** 理由：Windows UAC 提权后父进程 stdio 句柄不可继承、Dart 无原生 named pipe；TCP 回环 + `--port-file`（0600，含端口与 32 位十六进制令牌）+ 首行 `{"auth":"<token>"}` 握手在三平台同一实现、**在 Linux CI 即可全链路真测**。威胁模型：令牌文件 0600 仅本用户可读；同用户攻击者本就有 uaccess 直读设备权限（无提权增益），异用户读不到令牌（见 security 文档更新）。

**纪律：** 一切真机/真提权路径在文档与代码注释中标注**「未验证（需真机）」**；CI 能验的（编译、单元、枚举冒烟、stdio 会话、**Linux 上的 TCP 会话全链路**、打包产物冒烟）必须验。

**前置：** M1b/M1c/M1d 已合入（契约 v1.2、daemon、UI）。

---

### Task 1: TCP 回环传输（daemon `--listen/--port-file` + Dart SocketTransport）

**Files:**
- Create: `crates/xd-daemon/src/{transport.rs, portfile.rs}`（+main.rs 分派）
- Modify: `crates/xd-daemon/Cargo.toml`（+`rand`？——**不引入**：令牌 32 位十六进制由 `/dev/urandom`（unix）/`BCryptGenRandom`（windows）读 16 字节生成；封装 `fn random_token()`（unix 一实现 + windows cfg 一实现，windows 侧标未验证）
- Modify: `ui/lib/core_client/ipc_transport.dart`（+`SocketCoreClient`）
- Create: `crates/xd-daemon/tests/tcp_session.rs`、`ui/test/socket_transport_test.dart`
- Modify: `docs/security/linux-privilege-model.md`（+提权会话传输节）

**协议（三平台一致，写在 transport.rs 头注）：** daemon `--listen 127.0.0.1:0 --port-file F`：绑定回环随机端口 → 生成令牌 → **原子写 F**（内容一行：`<port> <token>`；unix 0600，通过先写临时文件 0600 再 `rename`）→ stderr 打印就绪。每条连接：**首行必须是 `{"auth":"<token>"}`**，否则立即断开；通过后该连接按 stdio 同款逐行 JSON-RPC 处理（通知也推给该连接）。首连之后的新连接遵循同一规则（M1 允许并发连接，每连接独立会话——**多连接下通知只推发起订阅的连接**；M1 简化：通知广播给所有已认证连接，文档明示）。

- [ ] **Step 1: transport.rs（daemon 侧，关键全代码）**

```rust
// © 2026 erik · https://erik.xyz · erik@erik.xyz
//! 传输层：stdio（缺省）与 TCP 回环（提权会话）。TCP 协议见计划头注；令牌比较用常数时间。
//! 注意：提权（root）daemon 的监听面仅回环 + 令牌——异用户读不到 0600 令牌文件即无法接入。

pub struct TcpOptions {
    pub addr: std::net::SocketAddr,
    pub port_file: std::path::PathBuf,
}

pub fn serve_tcp(opts: TcpOptions, ctx: Arc<CoreCtx>, out: Arc<Mutex<dyn Write + Send>>) -> ! {
    let token = random_token();
    write_port_file(&opts.port_file, opts.addr.port(), &token); // 0600 + rename 原子
    eprintln!("xd-daemon: listening on 127.0.0.1:{}", opts.addr.port());
    let listener = TcpListener::bind(opts.addr).expect("bind loopback");
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let (token, ctx, out) = (token.clone(), ctx.clone(), out.clone());
        std::thread::spawn(move || handle_conn(stream, &token, &ctx, out));
    }
    unreachable!()
}

fn handle_conn(mut stream: TcpStream, token: &str, ctx: &CoreCtx, out: Arc<Mutex<dyn Write + Send>>) {
    let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let ok = serde_json::from_str::<serde_json::Value>(&first)
        .ok()
        .and_then(|v| v["auth"].as_str().map(|t| ct_eq(t.as_bytes(), token.as_bytes())))
        .unwrap_or(false);
    if !ok {
        let _ = writeln!(stream, r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32001,"message":"Authentication failed"}}}}"#);
        return; // 失败即断开（不读后续）
    }
    // 逐行请求循环：与 stdio 分支同款（抽 `serve_lines(reader, ctx, out, writer_factory)` 共用）
    serve_lines(reader, ctx, out, stream);
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    // 常数时间比较（长度差直接 false；M1 令牌恒定长度）
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
```
（`serve_lines`：把现有 main.rs 的 stdin 循环抽成 `pub(crate) fn serve_lines(reader: impl BufRead, ctx: &CoreCtx, out: Arc<Mutex<...>>, writer: impl Write + Send + 'static)`——stdio 分支调它，TCP 分支也调它。通知写入 `out`（带连接路由 M1 简化=广播；实现：`out` 为 `Arc<Mutex<Vec<Box<dyn Write + Send>>>>`？**裁定：M1 的 `out` 改为 `Notifier`（`Arc<Mutex<Vec<Box<dyn Write + Send>>>>` 的包装），scan_task 的 NotifyFn 语义不变、内部遍历写。stdio 会话注册自己，TCP 每连接注册自己、断开时移除。** 写失败即从集合移除。）

- [ ] **Step 2: portfile.rs**：`write_port_file(path, port, token)`：unix：`OpenOptions::mode(0o600).create_new(true)` 写临时同名 `.tmp-<pid>` → `rename`；windows：直接写（ACL 归 M2 记 TODO，**标未验证**）。`random_token()`：16 字节 → 32 hex。
- [ ] **Step 3: main.rs 分派**：`--listen <addr>` 与 `--port-file <path>`（须成对出现，否则 exit 2）；给定则走 TCP 模式（**不读 stdin**；stdout 仍可为诊断——契约规则保持 stdout 仅协议，诊断全 stderr ✓）；否则现状 stdio。
- [ ] **Step 4: tcp_session.rs 集成测试（全代码要点，CI Linux 真跑）**
1. `tcp_session_full_flow`：daemon `--listen 127.0.0.1:0 --port-file tmp`（先轮询文件出现）→ 读 port/token → TcpStream 发首行 auth → ping/device.list → 断言响应；**错误令牌** → 收到 -32001 后连接断开。
2. `port_file_is_0600`（unix 断言权限位）。
3. `tcp_scan_notifications_reach_client`：auth 后 `scan.start`（image）→ 连接上能读到 `scan.finished` 通知（同一连接）。
- [ ] **Step 5: Dart `SocketCoreClient`**（ipc_transport.dart 内）：`start({required addr, token, portFile})`——读 port-file → `Socket.connect` → 发 `{"auth":...}` → 同款 `_onLine`/`_call`/通知流（**抽公共基类 `LineCoreClient`（process 或 socket 皆可注入 `Stream<String> lines` 与 `void Function(String) write`）**，IpcCoreClient 与 SocketCoreClient 各自适配，`_call` 逻辑不再复制）。`ui/test/socket_transport_test.dart`：用 `ServerSocket.bind(loopback, 0)` 起一个**Dart 假 daemon**（回 auth 校验 + ping）→ 断言握手、调用、通知路由、错误令牌抛异常。
- [ ] **Step 6: security 文档**：提权会话传输节（威胁模型/令牌生命周期/已知限制=广播通知、无速率限制——M2 收紧）。
- [ ] **Step 7: 门禁与提交**（cargo + flutter 双侧；commit `feat(daemon,ui): TCP 回环提权会话传输（令牌握手/port-file/通知广播）+ Dart SocketCoreClient`）

---

### Task 2: Windows 平台层（枚举 + 只读打开，CI Windows runner 可编译可冒烟）

**Files:**
- Create: `crates/xd-device/src/windows.rs`（lib.rs `#[cfg(windows)] pub mod windows;`）
- Modify: `crates/xd-device/Cargo.toml`（`[target.'cfg(windows)'.dependencies] windows-sys = { version = "*定版*", features = ["Win32_Foundation", "Win32_Storage_FileSystem", "Win32_System_IO", "Win32_Devices_..."]}`）
- Modify: `crates/xd-daemon/src/main.rs`（`--device` 在 windows 分支：`win:\\.\PhysicalDriveN` id）
- Create: `crates/xd-device/tests/windows_smoke.rs`（cfg(windows)）

**范围（未验证边界写注释）：** 枚举 `SetupDiGetClassDevs(GUID_DEVINTERFACE_DISK)` → 每盘：设备路径 `\\.\PhysicalDriveN`、`IOCTL_DISK_GET_LENGTH_INFO` 取大小、`IOCTL_STORAGE_QUERY_PROPERTY(BusType)` 映射 transport（usb/sata/nvme/other）、removable=BusType Usb/1394/Sd。打开：`CreateFileW(path, GENERIC_READ, FILE_SHARE_READ|WRITE, …, OPEN_EXISTING)` + `FILE_FLAG_NO_BUFFERING`？——**不加**（对齐要求复杂，M1 用带缓冲读）；`SetFilePointerEx`+`ReadFile` 实现 `read_at`（**无任何写权限请求**，与只读铁律一致：desired access 仅 GENERIC_READ）。id 格式：`win:\\.\PhysicalDriveN`（契约 DeviceInfo.id 既有约定 ✓）。

- [ ] **Step 1: windows.rs（结构+关键代码；全部函数带"未验证（需真机）"标注，CI 验编译与冒烟）**：`WindowsBlockDevice { handle: RawHandle, info: DeviceInfo }`（`Send+Sync`：HANDLE 裸指针 → `unsafe impl Send/Sync` 且注明"句柄仅读、无跨线程共享可变状态"）；`enumerate() -> Result<Vec<WindowsDisk>, DeviceError>`；rdev 映射：Windows 无 st_rdev——`BlockDevice::source_rdev` 默认 None ✓（M1d 契约：源设备为 None 时导出不做同盘校验 → **Windows 上同盘校验缺口** → 在 README/security 记为已知限制「Windows 目标盘同源校验 M2 补（卷句柄卷号比较）」）。
- [ ] **Step 2: 冒烟测试（CI Windows runner 真跑）**
```rust
#[test]
fn enumerates_at_least_one_disk_on_ci_runner() {
    let disks = crate::windows::enumerate().expect("enumerate");
    assert!(!disks.is_empty(), "CI runner 至少有一块系统盘");
    assert!(disks.iter().all(|d| d.info.size_bytes > 0));
}
#[test]
fn opens_readonly_and_reads_boot_area() {
    // 打开 PhysicalDrive0，读 512 字节（MBR/GPT 头）不 panic；读越尾 → 短读/Err 均可但不崩
}
```
- [ ] **Step 3: 门禁与提交**（本地只能 `cargo check --target x86_64-pc-windows-msvc` 若已装 target；否则以 CI Windows job 为准——提交后看 CI；commit `feat(device): Windows 平台层（SetupAPI 枚举/只读物理盘句柄，未验证=需真机）`）

---

### Task 3: macOS 平台层（枚举 + 只读打开 + Full Disk Access 提示）

**Files:**
- Create: `crates/xd-device/src/macos.rs`（`#[cfg(target_os = "macos")]`）
- Modify: `crates/xd-daemon/src/main.rs`（macOS `--device` 分支）
- Create: `crates/xd-device/tests/macos_smoke.rs`

**范围：** 枚举 `/dev/disk[0-9]+`（`std::fs::read_dir` + 正则式解析；过滤 `diskXsY` 分区——M1 只要整盘 `/dev/diskN`）；大小：`rustix::fs::ioctl`？——macOS 的 `DKIOCGETBLOCKCOUNT/DKIOCGETBLOCKSIZE` 经 `libc::ioctl`（**依赖：`libc` target macos**）；transport：`IORegistry`？——M1 简化：`diskutil info -plist /dev/diskN` 太慢且引入子进程 → **M1 标 None（transport 未知）**，注释记 M2 用 IOKit。打开：`/dev/diskN`（buffered；rdisk 更快但权限重——M2 评估），`O_RDONLY`（rustix::fs::open）。Full Disk Access 缺失时 open 返回 EPERM → 映射 `OpenError::PermissionDenied`（UX 文案「需在系统设置授权完全磁盘访问」）。
- [ ] **Step 1: macos.rs 实现（未验证标注同 Windows）**；`source_rdev`：unix 系可直接 stat → **macOS 复用 Linux 的 st_rdev 逻辑**（把 xd-device 里 Linux 的 `source_rdev` 实现提为 `#[cfg(unix)]` 共用函数）。
- [ ] **Step 2: 冒烟（CI macOS runner）**：枚举 ≥1（runner 有根盘）+ 打开 `/dev/disk0` 读 512B（CI runner 无需 FDA 读根盘？**不确定——测试改为「枚举 ≥1」硬断言 + 「打开成功则读 512B，返回 PermissionDenied 亦接受并 eprintln 标注」的弱断言**，避免 runner 权限差异红 CI）。
- [ ] **Step 3: 门禁与提交**（commit `feat(device): macOS 平台层（/dev/diskN 枚举/只读打开/FDA 提示，未验证=需真机）`）

---

### Task 4: 提权流（Windows UAC / macOS osascript / UI 引导三分支）

**Files:**
- Create: `ui/lib/core_client/elevation.dart`（命令构造纯函数 + 启动器）
- Modify: `ui/lib/features/scan/scan_controller.dart`（EACCES 引导按平台分派：Linux pkexec[已有] / Windows UAC / macOS osascript）
- Create: `ui/test/elevation_test.dart`

- [ ] **Step 1: elevation.dart（全代码）**
```dart
/// 提权命令构造（纯函数，可单测）；真提权路径未验证（需真机 UAC/polkit/授权对话框）。
class ElevationPlan { const ElevationPlan(this.executable, this.arguments); final String executable; final List<String> arguments; }

/// Windows：UAC 弹窗启动（Start-Process -Verb RunAs）。daemon 以 --listen/--port-file 会话模式
/// 启动（stdio 句柄无法跨提权继承——这就是 TCP 回环传输存在的理由）。
ElevationPlan windowsPlan({required String daemonPath, required String portFile}) => ElevationPlan(
  'powershell.exe',
  ['-NoProfile', '-Command',
   'Start-Process -Verb RunAs -FilePath "\$args[0]" -ArgumentList @("--listen","127.0.0.1:0","--port-file","\$args[1]") -WindowStyle Hidden', ...],
);
```
（`-Command` 传参转义是易错点：用 `-EncodedCommand`（base64 UTF-16LE）避开引号地狱——**决定：用 EncodedCommand**，构造函数单测断言 base64 解码回的脚本文本包含 daemonPath/portFile 且无注入面（路径含空格/引号用例）。）
```dart
/// macOS：管理员授权启动（osascript do shell script … with administrator privileges）。
ElevationPlan macosPlan({required String daemonPath, required String portFile}) => ...
/// Linux：pkexec（policy 文案见 docs/security）。
ElevationPlan linuxPlan({required String daemonPath, required String portFile}) => ElevationPlan('pkexec', [daemonPath, '--listen', '127.0.0.1:0', '--port-file', portFile]);
```
- [ ] **Step 2: scan_controller 分派**：捕获 -32001 → 平台分支 → 生成 port-file 临时路径 → 启动提权进程 → **轮询 port-file（≤30s，500ms 间隔；用户可能在认证框上耗时）** → `SocketCoreClient.start(...)` → 替换 client → 重试 scanStart；超时/用户取消 → 文案「未获得授权」。widget 测试：Fake 平台注入（`debugDefaultTargetPlatformOverride` 或依赖注入 `ElevationPlan Function()`）+ 假 port-file 写入 → 断言走通；**真对话框路径标未验证**。
- [ ] **Step 3: 门禁与提交**（commit `feat(ui): 三平台提权引导（UAC/osascript/pkexec → TCP 会话）`）

---

### Task 5: 打包（Windows zip / macOS staged；产物冒烟进 CI）

**Files:**
- Create: `scripts/package-windows.ps1`、`scripts/package-macos.sh`
- Modify: `.github/workflows/ci.yml`（+`package-windows`/`package-macos` 两个 **workflow_dispatch 手动** job）
- Create: `scripts/e2e-package-smoke.ps1`、`scripts/e2e-package-smoke.sh`

- [ ] **Step 1: Windows 打包**：`flutter build windows --release` + `cargo build --release -p xd-daemon` → 组装 `xiaodun-vX.Y.Z-windows-x64.zip`（`xiaodun.exe` + `xd-daemon.exe` + Flutter 产物 + `README-安装.txt`[含"提权由应用内 UAC 引导触发"]）；冒烟 `e2e-package-smoke.ps1`：解压 → 以 `--image` 起 xd-daemon.exe → 管道 ping→断言 pong（PowerShell 进程管道 ✓ CI 可跑）。
- [ ] **Step 2: macOS 打包 staged**：`flutter build macos --release` + daemon → `xiaodun-vX.Y.Z-macos-universal.zip`（arm64+x64 或分别两个 zip——**裁定：分别两个**，CI macos runner 单架构最稳）；**签名/公证/notarization 归 M2**（`scripts/notarize.sh` 空壳 + TODO 注释，文档记"未签名包首次打开需右键-打开"）；冒烟同 Windows（bash 版）。
- [ ] **Step 3: CI job（手动触发）**：windows runner：build → package → smoke → upload-artifact；macos runner 同。**开发机无法本地验证 → 以 dispatch 一轮 CI 实证**。
- [ ] **Step 4: 门禁与提交**（commit `feat(packaging): Windows/macOS 打包脚本与产物冒烟（CI 手动 job）`）

---

### Task 6: 出口验收

- [ ] **Step 1: 全矩阵 CI**：rust（Linux/Windows/macOS）+ flutter + deb + e2e + **手动 dispatch 的 packaging 两个 job**，逐 job 验证（`gh run view <id> --json jobs`）。
- [ ] **Step 2: 未验证边界总表**（docs/security + README）：真机 UAC/授权对话框、Windows/macOS 真设备删除-恢复、Windows 同盘校验缺口、macOS 未签名包 Gatekeeper——全部如实标注。
- [ ] **Step 3: 文档与合入**：README 平台矩阵更新（三平台"可用/未验证"分列）、设计文档 §5.1 实现注记、计划执行记录；合入 main + push。

---

## 验收定义（M1e 尾段 Done 的判据）

1. TCP 回环传输：Linux CI 全链路真测（握手/错误令牌/通知/0600）；Dart SocketCoreClient 双向测试；stdio 契约零回归。
2. Windows/macOS 平台层：CI 两 runner 编译 + 枚举/读冒烟通过；真机路径明确标注未验证。
3. 提权三分支：命令构造纯函数单测（含注入用例）；真提权路径标未验证。
4. 打包：Windows/macOS 产物在 CI 手动 job 构建 + 解压冒烟（daemon ping）通过。
5. 全矩阵门禁绿；未验证边界总表入文档。

---

## 执行记录

### T1（TCP 回环传输）—— impl-m1e-t1。提交沿革：`3d505cc`（主）→ `e309254`（spec 观察①：`SharedSink::write_all` 整行原子 + 并发布/响应交错钉测）→ `07710a2`（qual 修复轮：F-Dart-1 会话死亡语义 + 护栏钉 + security §7 事实化；原 `16720d3`，lead push 前 amend 消息）→ `1661088`（qual 第 5 件：`wait_port_file` 32-hex 收下加固）。DONE → spec **PASS** → qual **ISSUES**（1 行为缺陷 + 安全护栏缺钉）→ 修复有牙 + 复核 → **APPROVED**（T1 关闭；workspace 457/0；flutter 120+2 / 带 daemon 122；CI 四轮 5/5）

- **交付**：daemon `--listen/--port-file` TCP 回环会话（令牌握手[首行 auth/常数时间/-32001 即断不读后续]、`serve_lines` stdio/TCP 共用、`Notifier` 广播[写失败剔除/drop 注销/`SharedSink` 整行原子]、`portfile.rs`[unix 0600+`.tmp-<pid>`+rename 原子；windows 直写+BCryptGenRandom 标注]）；Dart `LineCoreClient` 抽取 + `SocketCoreClient`（port-file 单次读；-32001 粘性会话失败快速失败）；security §7（威胁模型/令牌生命周期/已知限制）。
- **spec 独立核验**：12 条对照 + 手工实证（错令牌+合法请求同包只回 -32001 即 EOF；CLI 成对/非回环/坏地址 exit 2；port-file 0600+32hex 无残留；stdout 全程 0 字节）；6 偏离 + 1 计划缺陷（骨架先写 port-file 用 `opts.addr.port()`，`:0` 时会写 0——按正文修正，**erratum 记录**）全接受。
- **qual 变异 23 枚（R1-R16/D1-D7）**：KILL 归因干净（R15 write_all 摘除 3/3 稳定红）；SURVIVE = 2 时序等价/1 死代码 + **7 枚真缺钉补测**（前缀/空串令牌绕过、非回环放行、成对约束、空行、符号链接写穿、写失败剔除、SinkHandle 注销）+ **1 行为缺陷 F-Dart-1**（FIN 不触发 `Socket.done` → 在飞 + 后续每次 10s 悬挂；T4 重试流受影响）→ 粘性 `_sessionError` 修复（缺口钉 <1s 绿）。
- **落地文件 sha 差异说明**：landed 两新文件含仓库既有零宽版权串（全树约定，非缺陷）；qual INDEX 已补 landed/CI 对照。
- **记录项（归 M2）**：无行长上限（TCP 把可达面扩到任何本机进程）；accept 循环 EMFILE 紧旋（建议退避）；握手无超时；`lock().unwrap()` 中毒模式；`.tmp` 残留重试与 bind 失败 exit(2) 两分支无测试（近不可达）；`SharedSink::write_fmt/write` 保留为纵深（注释标注无调用路径）；一条注释归属措辞微瑕（记录不改）。
- **T4 移交（头号设计输入）**：① 提权 daemon 生命周期缺口——协议无 shutdown RPC、client 无进程句柄 → root daemon 无主滞留 + 多轮提权累积 + port-file/令牌清理归属须定方案（shutdown RPC / owner-pid 监督 / 全断自退）；② port-file 半行**两形态**（FormatException / 截断 token→-32001）轮询须双形态重试；③ 白名单用字面 IP（勿 localhost）；④ TCP 形态新建 client 不走 restartPrivileged；⑤ taskId 过滤保留。
- **T2/T3 移交**：port-file ACL 收紧（真机 `icacls` 核对）；Windows 直写可升级 tmp+rename（`MOVEFILE_REPLACE_EXISTING` 可原子）；中危四项随平台层加固；§7 未验证清单随演进同步。
- **未验证**：Windows/macOS port-file ACL 可读性、真 UAC/osascript/TCC 提权链、提权 daemon 停机行为、中危四项行为面。

---

© 2026 erik · https://erik.xyz · erik@erik.xyz