// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
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
