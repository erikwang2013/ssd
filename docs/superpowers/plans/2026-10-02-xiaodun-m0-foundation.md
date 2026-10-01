# 小盾 M0 地基阶段实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 搭起小盾的 Rust workspace 与 proto v0 契约，跑通「Flutter UI 列出设备（含镜像文件）→ daemon → core」的端到端握手。

**Architecture:** Rust workspace 内 4 个 crate（xd-core 编排与 RPC、xd-device 块设备抽象、xd-daemon stdio 特权进程、xd-ffi 移动端占位）。JSON-RPC 2.0 over stdio（每行一条 JSON）。契约以 `proto/v0/examples/*.json` golden 文件为唯一事实源，Rust 与 Dart 两侧测试都对同一组 golden 断言。

**Tech Stack:** Rust 1.97（edition 2024, serde/serde_json）、Flutter stable（M0 桌面平台，零 pub 依赖）、GitHub Actions。

**前提与边界（M0 明确不做）：**
- 不做：真实物理设备枚举（M1+）、扇区缓存/坏道处理（M2 物理盘时加）、任务状态机与检查点（M1 引擎时加）、frb 绑定（M4 移动端时加）、xd-fs-*/xd-carving 等 crate（各自里程碑再加）。
- 仓库现状：只有 docs/README 与一个提交；`origin` = github.com/erikwang2013/ssd.git。本计划只提交自己创建的文件，不动 ruflo 脚手架。
- 执行策略（项目规范）：同一时刻只有一个写者。任务按依赖顺序执行；Task 8-9（UI 线）可与 Task 3-7（Rust 线）并行（文件范围零重叠），但由**串行派发**保证单写者。

**依赖关系：** 1 → 2 → {3, 4} → 5 → 6 → 7，{8 → 9}，10 最后。

---

## File Structure（M0 结束时）

```
Cargo.toml                          # workspace 根
crates/xd-core/
  Cargo.toml
  src/lib.rs                        # pub mod api; pub mod handlers;
  src/api.rs                        # JSON-RPC 信封类型 + 错误码
  src/handlers.rs                   # handle_request / CoreCtx
  tests/contract.rs                 # golden 契约测试
crates/xd-device/
  Cargo.toml
  src/lib.rs                        # DeviceInfo / DeviceKind / DeviceError / BlockDevice
  src/image.rs                      # ImageFileDevice（只读镜像后端）+ 单元测试
crates/xd-daemon/
  Cargo.toml
  src/main.rs                       # stdio JSON-RPC 循环 + --image
  tests/ipc.rs                      # 集成测试（spawn 真二进制）
crates/xd-ffi/
  Cargo.toml
  src/lib.rs                        # M4 前占位
proto/v0/
  README.md                         # 契约文档（信封/方法/错误码/版本=0）
  examples/ping.request.json
  examples/ping.response.json
  examples/device_list.request.json
  examples/device_list.response.json
  examples/error_method_not_found.response.json
fixtures/gen_image.sh               # 确定性测试镜像生成
scripts/e2e.sh                      # 端到端冒烟脚本
ui/                                 # Flutter app（flutter create 生成）
  lib/main.dart
  lib/core_client/core_client.dart  # CoreClient 抽象 + PingResult
  lib/core_client/protocol.dart     # DeviceInfo / encodeRequest / RpcException / decode
  lib/core_client/ipc_transport.dart# IpcCoreClient（spawn daemon + stdio JSON-RPC）
  lib/home_page.dart                # 设备列表页
  test/protocol_test.dart           # golden 契约测试（Dart 侧）
  test/home_page_test.dart          # widget 测试（FakeCoreClient）
.github/workflows/ci.yml            # rust 三平台矩阵 + flutter job
```

---

### Task 1: Rust workspace 骨架

**Files:**
- Create: `Cargo.toml`、`crates/xd-core/{Cargo.toml,src/lib.rs}`、`crates/xd-device/{Cargo.toml,src/lib.rs}`、`crates/xd-daemon/{Cargo.toml,src/main.rs}`、`crates/xd-ffi/{Cargo.toml,src/lib.rs}`
- Modify: `.gitignore`（追加 Rust/Flutter 忽略项）

- [ ] **Step 1: 创建 workspace 根 `Cargo.toml`**

```toml
[workspace]
resolver = "3"
members = [
    "crates/xd-core",
    "crates/xd-device",
    "crates/xd-daemon",
    "crates/xd-ffi",
]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tempfile = "3"
```

- [ ] **Step 2: 创建四个 crate 的 `Cargo.toml`**

`crates/xd-device/Cargo.toml`：
```toml
[package]
name = "xd-device"
version.workspace = true
edition.workspace = true

[dependencies]
serde = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

`crates/xd-core/Cargo.toml`：
```toml
[package]
name = "xd-core"
version.workspace = true
edition.workspace = true

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
xd-device = { path = "../xd-device" }
```

`crates/xd-daemon/Cargo.toml`：
```toml
[package]
name = "xd-daemon"
version.workspace = true
edition.workspace = true

[dependencies]
serde_json = { workspace = true }
xd-core = { path = "../xd-core" }
xd-device = { path = "../xd-device" }
```

`crates/xd-ffi/Cargo.toml`：
```toml
[package]
name = "xd-ffi"
version.workspace = true
edition.workspace = true
```

- [ ] **Step 3: 创建占位源码，使 workspace 可编译**

`crates/xd-device/src/lib.rs`：
```rust
//! 块设备抽象：M0 仅实现只读镜像文件后端。
```

`crates/xd-core/src/lib.rs`：
```rust
//! 小盾核心：编排、RPC 契约类型与处理器。
```

`crates/xd-daemon/src/main.rs`：
```rust
fn main() {}
```

`crates/xd-ffi/src/lib.rs`：
```rust
//! 移动端 FFI 占位（M4 接入 flutter_rust_bridge）。

pub fn core_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_workspace() {
        assert_eq!(super::core_version(), "0.1.0");
    }
}
```

- [ ] **Step 4: 追加 `.gitignore`**

先 `cat .gitignore` 看现有内容，确保包含（缺哪条补哪条）：
```
target/
ui/build/
ui/.dart_tool/
ui/.flutter-plugins*
```

- [ ] **Step 5: 验证编译与测试**

Run: `cargo test --workspace`
Expected: 全部编译通过，1 passed（xd-ffi 的版本测试）。

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates .gitignore
git commit -m "build: Rust workspace 骨架（xd-core/xd-device/xd-daemon/xd-ffi）"
```

---

### Task 2: proto v0 契约与 golden 样例

**Files:**
- Create: `proto/v0/README.md`、`proto/v0/examples/*.json`（5 个）

- [ ] **Step 1: 写契约文档 `proto/v0/README.md`**

```markdown
# 小盾 IPC 契约 v0

- 传输：stdio，每行一条 JSON（LF 结尾），UTF-8。**stdout 仅输出 JSON-RPC 响应行；日志与诊断一律走 stderr。**
- 信封：JSON-RPC 2.0。`id` 为请求方生成的整数，必须原样回显。
- 版本：`protocol = 0`。破坏性变更递增；daemon 与 UI 不匹配时由 `ping` 比对 `protocol` 检出。
  `version` 随 workspace 版本更新；仅因发版改动 golden 中的 `version` 不属于契约变更。
- **golden 规则**：`examples/*.json` 是契约唯一事实源。Rust（`crates/xd-core/tests/contract.rs`）
  与 Dart（`ui/test/protocol_test.dart`）两侧测试都对同一组文件断言：解析（decode）须逐字段一致，
  序列化（encode）输出也须与 golden 逐字段一致（`id` 除外，由调用方生成）。改契约必须同步改 golden 与两侧测试。

## 方法

| method | params | result |
|---|---|---|
| `ping` | null | `{"pong": true, "version": "<workspace 版本>", "protocol": 0}` |
| `device.list` | null | `{"devices": [DeviceInfo]}` |

`params` 可省略或显式 `null`；M0 两个方法均无参数。

## DeviceInfo

```json
{"id": "image:test.img", "name": "test.img", "kind": "image",
 "sizeBytes": 4096, "removable": false, "fsGuess": null}
```

- `id`：`image:<路径>`（M0）；M1 起为 `unix:/dev/sdX` / `win:\\.\PhysicalDriveN` / `volume:...`。
- `kind`：`physical` | `volume` | `image`。
- `fsGuess`：探测到的文件系统（M0 恒为 null）。

## 错误

| code | 含义 |
|---|---|
| -32700 | 无法解析的 JSON，或反序列化后不构成合法 Request（M0 简化：两种情况一律 -32700、`id` 回显 null；-32600 保留给未来） |
| -32601 | 方法不存在（`message` 为 `Method not found: <method>`，文案属契约一部分，有测试逐字断言） |
| -32602 | 参数不合法（M0 两个方法均无参，暂不可达，保留） |

## golden 文件

examples/ 下：`ping.request.json`、`ping.response.json`、`device_list.request.json`、
`device_list.response.json`、`error_method_not_found.response.json`。
```

- [ ] **Step 2: 写 5 个 golden 文件（每行一条 JSON，无多余空白）**

`proto/v0/examples/ping.request.json`：
```json
{"jsonrpc":"2.0","id":1,"method":"ping","params":null}
```

`proto/v0/examples/ping.response.json`：
```json
{"jsonrpc":"2.0","id":1,"result":{"pong":true,"version":"0.1.0","protocol":0}}
```

`proto/v0/examples/device_list.request.json`：
```json
{"jsonrpc":"2.0","id":2,"method":"device.list","params":null}
```

`proto/v0/examples/device_list.response.json`：
```json
{"jsonrpc":"2.0","id":2,"result":{"devices":[{"id":"image:test.img","name":"test.img","kind":"image","sizeBytes":4096,"removable":false,"fsGuess":null}]}}
```

`proto/v0/examples/error_method_not_found.response.json`：
```json
{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"Method not found: scan.start"}}
```

- [ ] **Step 3: Commit**

```bash
git add proto/v0
git commit -m "feat(proto): 冻结 IPC 契约 v0 与 golden 样例"
```

---

### Task 3: xd-device —— 设备模型与只读镜像后端

**Files:**
- Modify: `crates/xd-device/src/lib.rs`
- Create: `crates/xd-device/src/image.rs`

- [ ] **Step 1: 写失败的测试（`crates/xd-device/src/image.rs` 末尾测试模块）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_image(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn open_rejects_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ImageFileDevice::open(dir.path()).is_err());
    }

    #[test]
    fn read_at_returns_correct_bytes() {
        let pattern: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let f = temp_image(&pattern);
        let dev = ImageFileDevice::open(f.path()).unwrap();
        assert_eq!(dev.size_bytes(), 4096);
        assert_eq!(dev.info().kind, crate::DeviceKind::Image);
        assert_eq!(dev.info().id, format!("image:{}", f.path().display()));
        let mut buf = [0u8; 100];
        let n = dev.read_at(1000, &mut buf).unwrap();
        assert_eq!(n, 100);
        assert_eq!(&buf[..], &pattern[1000..1100]);
    }

    #[test]
    fn read_at_eof_returns_short_then_zero() {
        let f = temp_image(&[7u8; 64]);
        let dev = ImageFileDevice::open(f.path()).unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(dev.read_at(60, &mut buf).unwrap(), 4);
        assert_eq!(dev.read_at(64, &mut buf).unwrap(), 0);
        assert_eq!(dev.read_at(100, &mut buf).unwrap(), 0);
    }

    #[test]
    fn reads_do_not_modify_file() {
        let pattern: Vec<u8> = (0..256u32).map(|i| i as u8).collect();
        let f = temp_image(&pattern);
        let dev = ImageFileDevice::open(f.path()).unwrap();
        let mut buf = [0u8; 256];
        dev.read_at(0, &mut buf).unwrap();
        assert_eq!(std::fs::read(f.path()).unwrap(), pattern);
    }
}
```

- [ ] 同时把 `crates/xd-device/src/lib.rs` 先改成一行 `pub mod image;`（否则 image.rs 不会被编译，Step 2 不会如预期失败）。

**Step 2: 运行测试确认失败**

Run: `cargo test -p xd-device`
Expected: 编译失败（`ImageFileDevice` 未定义）。

- [ ] **Step 3: 实现 `crates/xd-device/src/lib.rs`**

```rust
//! 块设备抽象：M0 仅实现只读镜像文件后端。

pub mod image;

use serde::{Deserialize, Serialize};

/// 设备信息（IPC 契约类型，JSON 用 camelCase，见 proto/v0/README.md）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub size_bytes: u64,
    pub removable: bool,
    pub fs_guess: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Physical,
    Volume,
    Image,
}

#[derive(Debug)]
pub enum DeviceError {
    Io(std::io::Error),
    NotAFile(String),
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceError::Io(e) => write!(f, "io error: {e}"),
            DeviceError::NotAFile(p) => write!(f, "not a regular file: {p}"),
        }
    }
}

impl std::error::Error for DeviceError {}

impl From<std::io::Error> for DeviceError {
    fn from(e: std::io::Error) -> Self {
        DeviceError::Io(e)
    }
}

/// 只读块设备：没有任何写接口，只读铁律由类型系统保证。
pub trait BlockDevice: Send + Sync {
    fn info(&self) -> &DeviceInfo;

    fn size_bytes(&self) -> u64 {
        self.info().size_bytes
    }

    /// 从 offset 起读取直到填满 buf、到达 EOF 或出错；返回实际读取字节数。
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError>;
}
```

- [ ] **Step 4: 实现 `crates/xd-device/src/image.rs`**

```rust
//! 只读镜像文件后端：M0 的测试与开发全部基于镜像，不依赖真实硬件。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use crate::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};

pub struct ImageFileDevice {
    info: DeviceInfo,
    file: Mutex<File>,
}

impl ImageFileDevice {
    pub fn open(path: &Path) -> Result<Self, DeviceError> {
        let file = File::open(path)?; // 永远只读打开
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(DeviceError::NotAFile(path.display().to_string()));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self {
            info: DeviceInfo {
                id: format!("image:{}", path.display()),
                name,
                kind: DeviceKind::Image,
                size_bytes: meta.len(),
                removable: false,
                fs_guess: None,
            },
            file: Mutex::new(file),
        })
    }
}

impl BlockDevice for ImageFileDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        if buf.is_empty() || offset >= self.info.size_bytes {
            return Ok(0);
        }
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(offset))?;
        let mut total = 0;
        while total < buf.len() {
            let n = file.read(&mut buf[total..])?;
            if n == 0 {
                break;
            }
            total += n;
        }
        Ok(total)
    }
}
```

（把 Step 1 的测试模块保留在 `image.rs` 末尾。）

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p xd-device`
Expected: 4 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/xd-device
git commit -m "feat(device): 块设备抽象与只读镜像后端"
```

---

### Task 4: xd-core —— JSON-RPC 信封类型 + golden 契约测试

**Files:**
- Create: `crates/xd-core/src/api.rs`、`crates/xd-core/tests/contract.rs`
- Modify: `crates/xd-core/src/lib.rs`

- [ ] **Step 1: 写 golden 契约测试 `crates/xd-core/tests/contract.rs`**

```rust
use std::path::PathBuf;
use xd_core::api::*;

fn golden(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../proto/v0/examples")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(text.trim()).unwrap()
}

#[test]
fn ping_request_matches_golden() {
    let parsed: Request = serde_json::from_value(golden("ping.request.json")).unwrap();
    assert_eq!(parsed.jsonrpc, "2.0");
    assert_eq!(parsed.id, serde_json::json!(1));
    assert_eq!(parsed.method, "ping");
    assert_eq!(parsed.params, None);
}

#[test]
fn device_list_request_matches_golden() {
    let parsed: Request = serde_json::from_value(golden("device_list.request.json")).unwrap();
    assert_eq!(parsed.jsonrpc, "2.0");
    assert_eq!(parsed.id, serde_json::json!(2));
    assert_eq!(parsed.method, "device.list");
    assert_eq!(parsed.params, None);
}

#[test]
fn ping_response_matches_golden() {
    let v = golden("ping.response.json");
    let parsed: Response = serde_json::from_value(v.clone()).unwrap();
    let expected = Response::Ok(RpcOk {
        jsonrpc: "2.0".into(),
        id: serde_json::json!(1),
        result: serde_json::json!({"pong": true, "version": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL_VERSION}),
    });
    assert_eq!(parsed, expected);
    assert_eq!(serde_json::to_value(&expected).unwrap(), v);
}

#[test]
fn device_list_response_matches_golden() {
    let v = golden("device_list.response.json");
    let parsed: Response = serde_json::from_value(v.clone()).unwrap();
    let Response::Ok(ok) = parsed else {
        panic!("expected Ok");
    };
    let devices = ok.result["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    let dev: xd_device::DeviceInfo = serde_json::from_value(devices[0].clone()).unwrap();
    assert_eq!(dev.name, "test.img");
    assert_eq!(dev.kind, xd_device::DeviceKind::Image);
    assert_eq!(dev.size_bytes, 4096);
    assert_eq!(dev.fs_guess, None);
    // 序列化方向：encode 输出必须与 golden 逐字段一致
    assert_eq!(serde_json::to_value(&dev).unwrap(), devices[0]);
}

#[test]
fn error_response_matches_golden() {
    let v = golden("error_method_not_found.response.json");
    let parsed: Response = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(
        parsed,
        Response::Err(RpcErr {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(7),
            error: RpcError::method_not_found("scan.start"),
        })
    );
    // encode 方向：RpcErr 独立于 RpcOk，需单独钉死
    assert_eq!(serde_json::to_value(&parsed).unwrap(), v);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xd-core`
Expected: 编译失败（`xd_core::api` 未定义）。

- [ ] **Step 3: 实现 `crates/xd-core/src/api.rs`**

```rust
//! JSON-RPC 2.0 信封类型（契约见 proto/v0/README.md）。

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 0;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Ok(RpcOk),
    Err(RpcErr),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcOk {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub result: serde_json::Value,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcErr {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub error: RpcError,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn parse_error() -> Self {
        Self { code: -32700, message: "Parse error".into() }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self { code: -32601, message: format!("Method not found: {method}") }
    }

    pub fn invalid_params(message: &str) -> Self {
        Self { code: -32602, message: format!("Invalid params: {message}") }
    }
}

/// 构造成功响应。
pub fn ok(req: &Request, result: serde_json::Value) -> Response {
    Response::Ok(RpcOk { jsonrpc: "2.0".into(), id: req.id.clone(), result })
}

/// 构造错误响应。
pub fn err(req: &Request, error: RpcError) -> Response {
    Response::Err(RpcErr { jsonrpc: "2.0".into(), id: req.id.clone(), error })
}
```

- [ ] **Step 4: 更新 `crates/xd-core/src/lib.rs`**

```rust
//! 小盾核心：编排、RPC 契约类型与处理器。

pub mod api;
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p xd-core`
Expected: 5 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/xd-core
git commit -m "feat(core): JSON-RPC 信封类型与 golden 契约测试"
```

---

### Task 5: xd-core —— 处理器（ping / device.list）

**Files:**
- Create: `crates/xd-core/src/handlers.rs`
- Modify: `crates/xd-core/src/lib.rs`

- [ ] **Step 1: 写失败的测试（`handlers.rs` 末尾）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use xd_device::image::ImageFileDevice;

    fn req(id: i64, method: &str) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(id),
            method: method.into(),
            params: None,
        }
    }

    #[test]
    fn ping_returns_pong_and_protocol() {
        let ctx = CoreCtx::new(vec![]);
        let resp = handle_request(&ctx, &req(1, "ping"));
        let Response::Ok(ok) = resp else { panic!("expected Ok") };
        assert_eq!(ok.id, serde_json::json!(1));
        assert_eq!(ok.result["pong"], serde_json::json!(true));
        assert_eq!(ok.result["protocol"], serde_json::json!(PROTOCOL_VERSION));
        assert_eq!(ok.result["version"], serde_json::json!(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn device_list_empty() {
        let ctx = CoreCtx::new(vec![]);
        let Response::Ok(ok) = handle_request(&ctx, &req(2, "device.list")) else { panic!() };
        assert_eq!(ok.result["devices"], serde_json::json!([]));
    }

    #[test]
    fn device_list_includes_image() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&[0u8; 4096]).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        let ctx = CoreCtx::new(vec![Box::new(dev)]);
        let Response::Ok(ok) = handle_request(&ctx, &req(3, "device.list")) else { panic!() };
        let devices = ok.result["devices"].as_array().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["kind"], serde_json::json!("image"));
        assert_eq!(devices[0]["sizeBytes"], serde_json::json!(4096));
    }

    #[test]
    fn unknown_method_returns_minus_32601() {
        let ctx = CoreCtx::new(vec![]);
        let Response::Err(e) = handle_request(&ctx, &req(7, "scan.start")) else { panic!() };
        assert_eq!(e.id, serde_json::json!(7));
        assert_eq!(e.error.code, -32601);
        assert_eq!(e.error.message, "Method not found: scan.start");
    }
}
```

`tempfile` 需要加到 xd-core 的 dev-dependencies。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xd-core`
Expected: 编译失败（`handlers` 未定义）。

- [ ] **Step 3: 实现 `crates/xd-core/src/handlers.rs`**

```rust
//! RPC 处理器：daemon（stdio）与 M4 的 ffi 共用同一入口。

use xd_device::{BlockDevice, DeviceInfo};

use crate::api::{Request, Response, RpcError, PROTOCOL_VERSION, err, ok};

pub struct CoreCtx {
    devices: Vec<Box<dyn BlockDevice>>,
}

impl CoreCtx {
    pub fn new(devices: Vec<Box<dyn BlockDevice>>) -> Self {
        Self { devices }
    }

    pub fn device_infos(&self) -> Vec<DeviceInfo> {
        self.devices.iter().map(|d| d.info().clone()).collect()
    }
}

pub fn handle_request(ctx: &CoreCtx, req: &Request) -> Response {
    match req.method.as_str() {
        "ping" => ok(
            req,
            serde_json::json!({
                "pong": true,
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": PROTOCOL_VERSION,
            }),
        ),
        "device.list" => ok(req, serde_json::json!({ "devices": ctx.device_infos() })),
        other => err(req, RpcError::method_not_found(other)),
    }
}
```

- [ ] **Step 4: 更新 `crates/xd-core/src/lib.rs` 与 `Cargo.toml`**

`lib.rs`：
```rust
//! 小盾核心：编排、RPC 契约类型与处理器。

pub mod api;
pub mod handlers;
```

`Cargo.toml` 增加：
```toml
[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p xd-core`
Expected: 9 passed（5 contract + 4 handlers）。

- [ ] **Step 6: Commit**

```bash
git add crates/xd-core Cargo.lock
git commit -m "feat(core): ping 与 device.list 处理器"
```

---

### Task 6: xd-daemon —— stdio JSON-RPC 循环

**Files:**
- Modify: `crates/xd-daemon/src/main.rs`
- Create: `crates/xd-daemon/tests/ipc.rs`

- [ ] **Step 1: 写失败的集成测试 `crates/xd-daemon/tests/ipc.rs`**

```rust
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Daemon {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Daemon {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, stdout }
    }

    fn call(&mut self, line: &str) -> serde_json::Value {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
        let mut buf = String::new();
        self.stdout.read_line(&mut buf).unwrap();
        serde_json::from_str(buf.trim()).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn ping_over_stdio() {
    let mut d = Daemon::start(&[]);
    let resp = d.call(r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":null}"#);
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["pong"], true);
    assert_eq!(resp["result"]["protocol"], 0);
}

#[test]
fn device_list_with_image() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&[0u8; 4096]).unwrap();
    f.flush().unwrap();
    let path = f.path().to_str().unwrap().to_string();
    let mut d = Daemon::start(&["--image", &path]);
    let resp = d.call(r#"{"jsonrpc":"2.0","id":2,"method":"device.list","params":null}"#);
    let devices = resp["result"]["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["kind"], "image");
    assert_eq!(devices[0]["sizeBytes"], 4096);
}

#[test]
fn unknown_method_returns_error_with_same_id() {
    let mut d = Daemon::start(&[]);
    let resp = d.call(r#"{"jsonrpc":"2.0","id":9,"method":"scan.start","params":null}"#);
    assert_eq!(resp["id"], 9);
    assert_eq!(resp["error"]["code"], -32601);
}

#[test]
fn malformed_line_does_not_kill_stream() {
    let mut d = Daemon::start(&[]);
    let resp = d.call("not json at all");
    assert_eq!(resp["error"]["code"], -32700);
    assert_eq!(resp["id"], serde_json::Value::Null);
    // 流仍然存活
    let resp = d.call(r#"{"jsonrpc":"2.0","id":11,"method":"ping","params":null}"#);
    assert_eq!(resp["id"], 11);
    assert_eq!(resp["result"]["pong"], true);
}
```

`crates/xd-daemon/Cargo.toml` 增加：
```toml
[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xd-daemon`
Expected: 编译通过但测试失败（`main` 为空，读不到响应/进程立即退出）。

- [ ] **Step 3: 实现 `crates/xd-daemon/src/main.rs`**

```rust
//! 小盾桌面特权进程：stdio JSON-RPC 服务（每行一条 JSON，见 proto/v0/README.md）。
//! 提权与设备枚举在 M1/M2 接入；M0 只支持 --image 注册镜像设备。

use std::io::{BufRead, Write};
use std::path::PathBuf;

use xd_core::api::{Request, Response, RpcErr, RpcError};
use xd_core::handlers::{CoreCtx, handle_request};
use xd_device::BlockDevice;
use xd_device::image::ImageFileDevice;

fn main() {
    let mut devices: Vec<Box<dyn BlockDevice>> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--image" => {
                let Some(path) = args.next() else {
                    eprintln!("error: --image requires a path");
                    std::process::exit(2);
                };
                match ImageFileDevice::open(&PathBuf::from(&path)) {
                    Ok(dev) => devices.push(Box::new(dev)),
                    Err(e) => {
                        eprintln!("error: cannot open image {path}: {e}");
                        std::process::exit(2);
                    }
                }
            }
            other => {
                eprintln!("error: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }

    let ctx = CoreCtx::new(devices);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
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
        if writeln!(stdout, "{}", serde_json::to_string(&response).unwrap()).is_err() {
            break;
        }
        let _ = stdout.flush();
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p xd-daemon`
Expected: 4 passed。

- [ ] **Step 5: Commit**

```bash
git add crates/xd-daemon Cargo.lock
git commit -m "feat(daemon): stdio JSON-RPC 服务与 --image 设备注册"
```

---

### Task 7: fixtures 生成脚本与 e2e 冒烟脚本

**Files:**
- Create: `fixtures/gen_image.sh`、`scripts/e2e.sh`

- [ ] **Step 1: 写 `fixtures/gen_image.sh`（确定性镜像：同样参数永远同样字节）**

```bash
#!/usr/bin/env bash
# 生成确定性测试镜像：默认 1 MiB，字节模式 (i*7+13) mod 256。
set -euo pipefail
out="${1:-fixtures/test.img}"
size="${2:-1048576}"
python3 - "$out" "$size" <<'PY'
import sys
out, size = sys.argv[1], int(sys.argv[2])
with open(out, "wb") as f:
    f.write(bytes((i * 7 + 13) % 256 for i in range(size)))
PY
echo "wrote ${out} (${size} bytes)"
```

- [ ] **Step 2: 写 `scripts/e2e.sh`**

```bash
#!/usr/bin/env bash
# M0 端到端冒烟：构建 daemon → 生成镜像 → 走 stdio 发 ping + device.list → 断言。
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -p xd-daemon --quiet
bash fixtures/gen_image.sh /tmp/xiaodun-m0-test.img 65536 >/dev/null

out=$(printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"ping","params":null}' \
  '{"jsonrpc":"2.0","id":2,"method":"device.list","params":null}' \
  | ./target/debug/xd-daemon --image /tmp/xiaodun-m0-test.img)

echo "$out"
echo "$out" | grep -q '"pong":true'  || { echo "FAIL: ping"; exit 1; }
echo "$out" | grep -q '"sizeBytes":65536' || { echo "FAIL: device.list size"; exit 1; }
echo "$out" | grep -q '"kind":"image"' || { echo "FAIL: device kind"; exit 1; }
echo "E2E OK"
```

- [ ] **Step 3: 赋权并运行**

Run: `chmod +x fixtures/gen_image.sh scripts/e2e.sh && bash scripts/e2e.sh`
Expected: 输出两行 JSON 后打印 `E2E OK`。

- [ ] **Step 4: Commit**

```bash
git add fixtures scripts
git commit -m "test: 确定性镜像生成与 M0 端到端冒烟脚本"
```

---

### Task 8: Flutter 应用骨架与协议模型

**前置：** Flutter SDK 可用（`flutter --version` 有输出；本机在 `~/flutter/bin/flutter`）。
**Files:**
- Create: `ui/`（`flutter create`）、`ui/lib/core_client/protocol.dart`、`ui/lib/core_client/core_client.dart`、`ui/test/protocol_test.dart`
- Modify: `ui/lib/main.dart`（替换模板）

- [ ] **Step 1: 生成 Flutter 工程**

Run:
```bash
flutter create --project-name xiaodun_ui --platforms=linux,windows,macos --empty ui
```
Expected: `ui/` 生成成功；`flutter analyze --no-fatal-infos`（在 ui/ 下）无错误。

- [ ] **Step 2: 写失败的测试 `ui/test/protocol_test.dart`**

```dart
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';

Map<String, dynamic> golden(String name) =>
    jsonDecode(File('../proto/v0/examples/$name').readAsStringSync()) as Map<String, dynamic>;

void main() {
  test('ping response golden decodes', () {
    final msg = golden('ping.response.json');
    final ping = PingResult.fromJson(msg['result'] as Map<String, dynamic>);
    expect(ping.pong, isTrue);
    expect(ping.version, isNotEmpty); // 版本值随发版变动（与 Rust 侧 env! 策略对齐），此处只验证字段解析
    expect(ping.protocol, 0);
  });

  test('device_list response golden decodes', () {
    final msg = golden('device_list.response.json');
    final devices = (msg['result']['devices'] as List)
        .map((e) => DeviceInfo.fromJson(e as Map<String, dynamic>))
        .toList();
    expect(devices, hasLength(1));
    expect(devices[0].name, 'test.img');
    expect(devices[0].kind, 'image');
    expect(devices[0].sizeBytes, 4096);
    expect(devices[0].fsGuess, isNull);
    expect(devices[0].toJson(), msg['result']['devices'][0]);
  });

  test('encodeRequest matches golden request', () {
    final encoded = jsonDecode(encodeRequest(id: 1, method: 'ping', params: null));
    expect(encoded, golden('ping.request.json'));
  });

  test('encodeRequest matches device_list request golden', () {
    final encoded = jsonDecode(encodeRequest(id: 2, method: 'device.list', params: null));
    expect(encoded, golden('device_list.request.json'));
  });

  test('error golden throws RpcException with code', () {
    final msg = golden('error_method_not_found.response.json');
    expect(
      () => decodeResult(msg),
      throwsA(isA<RpcException>().having((e) => e.code, 'code', -32601)),
    );
  });
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cd ui && flutter test test/protocol_test.dart`
Expected: FAIL（`protocol.dart` 不存在）。

- [ ] **Step 4: 实现 `ui/lib/core_client/protocol.dart`**

```dart
import 'dart:convert';

class DeviceInfo {
  const DeviceInfo({
    required this.id,
    required this.name,
    required this.kind,
    required this.sizeBytes,
    required this.removable,
    this.fsGuess,
  });

  final String id;
  final String name;
  final String kind; // physical | volume | image
  final int sizeBytes;
  final bool removable;
  final String? fsGuess;

  factory DeviceInfo.fromJson(Map<String, dynamic> json) => DeviceInfo(
        id: json['id'] as String,
        name: json['name'] as String,
        kind: json['kind'] as String,
        sizeBytes: json['sizeBytes'] as int,
        removable: json['removable'] as bool,
        fsGuess: json['fsGuess'] as String?,
      );

  Map<String, dynamic> toJson() => {
        'id': id,
        'name': name,
        'kind': kind,
        'sizeBytes': sizeBytes,
        'removable': removable,
        'fsGuess': fsGuess,
      };
}

class PingResult {
  const PingResult({required this.pong, required this.version, required this.protocol});

  final bool pong;
  final String version;
  final int protocol;

  factory PingResult.fromJson(Map<String, dynamic> json) => PingResult(
        pong: json['pong'] as bool,
        version: json['version'] as String,
        protocol: json['protocol'] as int,
      );
}

class RpcException implements Exception {
  const RpcException(this.code, this.message);
  final int code;
  final String message;
  @override
  String toString() => 'RpcException($code): $message';
}

String encodeRequest({required Object id, required String method, Object? params}) =>
    jsonEncode({'jsonrpc': '2.0', 'id': id, 'method': method, 'params': params});

/// 从一条完整响应消息中取出 result；错误响应抛出 [RpcException]。
Map<String, dynamic> decodeResult(Map<String, dynamic> message) {
  final error = message['error'];
  if (error != null) {
    final e = error as Map<String, dynamic>;
    throw RpcException(e['code'] as int, e['message'] as String);
  }
  return message['result'] as Map<String, dynamic>;
}
```

- [ ] **Step 5: 实现 `ui/lib/core_client/core_client.dart`**

```dart
import 'protocol.dart';

/// UI 只依赖这个抽象；桌面实现 = IpcCoreClient，移动端 = FfiCoreClient（M4）。
abstract class CoreClient {
  Future<PingResult> ping();
  Future<List<DeviceInfo>> listDevices();
}
```

- [ ] **Step 6: 运行测试确认通过**

Run: `cd ui && flutter test test/protocol_test.dart`
Expected: 4 passed。

- [ ] **Step 7: Commit**

```bash
git add ui
git commit -m "feat(ui): Flutter 骨架与 IPC 协议模型（golden 测试对齐）"
```

---

### Task 9: UI —— IpcTransport 与设备列表页

**Files:**
- Create: `ui/lib/core_client/ipc_transport.dart`、`ui/lib/home_page.dart`、`ui/test/home_page_test.dart`、`ui/test/ipc_integration_test.dart`
- Modify: `ui/lib/main.dart`

- [ ] **Step 1: 写失败的 widget 测试 `ui/test/home_page_test.dart`**

```dart
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/core_client.dart';
import 'package:xiaodun_ui/core_client/protocol.dart';
import 'package:xiaodun_ui/home_page.dart';

class FakeCoreClient implements CoreClient {
  FakeCoreClient(this.devices);
  final List<DeviceInfo> devices;

  @override
  Future<PingResult> ping() async => const PingResult(pong: true, version: 'test', protocol: 0);

  @override
  Future<List<DeviceInfo>> listDevices() async => devices;
}

class FailingCoreClient implements CoreClient {
  @override
  Future<PingResult> ping() async => throw const RpcException(-1, 'boom');

  @override
  Future<List<DeviceInfo>> listDevices() async => throw const RpcException(-1, 'boom');
}

void main() {
  testWidgets('shows device list from client', (tester) async {
    await tester.pumpWidget(MaterialApp(
      home: HomePage(
        client: FakeCoreClient(const [
          DeviceInfo(id: 'image:test.img', name: 'test.img', kind: 'image', sizeBytes: 4096, removable: false),
        ]),
      ),
    ));
    await tester.pumpAndSettle();
    expect(find.text('test.img'), findsOneWidget);
    expect(find.textContaining('image · 4.0 KB'), findsOneWidget);
  });

  testWidgets('shows error state with retry', (tester) async {
    await tester.pumpWidget(MaterialApp(home: HomePage(client: FailingCoreClient())));
    await tester.pumpAndSettle();
    expect(find.textContaining('boom'), findsOneWidget);
    expect(find.text('重试'), findsOneWidget);
  });
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd ui && flutter test test/home_page_test.dart`
Expected: FAIL（`home_page.dart` / `ipc_transport.dart` 不存在）。

- [ ] **Step 3: 实现 `ui/lib/core_client/ipc_transport.dart`**

```dart
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'core_client.dart';
import 'protocol.dart';

/// 桌面实现：spawn 特权 daemon，stdio 上每行一条 JSON-RPC 消息。
class IpcCoreClient implements CoreClient {
  IpcCoreClient._(this._process) {
    _sub = _process.stdout
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen(_onLine);
    _process.stderr
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen((line) => stderrLines.add(line));
    _process.exitCode.then(_onExit);
  }

  /// 启动 daemon。[daemonPath] 缺省取环境变量 XD_DAEMON_BIN。
  static Future<IpcCoreClient> start({String? daemonPath, List<String> extraArgs = const []}) async {
    final path = daemonPath ?? Platform.environment['XD_DAEMON_BIN'];
    if (path == null) {
      throw StateError('设置 XD_DAEMON_BIN 或传入 daemonPath 指向 xd-daemon 可执行文件');
    }
    final process = await Process.start(path, extraArgs);
    return IpcCoreClient._(process);
  }

  final Process _process;
  late final StreamSubscription<String> _sub;
  final List<String> stderrLines = [];
  final Map<int, Completer<Map<String, dynamic>>> _pending = {};
  int _nextId = 0;

  Future<Map<String, dynamic>> _call(String method) {
    final id = ++_nextId;
    final completer = Completer<Map<String, dynamic>>();
    _pending[id] = completer;
    _process.stdin.writeln(encodeRequest(id: id, method: method, params: null));
    return completer.future.timeout(
      const Duration(seconds: 10),
      onTimeout: () {
        _pending.remove(id);
        throw TimeoutException('RPC $method timed out');
      },
    );
  }

  void _onLine(String line) {
    if (line.trim().isEmpty) return;
    final Map<String, dynamic> message;
    try {
      message = jsonDecode(line) as Map<String, dynamic>;
    } catch (_) {
      return; // 无法解析的行直接忽略，不打断流
    }
    final id = message['id'];
    if (id is! int) return;
    final completer = _pending.remove(id);
    if (completer == null) return;
    try {
      completer.complete(decodeResult(message));
    } on RpcException catch (e) {
      completer.completeError(e);
    }
  }

  void _onExit(int code) {
    for (final completer in _pending.values) {
      completer.completeError(StateError('daemon exited with code $code'));
    }
    _pending.clear();
  }

  @override
  Future<PingResult> ping() async => PingResult.fromJson(await _call('ping'));

  @override
  Future<List<DeviceInfo>> listDevices() async {
    final result = await _call('device.list');
    return (result['devices'] as List)
        .map((e) => DeviceInfo.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  /// 关闭 daemon（结束时调用，避免 UI 退出留下孤儿进程）。
  Future<void> close() async {
    await _sub.cancel();
    _process.kill();
    await _process.exitCode;
  }
}
```

- [ ] **Step 4: 实现 `ui/lib/home_page.dart`**

```dart
import 'package:flutter/material.dart';

import 'core_client/core_client.dart';
import 'core_client/protocol.dart';

class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.client});

  final CoreClient client;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  late Future<List<DeviceInfo>> _devices;

  @override
  void initState() {
    super.initState();
    _reload();
  }

  void _reload() {
    setState(() => _devices = widget.client.listDevices());
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('小盾 · 选择设备')),
      body: FutureBuilder<List<DeviceInfo>>(
        future: _devices,
        builder: (context, snapshot) {
          if (snapshot.connectionState != ConnectionState.done) {
            return const Center(child: CircularProgressIndicator());
          }
          if (snapshot.hasError) {
            return Center(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text('读取设备失败：${snapshot.error}'),
                  const SizedBox(height: 12),
                  ElevatedButton(onPressed: _reload, child: const Text('重试')),
                ],
              ),
            );
          }
          final devices = snapshot.data ?? const <DeviceInfo>[];
          if (devices.isEmpty) {
            return const Center(child: Text('未发现设备'));
          }
          return ListView.separated(
            itemCount: devices.length,
            separatorBuilder: (_, _) => const Divider(height: 1),
            itemBuilder: (context, index) => _DeviceTile(device: devices[index]),
          );
        },
      ),
    );
  }
}

class _DeviceTile extends StatelessWidget {
  const _DeviceTile({required this.device});

  final DeviceInfo device;

  @override
  Widget build(BuildContext context) {
    return ListTile(
      leading: const Icon(Icons.storage),
      title: Text(device.name),
      subtitle: Text('${device.kind} · ${formatBytes(device.sizeBytes)}'),
      onTap: () {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('扫描功能将在 M1 接入')),
        );
      },
    );
  }
}

String formatBytes(int bytes) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  var value = bytes.toDouble();
  var unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return '${value.toStringAsFixed(1)} ${units[unit]}';
}
```

- [ ] **Step 5: 替换 `ui/lib/main.dart`**

```dart
import 'package:flutter/material.dart';

import 'core_client/core_client.dart';
import 'core_client/ipc_transport.dart';
import 'home_page.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // M0：真实 daemon 通过 XD_DAEMON_BIN 指定；无 daemon 时 UI 显示错误态。
  CoreClient client;
  try {
    client = await IpcCoreClient.start();
  } on StateError {
    client = _MissingDaemonClient();
  }
  runApp(XiaodunApp(client: client));
}

class XiaodunApp extends StatelessWidget {
  const XiaodunApp({super.key, required this.client});

  final CoreClient client;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: '小盾',
      theme: ThemeData(colorSchemeSeed: const Color(0xFF2E6BE6), useMaterial3: true),
      home: HomePage(client: client),
    );
  }
}

class _MissingDaemonClient implements CoreClient {
  @override
  Future<PingResult> ping() async => throw StateError('未找到 daemon：请设置 XD_DAEMON_BIN');

  @override
  Future<List<DeviceInfo>> listDevices() async =>
      throw StateError('未找到 daemon：请设置 XD_DAEMON_BIN');
}
```

（`_MissingDaemonClient` 用到 `protocol.dart` 的 `PingResult`/`DeviceInfo`，需要 `import 'core_client/protocol.dart';`。）

- [ ] **Step 6: 全量验证**

Run:
```bash
cd ui && flutter analyze && flutter test
```
Expected: analyze 无 error；protocol_test 5 passed、home_page_test 2 passed。

- [ ] **Step 7: 真实 daemon 集成测试 `ui/test/ipc_integration_test.dart`**

```dart
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:xiaodun_ui/core_client/ipc_transport.dart';

void main() {
  final bin = Platform.environment['XD_DAEMON_BIN'];
  test(
    'handshake with real daemon (ping + device.list)',
    () async {
      final dir = Directory.systemTemp.createTempSync('xd_ui_it');
      final image = File('${dir.path}/test.img')..writeAsBytesSync(List<int>.filled(4096, 0));
      final client = await IpcCoreClient.start(daemonPath: bin, extraArgs: ['--image', image.path]);
      try {
        final ping = await client.ping();
        expect(ping.pong, isTrue);
        expect(ping.protocol, 0);
        final devices = await client.listDevices();
        expect(devices, hasLength(1));
        expect(devices.single.kind, 'image');
        expect(devices.single.sizeBytes, 4096);
      } finally {
        await client.close();
      }
    },
    skip: bin == null ? 'XD_DAEMON_BIN 未设置，跳过' : null,
  );
}
```

Run:
```bash
cargo build -p xd-daemon
cd ui && XD_DAEMON_BIN=../target/debug/xd-daemon flutter test
```
Expected: 全部通过（M0 出口：真实 UI 代码路径 ↔ 真实 daemon 握手）。

- [ ] **Step 8: Commit**

```bash
git add ui
git commit -m "feat(ui): IpcCoreClient 与设备列表页（可对接真实 daemon）"
```

---

### Task 10: CI 矩阵与 M0 全量验证

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: 写 `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  rust:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace

  flutter:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: ui
    steps:
      - uses: actions/checkout@v4
      - uses: subosito/flutter-action@v2
        with:
          channel: stable
      - run: flutter analyze
      - run: flutter test
```

- [ ] **Step 2: 本地全量验证（模拟 CI）**

Run:
```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/e2e.sh
cd ui && flutter analyze && flutter test
```
Expected: clippy 0 warning；全部测试 passed；`E2E OK`；flutter analyze/test 通过。
若 `cargo fmt --all` 产生改动，与 ci.yml 一并提交。

- [ ] **Step 3: Commit**

```bash
git add -u
git add .github/workflows/ci.yml
git commit -m "ci: Rust 三平台矩阵与 Flutter job"
```

---

## M0 出口验收（全部打勾才算完成）

- [ ] `cargo test --workspace` 全绿；`cargo clippy --workspace --all-targets -- -D warnings` 无告警
- [ ] `bash scripts/e2e.sh` 输出 `E2E OK`
- [ ] `cd ui && flutter analyze && flutter test` 全绿
- [ ] golden 契约测试在 Rust 和 Dart 两侧同时通过（Task 4 / Task 8 的测试）
- [ ] `ui` 通过 `XD_DAEMON_BIN` 对接真实 daemon，设备列表显示镜像设备（Task 9 Step 7）
- [ ] CI 文件就位（推送后三平台绿）

## 交接说明

- M1 起点：`xd-fs-fat`（FAT/exFAT 快速扫描）、carving v1（JPEG/PNG）、扫描三页 UI、Windows 提权打包。届时按需新增 crate 成员与 proto 方法（`scan.start`/`scan.progress` 事件流）。
- M0 未做但已为此预留的形状：BlockDevice trait（物理设备后端直接实现它）、RpcError 错误码表、golden 契约流程（新方法 = 新 golden + 两侧测试）。
- 质量审查登记（不阻塞 M0）：① M1 动工前给 `BlockDevice::read_at` 补一行 doc「M0 支持任意偏移；M1+ 真实设备可能要求扇区对齐」；② 顺手补 3 个浅测试：`read_at` 空 buf 分支、`open` 不存在路径、`info().name` 字段断言；③ `DeviceError::source()` 可选实现；④ M4 xd-ffi 在 Rust 侧消费 Response 前，评估 untagged 判别的 Err 优先改造（result+error 并存目前会被 Ok 静默吞掉）。
