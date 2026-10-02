// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! RPC 处理器：daemon（stdio）与 M4 的 ffi 共用同一入口。

use xd_device::{BlockDevice, DeviceInfo};

use crate::api::{PROTOCOL_VERSION, Request, Response, RpcError, err, ok};

pub struct CoreCtx {
    devices: Vec<Box<dyn BlockDevice>>,
    /// 枚举到但**未打开**的设备（device.list 用；零 open()——M1e 契约要求）。
    list_only: Vec<DeviceInfo>,
}

impl CoreCtx {
    pub fn new(devices: Vec<Box<dyn BlockDevice>>) -> Self {
        Self {
            devices,
            list_only: Vec::new(),
        }
    }

    pub fn with_list_only(mut self, infos: Vec<DeviceInfo>) -> Self {
        self.list_only = infos;
        self
    }

    /// 双向 first-wins 去重：打开项在前，同 id 只留首个（含打开项内部双开——
    /// `--device /dev/sda --device /dev/disk/by-id/…` canonicalize 同 id）；
    /// `list_only` 与已打开 id 重复的同样丢弃，否则同一块盘在 device.list 出现两次（qual-t1 I1 / qual-t2 Minor 5）。
    pub fn device_infos(&self) -> Vec<DeviceInfo> {
        let mut v: Vec<DeviceInfo> = Vec::new();
        for info in self.devices.iter().map(|d| d.info()).chain(&self.list_only) {
            if !v.iter().any(|e| e.id == info.id) {
                v.push(info.clone());
            }
        }
        v
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
    fn device_list_merges_list_only_after_opened() {
        use xd_device::{DeviceInfo, DeviceKind};
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&[0u8; 512]).unwrap();
        let img = ImageFileDevice::open(f.path()).unwrap();
        let ctx = CoreCtx::new(vec![Box::new(img)]).with_list_only(vec![DeviceInfo {
            id: "unix:/dev/sda".into(),
            name: "Disk".into(),
            kind: DeviceKind::Physical,
            size_bytes: 1 << 40,
            removable: false,
            fs_guess: None,
        }]);
        let infos = ctx.device_infos();
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].kind, DeviceKind::Image); // devices 在前（既有顺序不变）
        assert_eq!(infos[1].id, "unix:/dev/sda");
    }

    #[test]
    fn device_list_dedupes_by_id() {
        use xd_device::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};
        struct Stub;
        impl BlockDevice for Stub {
            fn info(&self) -> &DeviceInfo {
                static I: std::sync::OnceLock<DeviceInfo> = std::sync::OnceLock::new();
                I.get_or_init(|| DeviceInfo {
                    id: "unix:/dev/sda".into(),
                    name: "opened".into(),
                    kind: DeviceKind::Physical,
                    size_bytes: 42,
                    removable: false,
                    fs_guess: None,
                })
            }
            fn read_at(&self, _o: u64, _b: &mut [u8]) -> Result<usize, DeviceError> {
                Ok(0)
            }
        }
        let ctx = CoreCtx::new(vec![Box::new(Stub)]).with_list_only(vec![DeviceInfo {
            id: "unix:/dev/sda".into(), // 与打开项同 id → 必须被去重
            name: "enumerated".into(),
            kind: DeviceKind::Physical,
            size_bytes: 42,
            removable: false,
            fs_guess: None,
        }]);
        let infos = ctx.device_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "opened");
    }

    #[test]
    fn device_list_dedupes_double_open_same_id() {
        // 同一物理盘经两条路径双开（/dev/sda 与 /dev/disk/by-id/… canonicalize 同 id）→ 只留首个
        use xd_device::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};
        struct S(DeviceInfo);
        impl BlockDevice for S {
            fn info(&self) -> &DeviceInfo {
                &self.0
            }
            fn read_at(&self, _o: u64, _b: &mut [u8]) -> Result<usize, DeviceError> {
                Ok(0)
            }
        }
        let mk = |name: &str| -> Box<dyn BlockDevice> {
            Box::new(S(DeviceInfo {
                id: "unix:/dev/sda".into(),
                name: name.into(),
                kind: DeviceKind::Physical,
                size_bytes: 42,
                removable: false,
                fs_guess: None,
            }))
        };
        let infos = CoreCtx::new(vec![mk("first"), mk("second")]).device_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "first");
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
        // 走真实输出路径：请求 golden → handle_request → 与响应 golden 全等（含 jsonrpc 字段）。
        // version 是随发布变动的动态值：golden 存 "<VERSION>" 占位，先把实际值归一为占位再全等比对，
        // 然后单独断言实际值 == 本 crate 版本——golden 跨发布稳定，发版不再改契约文件（v0.2.0 注）。
        let ctx = CoreCtx::new(vec![]);
        let req: Request = serde_json::from_str(
            include_str!("../../../proto/v0/examples/ping.request.json").trim(),
        )
        .unwrap();
        let resp = handle_request(&ctx, &req);
        let mut actual = serde_json::to_value(&resp).unwrap();
        assert_eq!(
            actual["result"]["version"],
            serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string())
        );
        actual["result"]["version"] = serde_json::Value::String("<VERSION>".into());
        let expected: serde_json::Value = serde_json::from_str(
            include_str!("../../../proto/v0/examples/ping.response.json").trim(),
        )
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn error_round_trip_matches_response_golden() {
        // err() 输出路径同样钉死（jsonrpc 字面量在 Err 分支独立存在）
        let ctx = CoreCtx::new(vec![]);
        let resp = handle_request(&ctx, &req(7, "scan.start"));
        let expected: serde_json::Value = serde_json::from_str(
            include_str!("../../../proto/v0/examples/error_method_not_found.response.json").trim(),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&resp).unwrap(), expected);
    }
}
