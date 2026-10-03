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

/// golden 集合收口：新增/删除契约文件必须同步改测试（36 个 = v1 冻结 21 + M1c 雕刻页 1 +
/// M1c -32005 错误页 1 + M1d v1.2 增量 13）。Dart 侧同款集合测试见
/// `ui/test/protocol_v1_test.dart`——改一侧必红，两侧同步。
#[test]
fn golden_set_is_exactly_the_36_contract_files() {
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
        "error_entry_not_found.response.json",
        "error_entry_too_large.response.json",
        "error_insufficient_space.response.json",
        "error_target_not_writable.response.json",
        "error_target_on_source.response.json",
        "error_task_not_active.response.json",
        "error_unallocated_unavailable.response.json",
        "error_unsupported_fs.response.json",
        "export_cancel.request.json",
        "export_cancel.response.json",
        "export_finished.notification.json",
        "export_progress.notification.json",
        "export_start.request.json",
        "export_start.response.json",
        "fs_read.request.json",
        "fs_read.response.json",
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
        "scan_results_carved.response.json",
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
fn scan_results_carved_golden_matches_scan_entry() {
    // v1.1（M1c）：雕刻条目——无名字/无簇号，byteOffset 为未分配空间内坐标。
    let (_, result) = ok_result("scan_results_carved.response.json");
    assert_eq!(result["total"], serde_json::json!(2));
    let entries: Vec<ScanEntry> = serde_json::from_value(result["entries"].clone()).unwrap();
    assert_eq!(entries.len(), 2);
    for (e, off) in entries.iter().zip([835584u64, 892928]) {
        assert_eq!(e.byte_offset, Some(off));
        assert_eq!(e.quality, "carved");
        assert!(e.name.is_empty() && e.path.is_empty());
        assert!(e.deleted && !e.is_dir);
        assert_eq!(e.first_cluster, 0);
    }
    assert_eq!(entries[0].ext, "jpg");
    assert_eq!(entries[1].ext, "png");
    assert_eq!(serde_json::to_value(&entries).unwrap(), result["entries"]);
    // 缺省=null（FS 条目）：序列化省略 byteOffset 键，旧 golden/旧客户端不受影响。
    let mut plain = entries[0].clone();
    plain.byte_offset = None;
    let v = serde_json::to_value(&plain).unwrap();
    assert!(v.get("byteOffset").is_none(), "{v}");
    assert_eq!(
        serde_json::from_value::<ScanEntry>(v).unwrap().byte_offset,
        None
    );
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
        (
            "error_unallocated_unavailable.response.json",
            12,
            RpcError::unallocated_unavailable(),
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

    // v1.2 五码：参数化反钉（qual-m1d-t1）——调用值刻意异于 golden 值，
    // 构造器若退化为硬编码 golden 值，此处必红。
    assert_eq!(
        RpcError::target_on_source("/media/stick/Recovered"),
        RpcError {
            code: -32006,
            message: "Target is on the source device: /media/stick/Recovered".into(),
        }
    );
    assert_eq!(
        RpcError::target_not_writable("/tmp/ro"),
        RpcError {
            code: -32007,
            message: "Target not writable: /tmp/ro".into(),
        }
    );
    assert_eq!(
        RpcError::entry_not_found(7),
        RpcError {
            code: -32008,
            message: "Entry not found: 7".into(),
        }
    );
    assert_eq!(
        RpcError::entry_too_large(67108865), // 64MiB + 1：README「预览上限 64MiB」语义边界
        RpcError {
            code: -32009,
            message: "Entry too large: 67108865".into(),
        }
    );
    assert_eq!(
        RpcError::insufficient_space(999),
        RpcError {
            code: -32010,
            message: "Insufficient space on target: need 999 bytes".into(),
        }
    );
}

#[test]
fn v12_request_goldens_typed_params() {
    // 契约 v1.2（M1d）：请求 envelope 全等 + params 强类型逐字段。
    let fr = request_golden("fs_read.request.json", 21, "fs.read");
    let p: FsReadParams = serde_json::from_value(fr.params.clone().unwrap()).unwrap();
    assert_eq!(p.task_id, 1);
    assert_eq!(p.idx, 0);
    assert_eq!(p.offset, 0);
    assert_eq!(p.length, 16);

    let es = request_golden("export_start.request.json", 22, "export.start");
    let p: ExportStartParams = serde_json::from_value(es.params.clone().unwrap()).unwrap();
    assert_eq!(p.task_id, 1);
    assert_eq!(p.idxs, vec![0, 1]);
    assert_eq!(p.target_dir, "/home/user/Recovered");

    let ec = request_golden("export_cancel.request.json", 23, "export.cancel");
    let p: ExportIdParams = serde_json::from_value(ec.params.clone().unwrap()).unwrap();
    assert_eq!(p.export_id, 1);

    // 强类型字段名即契约 camelCase：缺键/改名必 decode 失败（反向钉死）。
    assert!(
        serde_json::from_value::<FsReadParams>(serde_json::json!({
            "task_id": 1, "idx": 0, "offset": 0, "length": 16
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ExportStartParams>(serde_json::json!({
            "taskId": 1, "idxs": [0], "target_dir": "/x"
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<ExportIdParams>(serde_json::json!({"export_id": 1})).is_err());
    // 负值/别名反钉（qual-m1d-t1）：u64 必须拒绝负数；未知别名不得解码。
    assert!(
        serde_json::from_value::<ExportStartParams>(serde_json::json!({
            "taskId": 1, "idxs": [-1], "targetDir": "/x"
        }))
        .is_err(),
        "negative idx must be rejected"
    );
    assert!(
        serde_json::from_value::<ExportIdParams>(serde_json::json!({ "eid": 1 })).is_err(),
        "unknown alias must not decode"
    );
}

#[test]
fn fs_read_response_matches_golden() {
    // bytesBase64 = "hello, xiaodun!"（15 字节）的 base64；eof=true（短交付即条目尾）。
    let (_, result) = ok_result("fs_read.response.json");
    assert_eq!(
        result["bytesBase64"],
        serde_json::json!("aGVsbG8sIHhpYW9kdW4h")
    );
    assert_eq!(result["eof"], serde_json::json!(true));
}

#[test]
fn export_start_and_cancel_responses_match_golden() {
    let (_, result) = ok_result("export_start.response.json");
    assert_eq!(result["exportId"], serde_json::json!(1));
    assert_eq!(result["fileCount"], serde_json::json!(2));
    assert_eq!(result["estimatedBytes"], serde_json::json!(16007));

    // state ∈ "canceled" | "completed"（终态幂等原样返回）；非 ScanState 枚举，逐字钉死。
    let (_, result) = ok_result("export_cancel.response.json");
    assert_eq!(result["exportId"], serde_json::json!(1));
    assert_eq!(result["state"], serde_json::json!("canceled"));
}

#[test]
fn export_progress_notification_matches_golden() {
    let n = golden("export_progress.notification.json");
    assert_eq!(n["jsonrpc"], serde_json::json!("2.0"));
    assert!(n.get("id").is_none(), "通知不得携带 id");
    assert_eq!(n["method"], serde_json::json!("export.progress"));
    let p = &n["params"];
    assert_eq!(p["exportId"], serde_json::json!(1));
    assert_eq!(p["done"], serde_json::json!(1));
    assert_eq!(p["total"], serde_json::json!(2));
    assert_eq!(p["writtenBytes"], serde_json::json!(12000));
    assert_eq!(p["elapsedMs"], serde_json::json!(300));
    assert_eq!(
        xd_core::notify::notification("export.progress", n["params"].clone()),
        n
    );
}

#[test]
fn export_finished_notification_matches_golden() {
    let n = golden("export_finished.notification.json");
    assert_eq!(n["jsonrpc"], serde_json::json!("2.0"));
    assert!(n.get("id").is_none(), "通知不得携带 id");
    assert_eq!(n["method"], serde_json::json!("export.finished"));
    let p = &n["params"];
    assert_eq!(p["exportId"], serde_json::json!(1));
    assert_eq!(p["succeeded"], serde_json::json!(1));
    assert_eq!(p["degraded"], serde_json::json!(1));
    assert_eq!(p["failed"], serde_json::json!(0));
    assert_eq!(p["canceled"], serde_json::json!(false));
    assert_eq!(p["targetDir"], serde_json::json!("/home/user/Recovered"));
    assert_eq!(p["itemsTruncated"], serde_json::json!(false));
    // items 仅降级/失败（≤1000），逐字段：idx/name/status/reason。
    let items = p["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["idx"], serde_json::json!(0));
    assert_eq!(items[0]["name"], serde_json::json!("IMG_0001.JPG"));
    assert_eq!(items[0]["status"], serde_json::json!("degraded"));
    assert_eq!(items[0]["reason"], serde_json::json!("short read"));
    assert_eq!(
        xd_core::notify::notification("export.finished", n["params"].clone()),
        n
    );
}

#[test]
fn v12_error_responses_match_golden() {
    // 五个 v1.2 错误码：构造器文案与 golden 逐字（message 即契约）。
    for (name, id, error) in [
        (
            "error_target_on_source.response.json",
            22,
            RpcError::target_on_source("/mnt/usb/Recovered"),
        ),
        (
            "error_target_not_writable.response.json",
            22,
            RpcError::target_not_writable("/root/nope"),
        ),
        (
            "error_entry_not_found.response.json",
            21,
            RpcError::entry_not_found(999),
        ),
        (
            "error_entry_too_large.response.json",
            21,
            RpcError::entry_too_large(1073741824),
        ),
        (
            "error_insufficient_space.response.json",
            22,
            RpcError::insufficient_space(16007),
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
