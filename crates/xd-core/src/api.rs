// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! JSON-RPC 2.0 信封类型（契约见 proto/v0/README.md 与 proto/v1/README.md）。

use serde::{Deserialize, Serialize};

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
    pub quality: String, // "complete" | "maybeDamaged" | "carved"
    pub first_cluster: u32,
    /// 雕刻条目在未分配空间内的起始字节坐标；FS 条目恒 None（序列化省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_offset: Option<u64>,
    /// exfat 专用拓扑提示：`Some(true)` = NoFatChain（规范保证连续）；`Some(false)` = 走 FAT 链。
    /// fat/carved/迁移前旧行为 None（序列化省略）——反构造读取时按 `false` 处理（只信链，
    /// 不猜连续）；旧客户端缺省解析为 None，语义与 M1d 前一致。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contiguous: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanState {
    Pending,
    Scanning,
    Paused,
    Canceled,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub task_id: u64,
    pub state: ScanState,
    pub read_bytes: u64,
    pub found_count: u64,
    pub elapsed_ms: u64,
}

/// `scan.start` 参数（契约 v1）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStartParams {
    pub device: String,
    #[serde(default)]
    pub mode: Option<String>,
}

/// `scan.status`/`scan.pause`/`scan.resume`/`scan.cancel` 参数（契约 v1）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskIdParams {
    pub task_id: u64,
}

/// `scan.results` 参数（契约 v1）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResultsParams {
    pub task_id: u64,
    pub offset: u64,
    pub limit: u64,
    #[serde(default)]
    pub deleted_only: bool,
}

/// `fs.read` 参数（契约 v1.2）：`length` ∈ 1..=1048576，由 handlers 校验。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsReadParams {
    pub task_id: u64,
    pub idx: u64,
    pub offset: u64,
    pub length: u64,
}

/// `export.start` 参数（契约 v1.2）：`idxs` 去重后非空且 ≤100000，`targetDir` 为绝对路径。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStartParams {
    pub task_id: u64,
    pub idxs: Vec<u64>,
    pub target_dir: String,
}

/// `export.cancel` 参数（契约 v1.2）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportIdParams {
    pub export_id: u64,
}

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
        Self {
            code: -32700,
            message: "Parse error".into(),
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Method not found: {method}"),
        }
    }

    pub fn invalid_params(message: &str) -> Self {
        Self {
            code: -32602,
            message: format!("Invalid params: {message}"),
        }
    }

    pub fn device_permission(id: &str) -> Self {
        Self {
            code: -32001,
            message: format!("Device permission denied: {id}"),
        }
    }

    pub fn unsupported_fs() -> Self {
        Self {
            code: -32002,
            message: "Unsupported file system".into(),
        }
    }

    pub fn task_not_found(id: u64) -> Self {
        Self {
            code: -32003,
            message: format!("Task not found: {id}"),
        }
    }

    pub fn task_not_active(id: u64) -> Self {
        Self {
            code: -32004,
            message: format!("Task not active: {id}"),
        }
    }

    /// -32005：空闲空间不可判定（深扫前置：位图/FAT 表不可读）——不得伪装成「无空闲」。
    pub fn unallocated_unavailable() -> Self {
        Self {
            code: -32005,
            message: "Cannot determine free space".into(),
        }
    }

    /// -32006：恢复目标落在源设备上（`st_dev(目标) == st_rdev(源块设备)`，root 降权前判定）。
    pub fn target_on_source(dir: &str) -> Self {
        Self {
            code: -32006,
            message: format!("Target is on the source device: {dir}"),
        }
    }

    /// -32007：目标目录不存在/非目录/不可写。
    pub fn target_not_writable(dir: &str) -> Self {
        Self {
            code: -32007,
            message: format!("Target not writable: {dir}"),
        }
    }

    /// -32008：条目不存在（`taskId`+`idx` 无此条目）。
    pub fn entry_not_found(idx: u64) -> Self {
        Self {
            code: -32008,
            message: format!("Entry not found: {idx}"),
        }
    }

    /// -32009：条目过大（`fs.read` 预览上限，导出不受此限）。
    pub fn entry_too_large(size: u64) -> Self {
        Self {
            code: -32009,
            message: format!("Entry too large: {size}"),
        }
    }

    /// -32010：目标盘余量不足（`need` = 预估交付字节上界）。
    pub fn insufficient_space(need: u64) -> Self {
        Self {
            code: -32010,
            message: format!("Insufficient space on target: need {need} bytes"),
        }
    }

    pub fn cannot_open(id: &str) -> Self {
        Self {
            code: -32602,
            message: format!("Cannot open device: {id}"),
        }
    }

    pub fn internal() -> Self {
        Self {
            code: -32603,
            message: "Internal error".into(),
        }
    }
}

/// 构造成功响应。
pub fn ok(req: &Request, result: serde_json::Value) -> Response {
    Response::Ok(RpcOk {
        jsonrpc: "2.0".into(),
        id: req.id.clone(),
        result,
    })
}

/// 构造错误响应。
pub fn err(req: &Request, error: RpcError) -> Response {
    Response::Err(RpcErr {
        jsonrpc: "2.0".into(),
        id: req.id.clone(),
        error,
    })
}
