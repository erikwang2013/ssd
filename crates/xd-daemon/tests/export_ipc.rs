// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! daemon 全链路：scan 落库 → `export.start` → `--export-worker` 子进程逐件写盘 → `export.finished`。
//! 覆盖：happy 逐字节相等、删除件被复占簇的短交付（degraded/short read）、目标目录 -32007、
//! cancel 两态竞态容忍（同 scan_ipc 的 pause 先例）。
//! 铁律（T8/T6 教训）：**每个 spawn 必带 `--db <tempdir>`**——导出的子进程按 `--db` 只读打开同一
//! 库，内存库降级时 export.start 诚实 -32603；不触真实 $HOME。
//! -32006/-32010 的**真值**归单测（xd-core::export 的纯函数注入假 statvfs/rdev）；-32006 的真环回
//! 断言归 T9/scripts（e2e-loop 挂载后导出到挂载点），本文件不造环回设备。

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::json;

/// spawn 返回：子进程 + stdin + 「响应/通知」合流通道 + stderr。
type Spawned = (
    Child,
    ChildStdin,
    Receiver<serde_json::Value>,
    std::process::ChildStderr,
);

/// 统一 spawn 底盘（同 scan_ipc：stderr=piped 供诊断留痕）。
fn spawn_daemon(args: &[&str]) -> Spawned {
    let mut child = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                let _ = tx.send(v);
            }
        }
    });
    (child, stdin, rx, stderr)
}

fn spawn_image_daemon(image: &Path, db: &Path) -> Spawned {
    spawn_daemon(&[
        "--image",
        image.to_str().unwrap(),
        "--db",
        db.to_str().unwrap(),
    ])
}

fn send(stdin: &mut ChildStdin, v: serde_json::Value) {
    writeln!(stdin, "{v}").unwrap();
    stdin.flush().unwrap();
}

fn read_response(
    rx: &Receiver<serde_json::Value>,
    id: i64,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(v) if v.get("id").and_then(|x| x.as_i64()) == Some(id) => return v,
            Ok(_) => continue, // 通知行
            Err(RecvTimeoutError::Timeout) => panic!("timeout waiting response id={id}"),
            Err(RecvTimeoutError::Disconnected) => panic!("daemon closed stdout"),
        }
    }
}

/// 等通知（同 id 的响应须先被 `read_response` 取走；导出场景里的 progress 会被跳过）。
fn wait_notification(
    rx: &Receiver<serde_json::Value>,
    method: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(v) if v.get("method").and_then(|x| x.as_str()) == Some(method) => return v,
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout) => panic!("timeout waiting notification {method}"),
            Err(RecvTimeoutError::Disconnected) => panic!("daemon closed stdout"),
        }
    }
}

/// `device.list` 的 `devices[0]` = `--image` 注册项（打开项恒在最前）。
fn first_image_device(stdin: &mut ChildStdin, rx: &Receiver<serde_json::Value>) -> String {
    send(
        stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"device.list","params":null}),
    );
    let dl = read_response(rx, 1, Duration::from_secs(10));
    let id = dl["result"]["devices"][0]["id"].as_str().unwrap();
    assert!(id.starts_with("image:"), "devices[0] 应为注册镜像：{dl}");
    id.to_string()
}

/// 快扫至 completed（返回 taskId）；导出用例统一以此落库。
fn scan_to_completed(
    stdin: &mut ChildStdin,
    rx: &Receiver<serde_json::Value>,
    dev_id: String,
) -> i64 {
    send(
        stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev_id}}),
    );
    let task_id = read_response(rx, 2, Duration::from_secs(10))["result"]["taskId"]
        .as_i64()
        .unwrap();
    let fin = wait_notification(rx, "scan.finished", Duration::from_secs(30));
    assert_eq!(fin["params"]["taskId"], task_id);
    assert_eq!(fin["params"]["state"], "completed");
    task_id
}

/// 全量条目 `(idx, name, sizeBytes)`（按 idx 升序）。导出 idxs 由它给出——测试不猜条目序。
fn collect_named_entries(
    stdin: &mut ChildStdin,
    rx: &Receiver<serde_json::Value>,
    task_id: i64,
) -> Vec<(u64, String, u64)> {
    let mut out: Vec<(u64, String, u64)> = Vec::new();
    let mut total = u64::MAX;
    while (out.len() as u64) < total {
        send(
            stdin,
            json!({"jsonrpc":"2.0","id":901,"method":"scan.results","params":{"taskId":task_id,"offset":out.len(),"limit":1000,"deletedOnly":false}}),
        );
        let rs = read_response(rx, 901, Duration::from_secs(10));
        total = rs["result"]["total"].as_u64().unwrap();
        let page = rs["result"]["entries"].as_array().unwrap();
        assert!(!page.is_empty(), "分页停滞");
        out.extend(page.iter().map(|e| {
            (
                e["idx"].as_u64().unwrap(),
                e["name"].as_str().unwrap().to_string(),
                e["sizeBytes"].as_u64().unwrap(),
            )
        }));
    }
    out.sort_unstable();
    out
}

/// cancel 用例的取数器：cancel 响应与 `export.finished` **顺序不定**（导出可能先跑完 ⇒
/// finished 先于响应到达），逐行分流收齐两条——`read_response` 会丢弃先到的通知，不能用。
fn collect_cancel_outcome(
    rx: &Receiver<serde_json::Value>,
    id: i64,
    timeout: Duration,
) -> (serde_json::Value, serde_json::Value) {
    let deadline = Instant::now() + timeout;
    let (mut resp, mut fin) = (None, None);
    while resp.is_none() || fin.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(v) if v.get("id").and_then(|x| x.as_i64()) == Some(id) => resp = Some(v),
            Ok(v) if v.get("method").and_then(|x| x.as_str()) == Some("export.finished") => {
                fin = Some(v)
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {
                panic!("等 cancel 响应/export.finished 超时（resp={resp:?} fin={fin:?}）")
            }
            Err(RecvTimeoutError::Disconnected) => panic!("daemon closed stdout"),
        }
    }
    (resp.unwrap(), fin.unwrap())
}

/// happy 夹具（计划 Step 4.1 的「exfat 两文件」）：LIVE_A.TXT 5000B 模式字节 + LIVE_B.PNG 真 PNG。
/// 返回 `(镜像, [(落盘名, 原始字节)])`——逐字节比对以它为准。
fn two_file_image_bytes() -> (Vec<u8>, Vec<(&'static str, Vec<u8>)>) {
    let txt: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    let png = xd_fixtures::mini_png(b"export-me");
    let img = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "LIVE_A.TXT", &txt)
        .add_file("/", "LIVE_B.PNG", &png)
        .build();
    (img, vec![("LIVE_A.TXT", txt), ("LIVE_B.PNG", png)])
}

/// 短交付夹具：GONE.BIN 9000B（簇 6,7,8）→ 删除 → TAKER.BIN **复占 [7,8,12]**（位图重分配）。
/// 删除件读取以位图为分配权威：跑到首个已占簇（7）即停 ⇒ 交付恰 1 簇 = 4096B < 9000B ⇒
/// `eof` ⇒ degraded/"short read"。若位图被忽略（连读 6,7,8）则交付 9000B、degraded 归零——牙在此。
fn reused_cluster_image_bytes() -> Vec<u8> {
    xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "GONE.BIN", &[7u8; 9000])
        .delete("/", "GONE.BIN")
        .add_file_in_clusters("/", "TAKER.BIN", &[5u8; 9000], &[7, 8, 12], false)
        .build()
}

/// cancel 夹具：30 × 32KiB = 960KiB（240 簇，1MiB 卷内）——子进程启动 + 逐件 create/write
/// 撑出可观窗口；cancel 紧随 start 发出。
fn bulk_image_bytes() -> Vec<u8> {
    let mut b = xd_fixtures::ExfatImageBuilder::new();
    for i in 0..30u32 {
        b.add_file("/", &format!("BIG_{i:03}.BIN"), &vec![i as u8; 32 * 1024]);
    }
    b.build()
}

/// 1）happy：全条目导出 = 逐字节相等 + 原名落盘 + 终报计数（父侧 estimatedBytes 口径一并钉住）。
#[test]
fn exports_all_bytes_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("vol.img");
    let (image, files) = two_file_image_bytes();
    std::fs::write(&img_path, &image).unwrap();
    let db = dir.path().join("t.db");
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();

    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &db);
    let dev_id = first_image_device(&mut stdin, &rx);
    let task_id = scan_to_completed(&mut stdin, &rx, dev_id);
    let entries = collect_named_entries(&mut stdin, &rx, task_id);
    assert_eq!(entries.len(), 2, "夹具两条目：{entries:?}");
    let idxs: Vec<u64> = entries.iter().map(|e| e.0).collect();
    let want_bytes: u64 = files.iter().map(|(_, d)| d.len() as u64).sum();

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"export.start",
               "params":{"taskId":task_id,"idxs":idxs,"targetDir":out.to_str().unwrap()}}),
    );
    let st = read_response(&rx, 3, Duration::from_secs(10));
    assert_eq!(st["result"]["fileCount"], 2, "{st}");
    assert_eq!(
        st["result"]["estimatedBytes"].as_u64(),
        Some(want_bytes),
        "estimated = Σ sizeBytes：{st}"
    );
    assert!(st["result"]["exportId"].is_u64(), "{st}");

    let exf = wait_notification(&rx, "export.finished", Duration::from_secs(30));
    let p = &exf["params"];
    assert_eq!(p["exportId"], st["result"]["exportId"]);
    assert_eq!(p["succeeded"], 2, "{exf}");
    assert_eq!(p["degraded"], 0, "{exf}");
    assert_eq!(p["failed"], 0, "{exf}");
    assert_eq!(p["canceled"], false, "{exf}");
    assert_eq!(p["targetDir"], out.to_str().unwrap(), "{exf}");
    assert_eq!(
        p["items"].as_array().unwrap().len(),
        0,
        "items 仅含降级/失败条目（契约 v1.2）：{exf}"
    );
    assert_eq!(p["itemsTruncated"], false, "{exf}");

    for (name, want) in &files {
        let got = std::fs::read(out.join(name)).unwrap_or_else(|e| panic!("缺 {name}：{e}"));
        assert_eq!(&got, want, "逐字节相等：{name}");
    }
    // 目录里恰两件（无半成品/临时件残留）
    let mut names: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["LIVE_A.TXT", "LIVE_B.PNG"]);

    drop(stdin);
    let _ = child.wait();
}

/// 2）短交付：删除件被复占簇 → degraded + reason=="short read" + 落盘长 == 实交付长（4096）；
/// 同批的 live 件照常整件成功（单件降级不拖垮批次）。
#[test]
fn degraded_reported_for_damaged_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("vol.img");
    std::fs::write(&img_path, reused_cluster_image_bytes()).unwrap();
    let db = dir.path().join("t.db");
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();

    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &db);
    let dev_id = first_image_device(&mut stdin, &rx);
    let task_id = scan_to_completed(&mut stdin, &rx, dev_id);
    let entries = collect_named_entries(&mut stdin, &rx, task_id);
    let gone = entries
        .iter()
        .find(|e| e.1 == "GONE.BIN")
        .expect("删除件在场");
    assert_eq!(gone.2, 9000, "删除件 sizeBytes：{entries:?}");
    let idxs: Vec<u64> = entries.iter().map(|e| e.0).collect();

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"export.start",
               "params":{"taskId":task_id,"idxs":idxs,"targetDir":out.to_str().unwrap()}}),
    );
    let st = read_response(&rx, 3, Duration::from_secs(10));
    assert!(st.get("error").is_none(), "{st}");

    let exf = wait_notification(&rx, "export.finished", Duration::from_secs(30));
    let p = &exf["params"];
    assert_eq!(p["degraded"], 1, "{exf}");
    assert_eq!(p["succeeded"], 1, "{exf}");
    assert_eq!(p["failed"], 0, "{exf}");
    let item = &p["items"][0];
    assert_eq!(item["name"], "GONE.BIN", "{exf}");
    assert_eq!(item["status"], "degraded", "{exf}");
    assert_eq!(item["reason"], "short read", "{exf}");
    assert_eq!(item["idx"].as_u64(), Some(gone.0), "条目 idx 透传：{exf}");

    let got = std::fs::read(out.join("GONE.BIN")).unwrap();
    assert_eq!(
        got.len(),
        4096,
        "落盘长 == 实交付长（首个已占簇前的一簇）；若忽略位图会连读 6,7,8 = 9000B"
    );
    assert_eq!(got, vec![7u8; 4096], "交付字节来自未复占的首簇");
    assert_eq!(
        std::fs::read(out.join("TAKER.BIN")).unwrap(),
        vec![5u8; 9000],
        "同批 live 件（链式 [7,8,12]）整件导出"
    );

    drop(stdin);
    let _ = child.wait();
}

/// 3）目标校验（集成面）：不存在目录 → -32007 且错误消息逐字（契约 v1.2）。
/// -32006/-32010 的真值由 xd-core::export 的纯函数单测注入假 rdev/statvfs 覆盖（计划 Step 4.3）。
#[test]
fn target_dir_missing_maps_to_minus_32007() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("vol.img");
    std::fs::write(&img_path, two_file_image_bytes().0).unwrap();
    let db = dir.path().join("t.db");
    let missing = dir.path().join("nope");

    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &db);
    let dev_id = first_image_device(&mut stdin, &rx);
    let task_id = scan_to_completed(&mut stdin, &rx, dev_id);

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"export.start",
               "params":{"taskId":task_id,"idxs":[0],"targetDir":missing.to_str().unwrap()}}),
    );
    let r = read_response(&rx, 3, Duration::from_secs(10));
    assert_eq!(r["error"]["code"], -32007, "{r}");
    assert_eq!(
        r["error"]["message"],
        format!("Target not writable: {}", missing.display()),
        "{r}"
    );
    // 校验在 spawn 之前：目录仍不存在（无子进程残留半成品）
    assert!(!missing.exists());

    drop(stdin);
    let _ = child.wait();
}

/// 4）cancel：紧随 start 发出 → 两态竞态容忍（同 scan_ipc 的 pause 先例）：运行中 → canceled +
/// 终报 canceled==true（计数自洽）；恰已跑完 → 幂等 "completed" + 终报 canceled==false 全成功。
/// 之后 ping 仍通 = 取消不卡死 jobs 锁/转发线程。
#[test]
fn cancel_stops_export_two_state_tolerance() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("bulk.img");
    std::fs::write(&img_path, bulk_image_bytes()).unwrap();
    let db = dir.path().join("t.db");
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();

    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &db);
    let dev_id = first_image_device(&mut stdin, &rx);
    let task_id = scan_to_completed(&mut stdin, &rx, dev_id);
    let entries = collect_named_entries(&mut stdin, &rx, task_id);
    assert_eq!(entries.len(), 30, "夹具条目数");
    let total = entries.len() as u64;
    let idxs: Vec<u64> = entries.iter().map(|e| e.0).collect();

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":5,"method":"export.start",
               "params":{"taskId":task_id,"idxs":idxs,"targetDir":out.to_str().unwrap()}}),
    );
    let st = read_response(&rx, 5, Duration::from_secs(10));
    let export_id = st["result"]["exportId"].as_u64().unwrap();
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":6,"method":"export.cancel","params":{"exportId":export_id}}),
    );
    let (c, exf) = collect_cancel_outcome(&rx, 6, Duration::from_secs(30));
    let state = c["result"]["state"].as_str().unwrap_or_else(|| {
        panic!("cancel 须 ok（未知 id 才是 -32602）：{c}");
    });
    let p = &exf["params"];
    assert_eq!(p["exportId"].as_u64(), Some(export_id), "{exf}");
    let (succ, deg, fail) = (
        p["succeeded"].as_u64().unwrap(),
        p["degraded"].as_u64().unwrap(),
        p["failed"].as_u64().unwrap(),
    );
    match state {
        "canceled" => {
            assert_eq!(p["canceled"], true, "运行中取消 ⇒ 终报必带 canceled：{exf}");
            assert_eq!(
                succ + deg + fail,
                total,
                "终报计数须自洽（未跑完计 failed）：{exf}"
            );
        }
        "completed" => {
            // 竞态另一态：已跑完（cancel 落在终态之后）→ 幂等原样返回终态
            assert_eq!(p["canceled"], false, "{exf}");
            assert_eq!(succ, total, "跑到终态 = 全部成功：{exf}");
        }
        other => panic!("cancel 状态只能 canceled/completed：{other}"),
    }

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":7,"method":"ping","params":null}),
    );
    assert_eq!(
        read_response(&rx, 7, Duration::from_secs(10))["result"]["pong"],
        true,
        "取消后 daemon 仍服务（转发线程/jobs 锁无卡死）"
    );
    drop(stdin);
    let _ = child.wait();
}
