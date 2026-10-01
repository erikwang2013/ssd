//! RPC 处理器：daemon（stdio）与 M4 的 ffi 共用同一入口。

use xd_device::{BlockDevice, DeviceInfo};

use crate::api::{PROTOCOL_VERSION, Request, Response, RpcError, err, ok};

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
        let Response::Ok(ok) = resp else {
            panic!("expected Ok")
        };
        assert_eq!(ok.id, serde_json::json!(1));
        assert_eq!(ok.result["pong"], serde_json::json!(true));
        assert_eq!(ok.result["protocol"], serde_json::json!(PROTOCOL_VERSION));
        assert_eq!(
            ok.result["version"],
            serde_json::json!(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn device_list_empty() {
        let ctx = CoreCtx::new(vec![]);
        let Response::Ok(ok) = handle_request(&ctx, &req(2, "device.list")) else {
            panic!()
        };
        assert_eq!(ok.result["devices"], serde_json::json!([]));
    }

    #[test]
    fn device_list_includes_image() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&[0u8; 4096]).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        let ctx = CoreCtx::new(vec![Box::new(dev)]);
        let Response::Ok(ok) = handle_request(&ctx, &req(3, "device.list")) else {
            panic!()
        };
        let devices = ok.result["devices"].as_array().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["kind"], serde_json::json!("image"));
        assert_eq!(devices[0]["sizeBytes"], serde_json::json!(4096));
    }

    #[test]
    fn unknown_method_returns_minus_32601() {
        let ctx = CoreCtx::new(vec![]);
        let Response::Err(e) = handle_request(&ctx, &req(7, "scan.start")) else {
            panic!()
        };
        assert_eq!(e.id, serde_json::json!(7));
        assert_eq!(e.error.code, -32601);
        assert_eq!(e.error.message, "Method not found: scan.start");
    }

    #[test]
    fn ping_round_trip_matches_response_golden() {
        // 走真实输出路径：请求 golden → handle_request → 与响应 golden 全等（含 jsonrpc 字段）
        let ctx = CoreCtx::new(vec![]);
        let req: Request = serde_json::from_str(
            include_str!("../../../proto/v0/examples/ping.request.json").trim(),
        )
        .unwrap();
        let resp = handle_request(&ctx, &req);
        let expected: serde_json::Value = serde_json::from_str(
            include_str!("../../../proto/v0/examples/ping.response.json").trim(),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&resp).unwrap(), expected);
    }
}
