// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! daemon 全链路：scan 落库 → `export.start` → `--export-worker` 子进程逐件写盘 → `export.finished`。
//! 覆盖：happy 逐字节相等、删除件被复占簇的短交付（degraded/short read）、目标目录 -32007、
//! cancel 两态竞态容忍（同 scan_ipc 的 pause 先例）+ 取消须真终止（qual I1）、
//! unknown-ext 雕刻件 failed + 无残骸（qual I3）、父死后子 EPIPE 静默退出（qual I4）、
//! 伪造库行的落盘名净化/穿越拦截（qual 硬化 (a)）。
//! 铁律（T8/T6 教训）：**每个 spawn 必带 `--db <tempdir>`**——导出的子进程按 `--db` 只读打开同一
//! 库，内存库降级时 export.start 诚实 -32603；不触真实 $HOME。
//! -32006/-32010 的**真值**归单测（xd-core::export 的纯函数注入假 statvfs/rdev）；-32006 的真环回
//! 断言归 T9/scripts（e2e-loop 挂载后导出到挂载点），本文件不造环回设备。

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use xd_core::api::ScanEntry;
use xd_core::store::Store;

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
/// 响应与 `scan.finished` 竞序（1MiB 卷毫秒级扫完）⇒ 走顺序容忍取数器，不得用 `read_response`
/// 先吞响应（否则先到的通知被丢，30s 后超时红——qual 实测命中）。
fn scan_to_completed(
    stdin: &mut ChildStdin,
    rx: &Receiver<serde_json::Value>,
    dev_id: String,
) -> i64 {
    send(
        stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev_id}}),
    );
    let (st, fin) =
        collect_response_and_notification(rx, 2, "scan.finished", Duration::from_secs(30));
    let task_id = st["result"]["taskId"].as_i64().unwrap();
    let fin = fin.expect("扫描已起 ⇒ 必有 scan.finished");
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

/// 「响应 + 后续通知」的**顺序容忍**取数器：`start` 类请求在返回响应前就起了后台线程，小任务
/// （1MiB 卷的扫描、1 件导出——毫秒级）可在响应行之前跑完 ⇒ 通知先到。逐行分流收齐两条；
/// `read_response`/`wait_notification` 会把先到的那条当噪声丢掉，凡「既要响应又要通知」的
/// 用例都不能用（qual 探针：150 次全量跑命中 6 次抢跑——5×scan.finished、1×export.finished；
/// 命中即触发「先到者被丢 → 30s 超时红」，修前实测 2/130 真红）。
/// 响应带 `error`（任务未起）时不再等通知，返回 `(resp, None)`——调用方先断言错误码。
fn collect_response_and_notification(
    rx: &Receiver<serde_json::Value>,
    id: i64,
    method: &str,
    timeout: Duration,
) -> (serde_json::Value, Option<serde_json::Value>) {
    let deadline = Instant::now() + timeout;
    let (mut resp, mut fin): (Option<serde_json::Value>, Option<serde_json::Value>) = (None, None);
    while !(resp.is_some() && fin.is_some()) {
        if resp.as_ref().is_some_and(|r| r.get("error").is_some()) {
            break; // 出错 ⇒ 不会有终报（任务未起）
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(v) if v.get("id").and_then(|x| x.as_i64()) == Some(id) => resp = Some(v),
            Ok(v) if v.get("method").and_then(|x| x.as_str()) == Some(method) => fin = Some(v),
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {
                panic!("等响应 id={id}/{method} 超时（resp={resp:?} fin={fin:?}）")
            }
            Err(RecvTimeoutError::Disconnected) => panic!("daemon closed stdout"),
        }
    }
    (resp.unwrap(), fin)
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
    let (st, exf) =
        collect_response_and_notification(&rx, 3, "export.finished", Duration::from_secs(30));
    assert_eq!(st["result"]["fileCount"], 2, "{st}");
    assert_eq!(
        st["result"]["estimatedBytes"].as_u64(),
        Some(want_bytes),
        "estimated = Σ sizeBytes：{st}"
    );
    assert!(st["result"]["exportId"].is_u64(), "{st}");
    let exf = exf.expect("导出已起 ⇒ 必有终报");
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
    let (st, exf) =
        collect_response_and_notification(&rx, 3, "export.finished", Duration::from_secs(30));
    assert!(st.get("error").is_none(), "{st}");
    let exf = exf.expect("导出已起 ⇒ 必有终报");
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
    let (c, exf) =
        collect_response_and_notification(&rx, 6, "export.finished", Duration::from_secs(30));
    let state = c["result"]["state"].as_str().unwrap_or_else(|| {
        panic!("cancel 须 ok（未知 id 才是 -32602）：{c}");
    });
    let exf = exf.expect("cancel 命中运行中态 ⇒ 必有终报");
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
            // 真终止断言（qual I1）：cancel 紧随 start（μs 级）发出，子进程还在 exec/开库/盘上写，
            // 30 件不可能已跑完 ⇒ 必有未跑完的件。残窗：worker 恰在「写完最后一件」与「落定 state」
            // 之间被取消（µs），此时计数可满——qual 探针 30 次未见此态，故不为此加松弛。
            assert!(
                succ + deg < total,
                "取消须实际终止子进程（qual 探针 0/30 全计数；SIGTERM 未生效？）：{exf}"
            );
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

// ===== qual 硬化（本轮）：failed 件 E2E / 库行注入 / EPIPE 转正 =====

/// 伪造库行：经 `Store::insert_entries`（本仓自己的写入 API，同表同列）直插——等价「sqlite 直插」，
/// 模拟调用者/外部工具/未来迁移写出引擎不会产出之形。骨架字段按需覆盖。
fn forged(idx: u64, name: &str, ext: &str, size: u64, quality: &str) -> ScanEntry {
    ScanEntry {
        idx,
        name: name.into(),
        path: "/".into(),
        ext: ext.into(),
        size_bytes: size,
        deleted: false,
        is_dir: false,
        quality: quality.into(),
        first_cluster: 0,
        byte_offset: None,
        contiguous: None,
    }
}

fn forge_entries(db: &Path, task_id: i64, rows: &[ScanEntry]) {
    Store::open(db)
        .unwrap()
        .insert_entries(task_id as u64, rows)
        .unwrap();
}

/// 1MiB exfat 夹具里簇 `c` 的起始字节偏移 = HEAP_OFFSET(32 扇区)×512 + (c−2)×4096
/// （xd-fixtures/src/exfat.rs 的常量）。两文件夹具的 LIVE_A/B 占簇 6,7,8 ⇒ 簇 100 必空闲。
fn cluster_offset(c: u32) -> u64 {
    32 * 512 + u64::from(c - 2) * 4096
}

/// 目录内文件名（已排序）。
fn dir_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// 本测试的 export worker 在场与否：直读 **daemon 的子进程表**
/// （`/proc/<dpid>/task/*/children`——daemon 只 spawn worker 这一个子进程，故 pid 无歧义），
/// 再复核 cmdline 含 `--export-worker`（父刚 fork 未 exec 时是父的 cmdline，跳过）；
/// **zombie（已跑完待收尸）不算在场**（vacuous-pass 封口：拿不到在跑的子就该红）。
///
/// 为何不用 `/proc` 全扫 + cmdline 匹配（T3 原实现）：release 下 30×32KiB 全量导出仅 ~4ms
/// （子进程寿命与之同级），而全扫一次 ~3ms（本机实测，见 T9 报告）⇒ 采样粒度与寿命同量级，
/// 观测纯靠运气：本机 release 单跑 5 次漏配 4 次（parent b93267a 同样红 = 前存缺陷，非 T9 回归）。
/// 快路径单次 ~0.1ms ⇒ 同一寿命内可采数十次；CI（更慢）余量更大。
fn find_export_worker(dpid: u32) -> Option<i32> {
    for t in std::fs::read_dir(format!("/proc/{dpid}/task"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(children) = std::fs::read_to_string(t.path().join("children")) else {
            continue;
        };
        for pid in children
            .split_whitespace()
            .filter_map(|x| x.parse::<i32>().ok())
        {
            let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
                continue; // 已退出（竞态）
            };
            if String::from_utf8_lossy(&cmd).contains("--export-worker") && !worker_exited(pid) {
                return Some(pid);
            }
        }
    }
    None
}

fn wait_export_worker(dpid: u32, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(pid) = find_export_worker(dpid) {
            return Some(pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// 子进程已退出证据：`/proc/<pid>` 消失（已收尸），或 state == 'Z'（已退出待收尸）。
fn worker_exited(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        // comm 可能含 ')'：取最后一个 ')' 之后才是 state 字段
        Ok(s) => s
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
    }
}

/// 5）雕刻件失败通路 + 半成品清理（qual I3）：直插未知 ext 的雕刻行 → `create_new` 已建文件、
/// 读取必败（from_ext 不认 "xyz"）⇒ failed 逐字 + **目标目录无残骸**。
/// 牙：删 `export_one` 里的 `cleanup` → 空文件 `carved_000042.xyz` 留盘 → 本测红。
#[test]
fn carved_unknown_ext_reports_failed_and_leaves_no_residue() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("vol.img");
    std::fs::write(&img_path, two_file_image_bytes().0).unwrap();
    let db = dir.path().join("t.db");
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();

    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &db);
    let dev_id = first_image_device(&mut stdin, &rx);
    let task_id = scan_to_completed(&mut stdin, &rx, dev_id);

    let mut row = forged(42, "", "xyz", 4096, "carved");
    row.byte_offset = Some(cluster_offset(100));
    forge_entries(&db, task_id, &[row]);

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"export.start",
               "params":{"taskId":task_id,"idxs":[42],"targetDir":out.to_str().unwrap()}}),
    );
    let (st, exf) =
        collect_response_and_notification(&rx, 3, "export.finished", Duration::from_secs(30));
    assert!(
        st.get("error").is_none(),
        "伪造行须过父侧校验（条目在场即可）：{st}"
    );
    assert_eq!(st["result"]["fileCount"], 1, "{st}");
    assert_eq!(st["result"]["estimatedBytes"], 4096, "{st}");
    let exf = exf.expect("导出已起 ⇒ 必有终报");
    let p = &exf["params"];
    assert_eq!(p["succeeded"], 0, "{exf}");
    assert_eq!(p["degraded"], 0, "{exf}");
    assert_eq!(p["failed"], 1, "{exf}");
    let item = &p["items"][0];
    assert_eq!(item["idx"], 42, "{exf}");
    assert_eq!(item["name"], "carved_000042.xyz", "{exf}");
    assert_eq!(item["status"], "failed", "{exf}");
    assert!(
        item["reason"]
            .as_str()
            .unwrap()
            .contains("unknown carved ext"),
        "reason 须言明坏 ext：{exf}"
    );
    let left = dir_names(&out);
    assert!(
        left.is_empty(),
        "失败件不得留半成品残骸（cleanup 必删）：{left:?}"
    );

    drop(stdin);
    let _ = child.wait();
}

/// 6）库行注入（qual 硬化 (a)）：伪造活条目名带 `../`、雕刻件 ext 带 `../`——落盘名一律过
/// sanitize ⇒ ① 无文件落到目标目录之外；② 报告名不含 `/`、`..`；③ 坏 ext 件 failed 且无残骸。
/// 牙：去掉 name 的 sanitize → `escaped.bin` 真出现在目标目录**之外** → 红；
/// 去掉 ext 的 sanitize → 报告名带 `/`+`..`（且 create 抛 ENOENT）→ 红。
#[test]
fn hostile_row_names_and_ext_cannot_escape_target_dir() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("vol.img");
    std::fs::write(&img_path, two_file_image_bytes().0).unwrap();
    let db = dir.path().join("t.db");
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();

    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &db);
    let dev_id = first_image_device(&mut stdin, &rx);
    let task_id = scan_to_completed(&mut stdin, &rx, dev_id);

    // ① 活条目：名 "../escaped.bin"，读簇 6（LIVE_A.TXT 首 4096B）必成功 ⇒ 净化不做则真越界落物
    let mut live = forged(42, "../escaped.bin", "bin", 4096, "complete");
    live.first_cluster = 6;
    live.contiguous = Some(true);
    // ② 雕刻条目：ext "../evil"（名字虽净化，ext 不净化即带穿越串）
    let mut evil = forged(43, "", "../evil", 4096, "carved");
    evil.byte_offset = Some(cluster_offset(100));
    forge_entries(&db, task_id, &[live, evil]);

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"export.start",
               "params":{"taskId":task_id,"idxs":[42,43],"targetDir":out.to_str().unwrap()}}),
    );
    let (st, exf) =
        collect_response_and_notification(&rx, 3, "export.finished", Duration::from_secs(30));
    assert!(st.get("error").is_none(), "{st}");
    let exf = exf.expect("导出已起 ⇒ 必有终报");
    let p = &exf["params"];
    assert_eq!(p["succeeded"], 1, "活条目按簇读出整件成功：{exf}");
    assert_eq!(p["degraded"], 0, "{exf}");
    assert_eq!(p["failed"], 1, "坏 ext 件失败：{exf}");
    let item = &p["items"][0];
    assert_eq!(item["idx"], 43, "{exf}");
    assert_eq!(item["status"], "failed", "{exf}");
    let n = item["name"].as_str().unwrap();
    assert!(
        n.starts_with("carved_") && !n.contains('/') && !n.contains(".."),
        "ext 必须过 sanitize：{n}"
    );

    // 越界拦截（名净化的牙）：目标目录之外不得有落物
    assert!(
        !dir.path().join("escaped.bin").exists(),
        "落盘名未净化：文件真逃逸出了目标目录"
    );
    let names = dir_names(&out);
    assert_eq!(
        names.len(),
        1,
        "目标目录恰一件（坏 ext 件已清残骸）：{names:?}"
    );
    assert!(!names[0].contains(".."), "落盘名须安全：{names:?}");
    let want: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(
        std::fs::read(out.join(&names[0])).unwrap(),
        want,
        "伪造活行读到簇 6 首 4096B（LIVE_A.TXT 模式字节），且落在目标目录内"
    );

    drop(stdin);
    let _ = child.wait();
}

/// 7）EPIPE 转正（qual I4）：父（daemon）被 SIGKILL → 子的 stdout 断开 → 子**静默退出**
/// （run_inner 的 writeln 失败臂 `return Ok(())`），stderr 无 panic 留痕——子 stderr 继承 daemon
/// 的 stderr（= 本测试管道），父死后仍可读全。
/// 牙：EPIPE 臂改成 unwrap/panic → 子 panic 落 stderr → 红。
#[test]
fn epipe_worker_exits_silently_when_parent_dies() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("bulk.img");
    std::fs::write(&img_path, bulk_image_bytes()).unwrap();
    let db = dir.path().join("t.db");
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();

    let (mut child, mut stdin, rx, stderr) = spawn_image_daemon(&img_path, &db);
    // 持续排空 stderr（防管道满阻塞子进程），事后断言无 panic
    let tail = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&tail);
    let drain = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            sink.lock().unwrap().push_str(&line);
            sink.lock().unwrap().push('\n');
        }
    });

    let dev_id = first_image_device(&mut stdin, &rx);
    let task_id = scan_to_completed(&mut stdin, &rx, dev_id);
    let entries = collect_named_entries(&mut stdin, &rx, task_id);
    let idxs: Vec<u64> = entries.iter().map(|e| e.0).collect();
    assert_eq!(idxs.len(), 30, "夹具条目数");

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"export.start",
               "params":{"taskId":task_id,"idxs":idxs,"targetDir":out.to_str().unwrap()}}),
    );
    let st = read_response(&rx, 3, Duration::from_secs(10));
    assert!(st.get("error").is_none(), "{st}");

    // 子此刻必在场：响应写出（t≈1.5ms）远早于子退出（t≈5.5ms，本机 release 实测），
    // 且采样走子进程表快路径（~0.1ms/次，见 find_export_worker）。
    let worker = wait_export_worker(child.id(), Duration::from_secs(2))
        .expect("worker 应在场（响应先于子进程结束）");
    // 杀父：子的 stdout 读端关闭 ⇒ 下一次 writeln 必 EPIPE
    child.kill().unwrap();
    let _ = child.wait();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !worker_exited(worker) {
        assert!(
            Instant::now() < deadline,
            "worker pid={worker} 未随父退出（EPIPE 臂失效？）"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(stdin);
    drain.join().unwrap(); // 写端全关（daemon 死 + 子退出）⇒ 管道 EOF
    let log = tail.lock().unwrap().clone();
    assert!(
        !log.contains("panicked"),
        "EPIPE 路径不得 panic（子 stderr 留痕）：{log}"
    );

    let _ = child.wait();
}
