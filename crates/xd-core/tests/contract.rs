// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
use std::path::PathBuf;
use xd_core::api::*;

fn golden(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../proto/v0/examples")
        .join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
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
    let mut v = golden("ping.response.json");
    // version 是随发布变动的动态值：golden 必须以 "<VERSION>" 占位（约定见 proto/v0/README.md），
    // 先钉占位、再归一为真实 crate 版本做全等比对——发版不再改契约文件（v0.2.0 注）。
    assert_eq!(
        v["result"]["version"],
        serde_json::Value::String("<VERSION>".into()),
        "ping.response.json 的 version 必须为 \"<VERSION>\" 占位"
    );
    v["result"]["version"] = serde_json::Value::String(env!("CARGO_PKG_VERSION").into());
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
    assert_eq!(serde_json::to_value(&parsed).unwrap(), v);
}
