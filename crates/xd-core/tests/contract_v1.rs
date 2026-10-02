// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 契约 v1 golden 双侧断言：decode 为强类型 + re-encode 全等（golden 唯一事实源约定与
//! `<VERSION>` 占位归一法沿用 v0，见 proto/v1/README.md 与 contract.rs）。

use std::path::PathBuf;

use xd_core::api::*;

fn golden(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../proto/v1/examples")
        .join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("parse {name}: {e}"))
}

/// golden 集合收口：新增/删除契约文件必须同步改测试（21 个是 v1 冻结清单）。
#[test]
fn golden_set_is_exactly_the_21_contract_files() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto/v1/examples");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut expected = [
        "device_list.request.json",
        "device_list.response.json",
        "error_device_permission.response.json",
        "error_task_not_active.response.json",
        "error_unsupported_fs.response.json",
        "ping.request.json",
        "ping.response.json",
        "scan_cancel.request.json",
        "scan_cancel.response.json",
        "scan_finished.notification.json",
        "scan_pause.request.json",
        "scan_pause.response.json",
        "scan_progress.notification.json",
        "scan_results.request.json",
        "scan_results.response.json",
        "scan_resume.request.json",
        "scan_resume.response.json",
        "scan_start.request.json",
        "scan_start.response.json",
        "scan_status.request.json",
        "scan_status.response.json",
    ];
    expected.sort();
    assert_eq!(names, expected);
}

fn request_golden(name: &str, id: i64, method: &str) -> Request {
    let v = golden(name);
    let req: Request = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(req.jsonrpc, "2.0");
    assert_eq!(req.id, serde_json::json!(id));
    assert_eq!(req.method, method);
    assert_eq!(serde_json::to_value(&req).unwrap(), v, "{name} re-encode");
    req
}

fn ok_result(name: &str) -> (serde_json::Value, serde_json::Value) {
    let v = golden(name);
    let parsed: Response = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        v,
        "{name} re-encode"
    );
    let Response::Ok(ok) = parsed else {
        panic!("{name}: expected Ok");
    };
    (v, ok.result)
}

#[test]
fn request_goldens_decode_and_reencode() {
    assert_eq!(request_golden("ping.request.json", 1, "ping").params, None);
    let dl = request_golden("device_list.request.json", 2, "device.list");
    assert_eq!(dl.params, None);

    let ss = request_golden("scan_start.request.json", 3, "scan.start");
    assert_eq!(
        ss.params,
        Some(serde_json::json!({"device": "unix:/dev/sdb", "mode": "quick"}))
    );
    let st = request_golden("scan_status.request.json", 4, "scan.status");
    assert_eq!(st.params, Some(serde_json::json!({"taskId": 1})));
    let rs = request_golden("scan_results.request.json", 5, "scan.results");
    assert_eq!(
        rs.params,
        Some(serde_json::json!({"taskId": 1, "offset": 0, "limit": 2, "deletedOnly": false}))
    );
    let pa = request_golden("scan_pause.request.json", 6, "scan.pause");
    assert_eq!(pa.params, Some(serde_json::json!({"taskId": 1})));
    let ca = request_golden("scan_cancel.request.json", 7, "scan.cancel");
    assert_eq!(ca.params, Some(serde_json::json!({"taskId": 1})));
    let re = request_golden("scan_resume.request.json", 8, "scan.resume");
    assert_eq!(re.params, Some(serde_json::json!({"taskId": 1})));
}

#[test]
fn ping_response_matches_golden() {
    // version 为随发布变动的动态值：先钉 "<VERSION>" 占位、归一为真实版本后全等比对
    //（约定见 proto/v1/README.md 引用的 v0 段），再单独钉 protocol=1。
    let mut v = golden("ping.response.json");
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
        result: serde_json::json!({
            "pong": true,
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": PROTOCOL_VERSION,
        }),
    });
    assert_eq!(parsed, expected);
    assert_eq!(serde_json::to_value(&expected).unwrap(), v);
}

#[test]
fn device_list_response_matches_golden() {
    let (_, result) = ok_result("device_list.response.json");
    let devices = result["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);

    let image: xd_device::DeviceInfo = serde_json::from_value(devices[0].clone()).unwrap();
    assert_eq!(image.kind, xd_device::DeviceKind::Image);
    assert_eq!(image.transport, None); // 镜像恒缺（缺失=未知）
    assert_eq!(serde_json::to_value(&image).unwrap(), devices[0]); // null 键省略（v0 兼容）

    let usb: xd_device::DeviceInfo = serde_json::from_value(devices[1].clone()).unwrap();
    assert_eq!(usb.id, "unix:/dev/sdb");
    assert_eq!(usb.transport.as_deref(), Some("usb"));
    assert!(usb.removable);
    assert_eq!(serde_json::to_value(&usb).unwrap(), devices[1]);
}

#[test]
fn scan_start_response_matches_golden() {
    let (_, result) = ok_result("scan_start.response.json");
    assert_eq!(result["taskId"], serde_json::json!(1));
    assert_eq!(result["fs"], serde_json::json!("exfat"));
    assert_eq!(result["totalBytes"], serde_json::json!(3907029168u64));
}

#[test]
fn scan_progress_shape_matches_status_and_notification_goldens() {
    let (_, result) = ok_result("scan_status.response.json");
    let p: ScanProgress = serde_json::from_value(result.clone()).unwrap();
    assert_eq!(
        p,
        ScanProgress {
            task_id: 1,
            state: ScanState::Scanning,
            read_bytes: 123456,
            found_count: 42,
            elapsed_ms: 1500,
        }
    );
    assert_eq!(serde_json::to_value(&p).unwrap(), result);

    // 通知：无 id（JSON-RPC 2.0 notification）、method/params 逐字，construct 侧同形。
    let n = golden("scan_progress.notification.json");
    assert_eq!(n["jsonrpc"], serde_json::json!("2.0"));
    assert!(n.get("id").is_none(), "通知不得携带 id");
    assert_eq!(n["method"], serde_json::json!("scan.progress"));
    let p2: ScanProgress = serde_json::from_value(n["params"].clone()).unwrap();
    assert_eq!(p2, p);
    assert_eq!(
        xd_core::notify::notification("scan.progress", n["params"].clone()),
        n
    );
}

#[test]
fn scan_finished_notification_matches_golden() {
    let n = golden("scan_finished.notification.json");
    assert_eq!(n["jsonrpc"], serde_json::json!("2.0"));
    assert!(n.get("id").is_none(), "通知不得携带 id");
    assert_eq!(n["method"], serde_json::json!("scan.finished"));
    let state: ScanState = serde_json::from_value(n["params"]["state"].clone()).unwrap();
    assert_eq!(state, ScanState::Completed);
    // 小写枚举往返（六变体全量字面量见 scan_state_all_variants_serialize_to_contract_literals）
    assert_eq!(serde_json::to_value(state).unwrap(), n["params"]["state"]);
    assert_eq!(
        xd_core::notify::notification("scan.finished", n["params"].clone()),
        n
    );
}

#[test]
fn scan_state_all_variants_serialize_to_contract_literals() {
    // v1 契约状态字面量全量（README 状态表）；golden 覆盖 scanning/paused/canceled/completed，
    // pending/failed 无 golden——双向往返逐字钉死，防枚举变体改名或漏映射。
    for (state, literal) in [
        (ScanState::Pending, "pending"),
        (ScanState::Scanning, "scanning"),
        (ScanState::Paused, "paused"),
        (ScanState::Canceled, "canceled"),
        (ScanState::Completed, "completed"),
        (ScanState::Failed, "failed"),
    ] {
        assert_eq!(
            serde_json::to_value(state).unwrap(),
            serde_json::json!(literal)
        );
        assert_eq!(
            serde_json::from_value::<ScanState>(serde_json::json!(literal)).unwrap(),
            state
        );
    }
}

#[test]
fn scan_results_response_matches_golden() {
    let (_, result) = ok_result("scan_results.response.json");
    assert_eq!(result["total"], serde_json::json!(42));
    let entries: Vec<ScanEntry> = serde_json::from_value(result["entries"].clone()).unwrap();
    assert_eq!(entries.len(), 2);
    let deleted = &entries[0];
    assert_eq!(deleted.idx, 0);
    assert_eq!(deleted.name, "IMG_0001.JPG");
    assert_eq!(deleted.path, "/DCIM");
    assert_eq!(deleted.ext, "jpg");
    assert_eq!(deleted.size_bytes, 12000);
    assert!(deleted.deleted && !deleted.is_dir);
    assert_eq!(deleted.quality, "complete");
    assert_eq!(deleted.first_cluster, 6);
    let live = &entries[1];
    assert_eq!(live.name, "READ_ME.TXT");
    assert_eq!(live.path, "/");
    assert!(!live.deleted && !live.is_dir);
    assert_eq!(serde_json::to_value(&entries).unwrap(), result["entries"]);
}

#[test]
fn task_state_responses_match_golden() {
    for (name, state) in [
        ("scan_pause.response.json", ScanState::Paused),
        ("scan_resume.response.json", ScanState::Scanning),
        ("scan_cancel.response.json", ScanState::Canceled),
    ] {
        let (_, result) = ok_result(name);
        assert_eq!(result["taskId"], serde_json::json!(1), "{name}");
        let s: ScanState = serde_json::from_value(result["state"].clone()).unwrap();
        assert_eq!(s, state, "{name}");
        assert_eq!(serde_json::to_value(s).unwrap(), result["state"], "{name}");
    }
}

#[test]
fn error_responses_match_golden() {
    for (name, id, error) in [
        (
            "error_device_permission.response.json",
            3,
            RpcError::device_permission("unix:/dev/sdb"),
        ),
        (
            "error_unsupported_fs.response.json",
            3,
            RpcError::unsupported_fs(),
        ),
        (
            "error_task_not_active.response.json",
            9,
            RpcError::task_not_active(1),
        ),
    ] {
        let v = golden(name);
        let parsed: Response = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(
            parsed,
            Response::Err(RpcErr {
                jsonrpc: "2.0".into(),
                id: serde_json::json!(id),
                error,
            }),
            "{name}"
        );
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            v,
            "{name} re-encode"
        );
    }
}

#[test]
fn error_constructor_messages_pin_contract_text() {
    // 无 golden 的三个码（README v1 错误表）：文案即契约，逐字钉死。
    assert_eq!(
        RpcError::task_not_found(7),
        RpcError {
            code: -32003,
            message: "Task not found: 7".into(),
        }
    );
    assert_eq!(
        RpcError::cannot_open("unix:/dev/sdb"),
        RpcError {
            code: -32602,
            message: "Cannot open device: unix:/dev/sdb".into(),
        }
    );
    assert_eq!(
        RpcError::internal(),
        RpcError {
            code: -32603,
            message: "Internal error".into(),
        }
    );
}
