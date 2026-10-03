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

### T2（Windows 平台层）—— impl-m1e-t2。提交沿革：`810405d`（主）→ `c2a6779`（§8 引 CI run id + `:253` 括注；qual docfix patch 原文落码）→ `aa4998f`（机械拆分 `windows/enumerate.rs` + 零盘跳过 + P2/P18/P19 钉补）。DONE → spec **PASS**（CI 面独立核验闭）→ qual **APPROVED**（20 枚移植变异 18 KILL / 2 记录）→ 收口增复 APPROVED（T2 关闭；457/0；Windows 腿 430/0；CI 两轮 5/5）

- **交付**：`windows.rs`（287 行）+ `windows/enumerate.rs`（354 行）——SetupAPI 枚举（`GUID_DEVINTERFACE_DISK` → 盘号经 `IOCTL_STORAGE_GET_DEVICE_NUMBER`）/ 只读句柄（`GENERIC_READ` only、无 `NO_BUFFERING`）/ `read_at` 钳位短读（≈Linux pread）；`io_lock: Mutex<()>` 串行化（**计划缺陷①修正**：原 Send/Sync 注释不成立——内核文件游标即共享可变状态）；`win:\\.\PhysicalDriveN` + `--device` 信任边界校验（Linux/macOS 零改动）；冒烟 2 枚 **CI runner 真跑**；纯函数单测 6→8；README/§8 已知限制。
- **spec 独立核验**：CI 面亲验（headSha 绑定、逐测试名、`runneradmin` 提权上下文旁证——预判风险消解）；计划缺陷②（daemon 臂本地不可交叉 check）由 CI 首次真编译关闭。
- **qual**：移植变异 19+1=20 枚（**18 KILL**；存活 P3 等效 / P4 两平台同表同缺记 M2）；`SP_DEVICE_INTERFACE_DETAIL_DATA_W` cbSize 与 windows-sys 0.61.2 源码逐位核对属实；只读铁律零写 API；windows-sys 单版本 + feature 最小性微 crate 实证；**拆分机械性独立复现**（25→27 函数，仅 4 处申报差异）。Nit 记录（P18 冗余断言等）。
- **★ T4 头号移交（opener/枚举接线缺口）**：`main.rs:199-200` Windows `list_only` 恒空 + `:254-255` opener 恒 `Noopener`；UAC 命令若只传 `--listen/--port-file` ⇒ 提权 daemon 列零设备、扫描走不通。必修：(a) Windows 枚举接线（提权上下文正是所需环境）；(b) `win:` id 的 `DaemonOpener` 臂（懒打开）或枚举即注册；(c) `image:` 拒绝语义保持；(d) `--device` 入 UAC 命令须显式规划（单盘注册 ≠ 替代枚举）。
- **T3 移交**：同款缺口（macOS 亦 `Noopener`+空 list_only）；`main.rs:136`「仅 Linux 支持」消息随 T3 更新；macos.rs 从第一笔守 500 行线宽。
- **未验证**：真机枚举/读、UAC 全链路与 `--device` 运行时、4Kn 512B 读、USB-SATA 桥接盘 BusType/removable、换盘热插拔、Windows port-file ACL（T1 项）、同盘校验缺口 UX（§8 M2 卷号比较）、IO 面 I1-I9 审查式裁定。CI 脆弱面：冒烟隐含 admin + 恒有 PhysicalDrive0（设计前提非 bug）。

### T3（macOS 平台层）—— impl-m1e-t3。提交沿革：`6304f92`（主）→ `fc4a8b2`（§9 引 CI run id + F1/F2）→ `a32d3b0`（收口补钉 P1-P7）。DONE → spec **PASS**（含 1 必办 docfix）→ qual **APPROVED**（36 变异 24 KILL / 11 SURVIVE / 1 HANG）→ 收口增复 APPROVED（T3 关闭；458/0；CI 三轮 5/5）

- **交付**：`macos.rs` 393 行（`/dev/diskN` 枚举[纯数字整盘、数值升序]/DKIOC 容量[Darwin `_IOC` 公式求值+单测钉头文件值]/`O_RDONLY` 打开/pread 补齐/FDA→`PermissionDenied`+hint/信任边界 `parse_disk_node`）；`source_rdev_of`+`dev_major_minor` 提为 `#[cfg(unix)]` 共用（linux 行为逐位不变）；main.rs macOS 臂；macos_smoke（枚举硬/打开弱断言）；§9+README；CI 新增 macOS `--nocapture` 取证步（P7）。
- **spec 独立核验**：14+ 条对照；ioctl 常量对源核（curl XNU `disk.h`/`ioccom.h` 逐字复算 0x40046418/0x40086419）；CI 面亲验（**xd-daemon 真 macOS 首次真编译**——T2 遗留缺口②就此闭合）。
- **qual**：36 枚（24 KILL / 11 SURVIVE[5 等效 + 6 缺口] / 1 HANG）；解码单射 30 万样本 0 碰撞；**P7 取证得真机实证**（runner 走 Err/EPERM 分支、`warn: …该盘仍列入，size=0`——容量容错设计口径 + 硬断言同获真机验证）；收口 P1-P7 落码后 **6/7 缺钉转 KILL**（T2/E2/E3/R3/R5/R6）。
- **偏离 5 项全 ACCEPT**：① 枚举容量容错（**未知 ≠ 确认零**——计划自身硬断言逼出）；② 跨 crate dev_t 解码同式（单射论证 + **P5 KAT 契约钉**封单侧改 Darwin 解码类风险）；③ 依赖（libc 0.2.189+rustix，lock +2 行零新版本）；④ §9 编号（§8 已占）；⑤ 接线缺口入档。
- **记录/移交**：**P8 容量乘法纯函数钉**（S2 残余，4 行，并入 T4 轮）；R2/R7 短读循环仅真机可验（M2）；F3 `fstat is_block_device`（M2）；`read_at` 三份逐字拷贝（P2 已给 macOS 执行覆盖；抽 `crate::read_at_fill` 留 T4）；`entries.flatten()` 吞 per-entry Err（记录）；**F4 erratum**（Task 3 Files 清单漏 lib.rs/linux.rs/Cargo.toml，impl 依正文执行正确）；`open` 侧 id canonicalize（P6）而枚举侧未规范——与 linux 同构（记录）。
- **T4 移交（与 T2 合并全集）**：双平台 opener 接线（`list_only` 恒空 + `NoopOpener`，含 `unix:` id opener 按平台分派；macOS 的 EPERM→PermissionDenied 目标**只在该臂可达**）；**osascript 提权 ≠ FDA**（root 后仍可能 EPERM——授权失败 UX 须能提示「已提权但仍缺完全磁盘访问」）；`--device` 是否入 UAC/osascript 命令须显式规划；-32006 macOS 盲区（整盘源 vs 分区目标）入 T6 总表。
- **未验证**：真机 root+FDA 全链、4Kn/Apple Fabric 命名、`rdisk` 性能与权限、Gatekeeper（T5/M2）、macOS port-file ACL（T1 项）。

### T4（三平台提权流）—— impl-m1e-t4。提交沿革：`cebe692`（daemon 接线：双平台枚举/opener + `--owner-pid` 监督/空转自退 + P8 + `read_at_fill`）→ `0ad6182`（port-file 属主交还）→ `aa9562f`（UI 三平台引导）→ `84392ae`（security §10）→ `1c59b1b`（ISSUE-1：会话目录显式 chmod 700）→ `bacf0dc`（root 测试取证：ran:/skip: 二值 + CI 步）→ `c9cfdf5`/`9abe104`（qual 补钉 P1/P2345/P7 + Windows 守卫）→ `bfbfff6`（P1 随件 production hunk 还原，byte 级对照=P1b）。DONE → spec **PASS**（含 ISSUE-1 闭合）→ qual **APPROVED**（39 变异 19 KILL / 20 SURVIVE → 12 补钉 + 8 等效）→ 增复 APPROVED（T4 关闭；cargo 469/0；flutter 137+2；CI 六轮 5/5）

- **交付**：`elevation.dart`（三平台 `ElevationPlan` 纯函数：Win32 `-EncodedCommand`[UTF-16LE] / osascript 双层引号 / pkexec 字面 IP + `spawnElevation` / `connectElevatedSession`）；scan_controller 提权流（-32001 → 0700 会话目录[**显式 chmod**] → 轮询 ≤30s/500ms → client 交换 → 重试；**半行双形态重试**；osascript≠FDA 提示格）；daemon 接线（`enumerate_startup_list` 三平台 + `win:`/`unix:` 平台分派 opener + `image:` 语义不变）；`--owner-pid` 监督 + 空转 3s 自退 + 清 port-file + exit 0 + stdout 零写；security §10。
- **spec 独立核验**：命令构造 12 组注入 0 逃逸（自写 UTF-16LE 解码器 + `CommandLineToArgvW` 语义复算）；生命周期独立真进程探针（含仓库未覆盖的「启动窗口不误退」）；ISSUE-1（`createTempSync` 跟随 umask，0002⇒0775）以**显式 chmod 700 + mode 真值断言**闭合（双 umask 复跑 + 448/509 变异互证）；8 偏离全 ACCEPT。
- **qual**：39 枚（19 KILL；**12 补钉**杀 R1/R6/R7/R9/R14/R17/R18 + D6/D13/D19/D20；8 等效）；补钉后 469/0 + 137+2；增复轮语义零差 + 全杀复跑 + 无夹带。
- **★ 计划缺陷①（关键承重，落地时发现）**：root daemon 写 port-file 属主=root ⇒ UI 读不到令牌、整链断（T1 同用户测试暴露不出）→ `adopt_owner_of_dir`（仅 root、取自目录属主、rename 前 chown，无任意 chown 原语）+ UI 0700；**CI ubuntu+macOS 两腿 `ran:` 直证真执行**（取证步首跑命中）。
- **计划缺陷②（Windows 提权入口不可达）**：**记 M2**（产品决策面：首页无设备时无触发点；判据 3 未失守）；强制移交互 T5（`README-安装.txt` 改写）与 T6（总表/矩阵行；表述「Windows 提权链已备、入口未接（M2）」）。
- **计划缺陷③**：`-ArgumentList` 数组拆断含空格路径 → 单串 + Win32 引用（KAT 钉）。
- **记录不修**：chmod 非零退出未检查（M2 一行建议）；Windows 第二轮 -32010 落 `_retryViaClientRestart` 文案误导（角落）；授权超时弃留 root daemon（窗口有界，T6 表）；30s 为软界；§10.4 未直言后果链（M2 文档补）；adopt 路径式 TOCTOU（M2 fd passing）；README「已知限制」段 stale（T5/T6）。
- **T5 移交**：README 平台矩阵/已知限制改写（Windows 提权流「未接入（M2）」；Linux/macOS「可用（真机未验证）」；删过期「枚举接线归 T4/M2」半句）；`README-安装.txt` 提权句改写；**打包布局验证 `_client.daemonPath` 非空**（否则重试静默退化旧支路）；macOS 未签名/Gatekeeper + osascript≠FDA 真机复验说明；打包冒烟不得依赖 `--listen/--port-file/--owner-pid`。
- **T6 移交**：未验证总表加行（真 UAC/osascript/pkexec、Windows port-file ACL/`OpenProcess` 语义、pid 复用/双 daemon 窗口、macOS -32006 整盘源盲区、Windows 提权入口不可达[M2]、授权超时弃留窗口、R11 属主交还 root 真路径、R2 空转下界）；判据 3 取证点=`ui/test/elevation_test.dart`（15 枚）。
- **未验证**：真 UAC/osascript/pkexec 对话框与端到端链、真 FDA(TCC)、Windows ACL/`OpenProcess`、真机 pid 复用、R11 root 真路径。

### T5（Windows/macOS 打包）—— impl-m1e-t5。提交沿革：`2fe0e03`（主：两打包脚本+两冒烟+CI 门控+`packagedDaemonPath`+README/§11）→ `710ed0f`（F1 活锁修复）→ `579b336`（README 用法 3 如实化 + smoke 退出码）→ `7695506`（spec 逐字补正：用法 3 三行 + ps1 一行，byte-exact）→ `fff5d29`（qual 钉 0001+0002）→ `950f186`（N-1 macOS 具名失败行）。DONE → spec **PASS 主干 + 1 轻微 ISSUE（已修）** → qual **ISSUES**（1 项：F1 回归钉缺失）→ 落钉有牙 → 增复 **APPROVED**（T5 关闭；cargo 470/0；flutter 139+2；CI 两轮 7/7 与 5/5+2skip）

- **交付**：`package-windows.ps1`（v0.3.0 注入 / `--locked` / 组装 / zip）/`package-macos.sh`（arch 分派 / ditto / 按架构两 zip）/`e2e-package-smoke.{ps1,sh}`（`--image`+ping、双平台具名 FAIL 奇偶）/`notarize.sh` 空壳（M2 顺序）；ci.yml 两手动 job（`packaging` input 门控、artifact `xiaodun-{windows,macos}-zip`）；`packagedDaemonPath()`（显式→ENV→**同目录回退**，双击承重件）+2 单测；README 三平台矩阵/已知限制；security §11。
- **spec 独立核验**：两 artifact **下载拆包**（win 15 项 / mac 57 项含 6 symlink 保真、universal 主程序）；CI 锚点逐行（`PACKAGE SMOKE OK` 双平台）；门控双跑零扰动（7/7 与 skip）；偏离 A-D 全 ACCEPT。
- **qual**：F1 修复正确但**回归保护缺失**（`pending` 提交套件 0 次填充——两序容忍机制是套件内死代码；变异双向 12/0 绿）；钉 `0001`（反序确定性钉）+`0002`（大写宿主上界钉）+N-1 落地后**双向有牙复现**（复活→挂死 rc=124、摘消费→0.304s panic、去 `toLowerCase`→红）。
- **★ F1（T1 遗留活锁，拆弹）**：`Lines::next()` pending 回放+失配 push 回 ⇒ 通知抢跑时纯用户态自旋（98% CPU、socket 读超时全程不生效）；修复=去掉回放分支；修复前复现 rc=124 vs 修复后 0.15s；回归钉 `0001` 已落地（13 枚）。
- **记录（T6 表）**：`Runner.rc` 版本资源仍 `xiaodun_ui.exe`（装饰性）；**F2** macOS 沙箱 entitlement + ad-hoc 签名后 seal 失效（首开或报「已损坏」）；`-macos-x64.zip` 物证缺（runner=arm64）；artifact 内 README CRLF（runner autocrlf，仓库 LF）。
- **T6 移交**：**终轮 `packaging=true` 必办**（唯一途径修已发布 artifact 内旧 README + ps1 新行真机首解析）；未验证总表增行全集（运行时回退/真机首启/SmartScreen/无 VC++ 库/macOS 挂载+沙箱/F2/签名公证 M2/TCC-FDA M2/x64 zip/Runner.rc）；判据 4 取证点=两 artifact。
- **未验证**：pwsh 脚本本机零执行（CI 已实证）；`packagedDaemonPath` 运行时回退（仅纯函数单测）；真机 GUI 首启/签名链全系。

---

© 2026 erik · https://erik.xyz · erik@erik.xyz