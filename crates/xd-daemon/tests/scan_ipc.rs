// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! daemon 全链路：scan.start → 通知 → 分页/过滤 → 重启持久 → image: 拒绝。
//! T6 评审移交增补全数落地：1) 每个 spawn 必带 `--db <tempdir>` 或注入 `XDG_STATE_HOME`
//! （绝不落真实 $HOME——T6 的 ipc.rs 踩过此坑）；2) cancel 场景 stderr 捕获，断言无 `panicked`
//! 且 finished=canceled（钉死 ScanCanceled hook 静默 + 取消真发生）；3) `--db` 指向目录的降级
//! 存活；4) SIGKILL 中断 → failed（paused 保留）；5) EACCES → -32001；7) XDG 优先于 HOME。
//! 6) 的裁定：progress 发射分支（250ms 节流）不做 CI 钉死——只做 `elapsedMs` 语义 + 数量 ≤ 条目数
//! 的弱判别（真机手测清单另记）；大介质难入 CI。
//! 慢镜像（[`slow_fat_image_bytes`]，513 条目的 FAT16）是 IPC 层撑开「扫描中」窗口的唯一手段
//! （T5 的 SlowDev 是进程内 testutil，IPC 层不可用）。**窗口量级（T7 阶段三后）**：落库
//! 8288-10265 条/秒（WAL+NORMAL，spec-t7 量化，见 `store.rs` 头注）⇒ 513 条目 ≈ 46-62ms，
//! 仍 ≫ 一次 IPC 往返（亚毫秒）——cancel/SIGKILL 的时序裕度 ~100×。旧注释曾记「每条目 fsync ~3.5s」，那是
//! T7 前 DELETE+FULL 的账，已随选型 b 作废。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::json;

/// spawn 返回：子进程 + stdin + 「响应/通知」合流通道 + stderr 捕获口。
type Spawned = (Child, ChildStdin, Receiver<serde_json::Value>, ChildStderr);

/// 统一 spawn 底盘：`stderr=piped`（增补 2 改自计划的 `Stdio::null()`——cancel/降级场景要捕获
/// 断言；不用 stderr 的测试不读亦无害，daemon 的 stderr 输出仅 banner 级）。
fn spawn_daemon(args: &[&str], envs: &[(&str, &str)]) -> Spawned {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xd-daemon"));
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
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

/// 既定用法（增补 1）：镜像注册 + 显式 `--db`，不触真实 HOME。
fn spawn_image_daemon(image: &Path, db: &Path) -> Spawned {
    spawn_daemon(
        &[
            "--image",
            image.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ],
        &[],
    )
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

/// `device.list` 的 `devices[0]`——`--image` 注册项恒在打开项最前（`CoreCtx::device_infos` 序）。
fn first_image_device(stdin: &mut ChildStdin, rx: &Receiver<serde_json::Value>) -> String {
    send(
        stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"device.list","params":null}),
    );
    let dl = read_response(rx, 1, Duration::from_secs(10));
    let id = dl["result"]["devices"][0]["id"].as_str().unwrap();
    assert!(
        id.starts_with("image:"),
        "devices[0] 应为 --image 注册项：{dl}"
    );
    id.to_string()
}

/// 轮询 `scan.results.total` 直到 ≥ `n`（返回命中时已落库条数）。条目边扫边插 ⇒ 该信号
/// 与检查点是否配对写无关，可在**两种实现下**都停进 run1 的条目相（见深扫续跑测试头注）。
fn wait_for_results_total(
    stdin: &mut ChildStdin,
    rx: &Receiver<serde_json::Value>,
    task_id: i64,
    n: u64,
    timeout: Duration,
) -> u64 {
    let deadline = Instant::now() + timeout;
    loop {
        send(
            stdin,
            json!({"jsonrpc":"2.0","id":902,"method":"scan.results","params":{"taskId":task_id,"offset":0,"limit":1,"deletedOnly":false}}),
        );
        let v = read_response(rx, 902, Duration::from_secs(10));
        let total = v["result"]["total"].as_u64().unwrap();
        if total >= n {
            return total;
        }
        assert!(
            Instant::now() < deadline,
            "等 scan.results.total ≥ {n} 超时（末次 {total}）"
        );
    }
}

/// 拉全量条目（`limit` 契约上限 1000 ⇒ 按 offset 分页取全）→ `(idx, byteOffset, sizeBytes)`
/// 序列（按 idx 升序，即扫描产出序）。逐点比较的形状：丢条、重报、错号都会现形。
fn collect_entries(
    stdin: &mut ChildStdin,
    rx: &Receiver<serde_json::Value>,
    task_id: i64,
) -> Vec<(u64, u64, u64)> {
    let mut out: Vec<(u64, u64, u64)> = Vec::new();
    let mut total = u64::MAX;
    while (out.len() as u64) < total {
        send(
            stdin,
            json!({"jsonrpc":"2.0","id":901,"method":"scan.results","params":{"taskId":task_id,"offset":out.len(),"limit":1000,"deletedOnly":false}}),
        );
        let rs = read_response(rx, 901, Duration::from_secs(10));
        total = rs["result"]["total"].as_u64().unwrap();
        let page = rs["result"]["entries"].as_array().unwrap();
        assert!(
            !page.is_empty(),
            "分页停滞（offset {} < total {total}）",
            out.len()
        );
        out.extend(page.iter().map(|e| {
            (
                e["idx"].as_u64().unwrap(),
                e["byteOffset"].as_u64().unwrap(),
                e["sizeBytes"].as_u64().unwrap(),
            )
        }));
    }
    assert_eq!(out.len() as u64, total, "分页须取全");
    out.sort_unstable();
    out
}

/// 深扫续跑夹具（spec-t7 阻断的 daemon 级形状）：exFAT 1MiB 卷 + **两个空闲区间**，
/// run0 埋 40 枚（512B 间距）、run1 埋 1560 枚（256B 间距；间距 > png 长 61B ⇒ 互不重叠）。
/// run0 刻意压到 40 枚（相位 ~5ms ≪ 250ms）：节流进度若在 run1 的 `Scanned` 上触发，会
/// 与配对写**同值同帧**地落 `found_count`（单写旧实现被顺手治好、牙被磨钝）——run0 相位
/// 越短，该掩蔽所需的时钟停顿倍数越高（5ms → 需 ~50× 停顿，实测不复现）。run1 的 1560 条
/// 把条目相撑到 ≫ IPC 往返：观测到 total > 40 后仍有充裕余量才收尾。
///
/// 几何（实测自 `xd_fs_exfat::freespace::unallocated_runs`，非头注猜读——早期版本按
/// 「数据自簇 6=24576」硬编码，把 run0 首埋点写进了根目录簇 5，镜像位图即不可读、
/// 深扫 -32005 响亮拒绝）：4KiB 簇、数据堆簇 n 字节 = 16384+(n-2)*4096、簇 2 位图 /
/// 3-4 upcase / 5 根目录；BIG.TMP 占簇 6..80、LIVE.TXT 簇 81 ⇒ 空闲 run0 = 簇 6..80
/// = [32768, 339968)，run1 = 簇 82..253 = [344064, 1048576)（区间间隙 4096B ≥ 7）。
/// 埋点自各区间起点 +4096（避开续跑点回退 7B 的量级，亦不压区间首簇头）。布局漂移会以
/// 「计数不吃夹具断言 / 深扫拒绝」形态**响亮**失败（不是静默失明）。
fn deep_resume_image_bytes() -> Vec<u8> {
    const RUN0: u64 = 32768;
    const RUN1: u64 = 344064;
    let mut img = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "BIG.TMP", &[0u8; 300 * 1024])
        .add_file("/", "LIVE.TXT", b"live")
        .delete("/", "BIG.TMP")
        .build();
    let png = xd_fixtures::mini_png(b"deep");
    for (start, n, step) in [(RUN0, 40u64, 512u64), (RUN1, 1560u64, 256u64)] {
        for i in 0..n {
            let off = start + 4096 + i * step;
            assert!(
                off + png.len() as u64 <= if start == RUN0 { 339968 } else { 1048576 },
                "埋点 {off} 越出区间"
            );
            xd_fixtures::plant_in_run(&mut img, off, &png);
        }
    }
    img
}

fn exfat_image_bytes() -> Vec<u8> {
    // 与 xd-core testutil::exfat_fixture 同构：3 条目（2 live + 1 删除）
    xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "LIVE_A.TXT", b"aaaa")
        .add_file("/", "LIVE_B.PNG", &[5u8; 100])
        .add_file("/", "DEL_ME.JPG", &[7u8; 9000])
        .delete("/", "DEL_ME.JPG")
        .build()
}

/// 慢扫镜像：FAT16 基础镜像（DCIM 一级子目录 + 一条删除文件），再把根目录区补满
/// 511 条 `0xE5` 首字节的删除项（槽 0 留给 builder 写的 DCIM）——共 513 条目。
/// 每条目在 worker 里一次落库（T7 阶段三后 8288-10265 条/秒 ⇒ 全图 ~46-62ms；T7 前的
/// DELETE+FULL 是 ~6.9ms/条、全图 ~3.5s——窗口量级仍 ≫ IPC 往返，见文件头注）：
/// cancel（增补 2）/ SIGKILL（增补 4）的「扫描中」窗口 ≫ 一次 IPC 往返，靠它成立。
fn slow_fat_image_bytes() -> Vec<u8> {
    let mut b = xd_fixtures::FatImageBuilder::fat16();
    b.add_subdir("/", "DCIM")
        .add_file("/DCIM", "MG_0001.JPG", &[9u8; 256])
        .delete("/DCIM", "MG_0001.JPG");
    let mut img = b.build();
    // FAT16 夹具布局（xd-fixtures 头注）：reserved 1 + FAT 17 扇区 → 根目录区自扇区 18 起，
    // 512 槽 × 32B。
    const ROOT_OFF: usize = 18 * 512;
    for slot in 1..512usize {
        let mut name = [b'A'; 11];
        name[8..].copy_from_slice(b"TXT");
        name[0] = 0xE5; // 已删除
        let mut e = [0u8; 32];
        e[..11].copy_from_slice(&name);
        e[11] = 0x20; // ATTR_ARCHIVE
        img[ROOT_OFF + slot * 32..ROOT_OFF + slot * 32 + 32].copy_from_slice(&e);
    }
    img
}

#[test]
fn scan_flow_and_restart_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("vol.img");
    std::fs::write(&img_path, exfat_image_bytes()).unwrap();
    let db = dir.path().join("tasks.db");

    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &db);
    let dev_id = first_image_device(&mut stdin, &rx);

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev_id,"mode":"quick"}}),
    );
    let st = read_response(&rx, 2, Duration::from_secs(10));
    assert_eq!(st["result"]["fs"], "exfat");
    let task_id = st["result"]["taskId"].as_i64().unwrap();

    let fin = wait_notification(&rx, "scan.finished", Duration::from_secs(30));
    assert_eq!(fin["params"]["taskId"], task_id);
    assert_eq!(fin["params"]["state"], "completed");
    assert_eq!(fin["params"]["foundCount"], 3);
    // 增补 6：`elapsedMs` 语义随 finished 交付（发射分支本身不 pin——裁定见文件头注）
    assert!(fin["params"]["elapsedMs"].is_u64(), "finished: {fin}");

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"scan.results","params":{"taskId":task_id,"offset":0,"limit":2,"deletedOnly":false}}),
    );
    let rs = read_response(&rx, 3, Duration::from_secs(10));
    assert_eq!(rs["result"]["total"], 3);
    assert_eq!(
        rs["result"]["entries"].as_array().unwrap().len(),
        2,
        "分页 limit=2"
    );
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":4,"method":"scan.results","params":{"taskId":task_id,"offset":0,"limit":10,"deletedOnly":true}}),
    );
    let rs2 = read_response(&rx, 4, Duration::from_secs(10));
    assert_eq!(rs2["result"]["total"], 1);
    assert_eq!(
        rs2["result"]["entries"][0]["name"], "DEL_ME.JPG",
        "exFAT 删除名一字不差"
    );
    assert_eq!(rs2["result"]["entries"][0]["deleted"], true);

    drop(stdin); // stdin EOF → daemon 退出
    let _ = child.wait();

    // 重启：同一 --db → 结果可查、completed 不被 mark_interrupted 误伤
    let (mut child2, mut stdin2, rx2, _err2) = spawn_image_daemon(&img_path, &db);
    send(
        &mut stdin2,
        json!({"jsonrpc":"2.0","id":5,"method":"scan.status","params":{"taskId":task_id}}),
    );
    let s2 = read_response(&rx2, 5, Duration::from_secs(10));
    assert_eq!(s2["result"]["state"], "completed");
    send(
        &mut stdin2,
        json!({"jsonrpc":"2.0","id":6,"method":"scan.results","params":{"taskId":task_id,"offset":0,"limit":10,"deletedOnly":false}}),
    );
    let r2 = read_response(&rx2, 6, Duration::from_secs(10));
    assert_eq!(r2["result"]["total"], 3, "重启后结果可查（SQLite 持久）");
    // T5 记录转 T8 的纵深防御（qual-t3）：observer 1:1 ⇒ idx 集合恰为 0..found_count（防回调重复致库内双行）
    let mut idxs: Vec<u64> = r2["result"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["idx"].as_u64().unwrap())
        .collect();
    idxs.sort_unstable();
    assert_eq!(idxs, vec![0, 1, 2], "idx 集合 == 0..found_count");
    drop(stdin2);
    let _ = child2.wait();
}

#[test]
fn scan_start_on_unregistered_image_id_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("vol.img");
    std::fs::write(&img, exfat_image_bytes()).unwrap();
    let db = dir.path().join("t.db");
    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img, &db);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"scan.start","params":{"device":"image:/etc/hostname"}}),
    );
    let r = read_response(&rx, 1, Duration::from_secs(10));
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(
        r["error"]["message"],
        "Cannot open device: image:/etc/hostname"
    );
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn pause_resume_over_ipc() {
    // 大图 + 立即 pause：条目边界驻停 → status 冻结 → resume 完成
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("big.img");
    let mut b = xd_fixtures::ExfatImageBuilder::new();
    for i in 0..80u32 {
        b.add_file("/", &format!("F{i:04}.BIN"), &[3u8; 200]);
    }
    std::fs::write(&img_path, b.build()).unwrap();
    let db = dir.path().join("t.db");
    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &db);
    let dev_id = first_image_device(&mut stdin, &rx);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev_id}}),
    );
    let task_id = read_response(&rx, 2, Duration::from_secs(10))["result"]["taskId"]
        .as_i64()
        .unwrap();
    // 小图可能秒完：pause 对终态返回 -32004 或对活动态 ok paused，两者皆合法——断言二选一
    // （IPC 层不强求确定性时序；确定性驻停已由 T5 的 SlowDev 单测覆盖）。
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"scan.pause","params":{"taskId":task_id}}),
    );
    let p = read_response(&rx, 3, Duration::from_secs(10));
    let code = p["error"]["code"].as_i64();
    assert!(
        p["result"]["state"] == "paused" || code == Some(-32004),
        "pause: {p}"
    );
    if p["result"]["state"] == "paused" {
        send(
            &mut stdin,
            json!({"jsonrpc":"2.0","id":4,"method":"scan.resume","params":{"taskId":task_id}}),
        );
        let r = read_response(&rx, 4, Duration::from_secs(10));
        assert_eq!(r["result"]["state"], "scanning");
    }
    let fin = wait_notification(&rx, "scan.finished", Duration::from_secs(30));
    assert_eq!(fin["params"]["taskId"], task_id);
    assert_eq!(fin["params"]["foundCount"], 80);
    drop(stdin);
    let _ = child.wait();
}

/// 增补 2：cancel 于扫描中 → ScanCanceled hook 静默（stderr 无 `panicked`）+ finished=canceled
/// 证明取消真发生。慢镜像（513 条目 ≈ 46-62ms 纯落库，spec-t7 量化）把「扫描中」窗口撑到 ≫ 一次 IPC 往返，
/// 故严格断言 canceled（非二态）；cancel 通常在**首个检查点**即落地（foundCount 尚 0）——
/// 响应 ok=canceled + finished=canceled 即证明 worker 在场且被取消，而非跑完或没跑。
#[test]
fn cancel_mid_scan_is_silent_and_finishes_canceled() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("slow.img");
    std::fs::write(&img_path, slow_fat_image_bytes()).unwrap();
    let db = dir.path().join("t.db");
    let (mut child, mut stdin, rx, mut stderr) = spawn_image_daemon(&img_path, &db);

    let dev_id = first_image_device(&mut stdin, &rx);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev_id}}),
    );
    let st = read_response(&rx, 2, Duration::from_secs(10));
    assert_eq!(st["result"]["fs"], "fat");
    let task_id = st["result"]["taskId"].as_i64().unwrap();

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"scan.cancel","params":{"taskId":task_id}}),
    );
    let c = read_response(&rx, 3, Duration::from_secs(10));
    assert_eq!(
        c["result"]["state"], "canceled",
        "慢镜像下 cancel 必落扫描中：{c}"
    );

    // 等 finished，顺带收集 progress 做增补 6 的弱判别（数量 ≤ 条目数；发射分支不 pin）。
    let mut progress_seen = 0u64;
    let fin = loop {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(v) if v["method"] == "scan.finished" => break v,
            Ok(v) if v["method"] == "scan.progress" => {
                let p = &v["params"];
                assert!(
                    p["readBytes"].is_u64() && p["elapsedMs"].is_u64() && p["foundCount"].is_u64(),
                    "progress 语义字段：{v}"
                );
                progress_seen += 1;
            }
            Ok(_) => {}
            Err(e) => panic!("no scan.finished after cancel: {e}"),
        }
    };
    assert_eq!(fin["params"]["taskId"], task_id);
    assert_eq!(fin["params"]["state"], "canceled");
    let found = fin["params"]["foundCount"].as_u64().unwrap();
    assert!(
        progress_seen <= found,
        "progress 条数不应多于条目数（弱判别）：{progress_seen} > {found}"
    );

    drop(stdin);
    let _ = child.wait();
    let mut err = String::new();
    stderr
        .read_to_string(&mut err) // 进程已退出 → 管道 EOF
        .expect("读 stderr");
    assert!(
        err.contains("xd-daemon"),
        "stderr 捕获失效（banner 缺失）：{err}"
    );
    assert!(
        !err.contains("panicked"),
        "ScanCanceled 泄漏到 stderr：{err}"
    );
}

/// 增补 3：`--db` 指向目录 → 打开失败降级内存库：ping 正常 + stderr 留痕 + 进程不退出。
#[test]
fn db_open_failure_degrades_to_memory_and_stays_alive() {
    let dir = tempfile::tempdir().unwrap(); // 目录本身作为 --db
    let (mut child, mut stdin, rx, mut stderr) =
        spawn_daemon(&["--db", dir.path().to_str().unwrap()], &[]);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"ping","params":null}),
    );
    assert_eq!(
        read_response(&rx, 1, Duration::from_secs(10))["result"]["pong"],
        true
    );
    // 再来一次：证明是「降级后继续服务」而非「回完就退」
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"ping","params":null}),
    );
    assert_eq!(
        read_response(&rx, 2, Duration::from_secs(10))["result"]["pong"],
        true
    );
    drop(stdin);
    let st = child.wait().unwrap();
    assert_eq!(st.code(), Some(0), "EOF 退出仍应为 0");
    let mut err = String::new();
    stderr.read_to_string(&mut err).expect("读 stderr");
    assert!(err.contains("任务库打开失败"), "stderr 未留降级痕：{err}");
}

/// 增补 4：SIGKILL 于扫描中 → 同 `--db` 重启 → 中断任务 failed、paused 任务保留
/// （recover_after_restart 护栏的 daemon 级钉死）。
#[test]
fn sigkill_mid_scan_leaves_failed_and_keeps_paused() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("slow.img");
    std::fs::write(&img_path, slow_fat_image_bytes()).unwrap();
    let db = dir.path().join("t.db");
    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &db);
    let dev_id = first_image_device(&mut stdin, &rx);

    // B：先起并驻停——pause 在主循环同步落库，响应返回即持久。
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev_id}}),
    );
    let task_b = read_response(&rx, 2, Duration::from_secs(10))["result"]["taskId"]
        .as_i64()
        .unwrap();
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"scan.pause","params":{"taskId":task_b}}),
    );
    let p = read_response(&rx, 3, Duration::from_secs(10));
    assert_eq!(
        p["result"]["state"], "paused",
        "慢镜像下 pause 必落扫描中：{p}"
    );

    // A：起后立即 SIGKILL（响应返回时 worker 至多刚开跑；A 的行已在库内 = scanning）。
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":4,"method":"scan.start","params":{"device":dev_id}}),
    );
    let task_a = read_response(&rx, 4, Duration::from_secs(10))["result"]["taskId"]
        .as_i64()
        .unwrap();
    while let Ok(v) = rx.try_recv() {
        assert_ne!(
            v["method"], "scan.finished",
            "慢镜像未撑住窗口：kill 前已有任务跑完（{v}）"
        );
    }
    // 窗口量级：A 的 513 条目 ≈ 46-62ms 落库（WAL+NORMAL）vs kill 紧随响应（µs 级）——裕度 ~100×。
    child.kill().unwrap(); // Unix 上即 SIGKILL
    let _ = child.wait();

    // 同库重启：scanning → failed（mark_interrupted），paused 保留。
    let (mut child2, mut stdin2, rx2, _err2) = spawn_image_daemon(&img_path, &db);
    send(
        &mut stdin2,
        json!({"jsonrpc":"2.0","id":5,"method":"scan.status","params":{"taskId":task_a}}),
    );
    let a = read_response(&rx2, 5, Duration::from_secs(10));
    assert_eq!(a["result"]["state"], "failed", "SIGKILL 中断 → failed：{a}");
    send(
        &mut stdin2,
        json!({"jsonrpc":"2.0","id":6,"method":"scan.status","params":{"taskId":task_b}}),
    );
    let b = read_response(&rx2, 6, Duration::from_secs(10));
    assert_eq!(b["result"]["state"], "paused", "paused 跨重启保留：{b}");
    drop(stdin2);
    let _ = child2.wait();
}

/// 深扫断点续跑的 daemon 级全链路（plan Step 2 的 `deep_scan_over_ipc_with_checkpoint_restart`
/// 真落地）：深扫 → 暂停驻留在**第二个空闲区间** → SIGKILL → 同 `--db` 重启 → `scan.resume`
/// （NeedsDevice → daemon 重开设备 → restart 续跑）→ 条目集合与**不中断参照运行逐点相等**。
/// 判别力（spec-t7 阻断的牙）：暂停时刻 run0 的 40 条已落库而 `found_count` 只有配对检查点
/// 写过（250ms 节流在 ~5ms 的 run0 相位内不触发）——单写旧实现以滞后计数续号，重扫 run1 的
/// 同号 REPLACE 覆盖 run0 旧行 → 总数 1560 ≠ 1600、逐点比较必红。
/// 定位器用 `scan.results.total`（> 400 即 run1 的 Scanned 已处理）而非 `scan.status.foundCount`：
/// 后者本身**依赖配对写**——单写旧实现下扫描全程恒 0，以它为定位器时命中的是「扫描已收尾」，
/// 牙会咬在「观测不到」而非配对不变量上（T7 实测：那条路径下报错信息与丢条毫无关系）。
#[test]
fn deep_scan_sigkill_resume_matches_uninterrupted_run_pointwise() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("deep.img");
    std::fs::write(&img_path, deep_resume_image_bytes()).unwrap();

    // 参照：不中断的深扫（独立 --db），基准全量序列
    let reference = {
        let ref_db = dir.path().join("ref.db");
        let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &ref_db);
        let dev_id = first_image_device(&mut stdin, &rx);
        send(
            &mut stdin,
            json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev_id,"mode":"deep"}}),
        );
        let sres = read_response(&rx, 2, Duration::from_secs(10));
        assert!(
            sres.get("error").is_none(),
            "深扫 start 须成功（-32005 = 空闲区间不可枚举：夹具几何/位图坏了）: {sres}"
        );
        let task = sres["result"]["taskId"].as_i64().unwrap();
        let fin = wait_notification(&rx, "scan.finished", Duration::from_secs(60));
        assert_eq!(fin["params"]["taskId"], task);
        assert_eq!(fin["params"]["state"], "completed");
        let entries = collect_entries(&mut stdin, &rx, task);
        drop(stdin);
        let _ = child.wait();
        entries
    };
    assert_eq!(
        reference.len(),
        1600,
        "参照全量 = run0 40 + run1 1560（夹具布局漂移必在此响亮失败）"
    );

    // 中断运行：深扫 → 落库数越过 run0（定位器 = `scan.results.total`，**与配对写无关**：
    // 条目边扫边插，单写旧实现同样能停进 run1，牙落在配对不变量而非定位器上）→ pause → SIGKILL
    let db = dir.path().join("t.db");
    let (mut child, mut stdin, rx, _err) = spawn_image_daemon(&img_path, &db);
    let dev_id = first_image_device(&mut stdin, &rx);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev_id,"mode":"deep"}}),
    );
    let task_id = read_response(&rx, 2, Duration::from_secs(10))["result"]["taskId"]
        .as_i64()
        .unwrap();
    let landed = wait_for_results_total(&mut stdin, &rx, task_id, 41, Duration::from_secs(60));
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":906,"method":"scan.status","params":{"taskId":task_id}}),
    );
    let st = read_response(&rx, 906, Duration::from_secs(10))["result"].clone();
    assert_eq!(
        st["state"], "scanning",
        "已落库 {landed} 条（> 40 ⇒ run1 的 Scanned 已处理）时仍须在扫描中：{st}"
    );
    // 定位阈 41 而非 40：run1 的条目只可能在其窗口的 `Scanned{at=344064}` **之后**产出，
    // 故 total > 40 是「配对检查点（344064 与条目计数）确已落盘」的**充分**证据——
    // 若恰停在 40 界上，杀机可能抢在 run1 的 Scanned 之前，检查点仍指 run0（旧实现下
    // 全量重扫即无损失、牙钝）。阈值 41 把「杀进 run1」从竞态变成确定。
    assert_eq!(
        st["foundCount"].as_u64(),
        Some(40),
        "扫描中 found_count 必为配对检查点值（= run0 条目数）：节流进度从未写过、\
         单写旧实现此处 0（滞后计数 → 续号覆盖 run0 旧行 = spec-t7 丢条）"
    );
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"scan.pause","params":{"taskId":task_id}}),
    );
    let p = read_response(&rx, 3, Duration::from_secs(10));
    assert_eq!(
        p["result"]["state"], "paused",
        "深扫中 pause 必须落扫描中（慢夹具撑住窗口的等价判据）：{p}"
    );
    child.kill().unwrap(); // SIGKILL：无干净关闭（WAL 恢复路径）
    let _ = child.wait();

    // 同库重启 → paused 保留 → scan.resume（NeedsDevice → 重开设备 → restart 按检查点续跑）
    let (mut child2, mut stdin2, rx2, _err2) = spawn_image_daemon(&img_path, &db);
    send(
        &mut stdin2,
        json!({"jsonrpc":"2.0","id":4,"method":"scan.status","params":{"taskId":task_id}}),
    );
    let st2 = read_response(&rx2, 4, Duration::from_secs(10));
    assert_eq!(
        st2["result"]["state"], "paused",
        "SIGKILL 后 paused 跨重启保留"
    );
    assert_eq!(
        st2["result"]["foundCount"].as_u64(),
        Some(40),
        "配对检查点跨进程存续：found_count 与 carved_offset 同帧（单写旧实现此处 0）"
    );
    send(
        &mut stdin2,
        json!({"jsonrpc":"2.0","id":5,"method":"scan.resume","params":{"taskId":task_id}}),
    );
    let r = read_response(&rx2, 5, Duration::from_secs(10));
    assert_eq!(
        r["result"]["state"], "scanning",
        "resume 须走 NeedsDevice → 重开设备 → restart 续跑：{r}"
    );
    let fin2 = wait_notification(&rx2, "scan.finished", Duration::from_secs(60));
    assert_eq!(fin2["params"]["taskId"], task_id);
    assert_eq!(fin2["params"]["state"], "completed");
    let resumed = collect_entries(&mut stdin2, &rx2, task_id);
    drop(stdin2);
    let _ = child2.wait();

    assert_eq!(
        resumed, reference,
        "续跑条目集合（idx/偏移/大小）必须与不中断参照运行逐点相等"
    );
}

/// 增补 5：`chmod 000` 常规文件 + `unix:` id → -32001（懒打开唯一出口的权限映射）。
/// root 下 chmod 000 不拦 open（EACCES 无从构造）→ 自跳过并留痕；euid 不可读同样跳过。
#[cfg(target_os = "linux")]
#[test]
fn eacces_on_unix_devid_maps_to_minus_32001() {
    let Some(euid) = effective_uid() else {
        eprintln!("skip: euid 不可读（/proc/self/status 缺 Uid 行）");
        return;
    };
    if euid == 0 {
        eprintln!("skip: root 下 chmod 000 仍可读，EACCES 无法构造");
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked.bin");
    std::fs::write(&locked, b"x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let db = dir.path().join("t.db");
    // 不注册 --image（chmod 000 时 daemon 会 exit(2)，测不到扫描路径）；直走 `unix:` 懒打开。
    let (mut child, mut stdin, rx, _err) = spawn_daemon(&["--db", db.to_str().unwrap()], &[]);
    let id = format!("unix:{}", locked.display());
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"scan.start","params":{"device":id}}),
    );
    let r = read_response(&rx, 1, Duration::from_secs(10));
    assert_eq!(r["error"]["code"], -32001, "{r}");
    assert_eq!(
        r["error"]["message"],
        format!("Device permission denied: unix:{}", locked.display())
    );
    drop(stdin);
    let _ = child.wait();
}

#[cfg(target_os = "linux")]
fn effective_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|l| l.starts_with("Uid:"))
        .and_then(|l| l.split_whitespace().nth(2))
        .and_then(|s| s.parse().ok())
}

/// 增补 7：不传 `--db`、`XDG_STATE_HOME` 与 `HOME` 并存 → 库落 XDG（qual-t6 变异 9 的归属护栏）。
#[test]
fn db_default_prefers_xdg_state_home_over_home() {
    let dir = tempfile::tempdir().unwrap();
    let xdg = dir.path().join("xdg");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&xdg).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let (mut child, mut stdin, rx, _err) = spawn_daemon(
        &[],
        &[
            ("XDG_STATE_HOME", xdg.to_str().unwrap()),
            ("HOME", home.to_str().unwrap()),
        ],
    );
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"ping","params":null}),
    );
    assert_eq!(
        read_response(&rx, 1, Duration::from_secs(10))["result"]["pong"],
        true
    );
    assert!(
        xdg.join("xiaodun/tasks.db").exists(),
        "库应落 XDG_STATE_HOME（XDG 优先）"
    );
    assert!(
        !home.join(".local/state/xiaodun/tasks.db").exists(),
        "库不得落 HOME（XDG 在场时）"
    );
    drop(stdin);
    let _ = child.wait();
}
