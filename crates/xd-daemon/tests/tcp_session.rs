// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! TCP 回环传输全链路（M1e-tail Task 1 Step 4，三平台 CI 真跑）：
//! 1) 握手（ping/device.list）+ 错误令牌 -32001 即断（含「首行非 auth」形态）；
//! 2) port-file 0600 + 无临时文件残留（unix）；
//! 3) 通知到达**同连接**（scan.start → scan.finished）；
//! 4) 通知广播全部已认证连接 + 断连剔除后广播不受影响（transport.rs 头注的 M1 简化钉子）；
//! 5) 会话生命周期（T4）：属主消亡自退、连接全断后空转自退、`--owner-pid` 成对约束
//!    ——三者都必须清 port-file，避免 root daemon 无主滞留/多轮提权累积（docs/security §10）。
//!
//! stdout 仅协议：TCP 模式全程断言 stdout 为空（诊断全 stderr）。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// 子进程守护：drop 即 kill+wait（panic 路径也不留孤儿 daemon）。
struct Daemon {
    child: Child,
    stdout: Option<ChildStdout>,
}

impl Daemon {
    /// 结束进程并断言 stdout 全程为空（TCP 模式 stdout 仅协议 ⇒ 无任何输出；诊断全 stderr）。
    fn kill_and_assert_stdout_empty(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let mut out = String::new();
        if let Some(mut s) = self.stdout.take() {
            let _ = s.read_to_string(&mut out);
        }
        assert!(out.is_empty(), "TCP 模式 stdout 必须为空：{out:?}");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `--listen 127.0.0.1:0 --port-file <pf>` + 附加参数；stdin 给 null（TCP 模式不读 stdin）。
fn spawn_tcp(args: &[&str], port_file: &Path) -> Daemon {
    let mut child = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
        .args([
            "--listen",
            "127.0.0.1:0",
            "--port-file",
            port_file.to_str().unwrap(),
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take();
    Daemon { child, stdout }
}

/// 轮询 port-file 出现并解析 `<port> <token>`（unix 原子 rename ⇒ 必是完整一行；Windows 直写，
/// 故以 token 形态判别就绪，见下）。
fn wait_port_file(path: &Path) -> (u16, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            let mut it = s.split_whitespace();
            if let (Some(p), Some(t)) = (it.next(), it.next()) {
                // token 恒 32 位十六进制（契约）：Windows 的 port-file 是直写（非原子，
                // 计划裁定），半行读取必须视作「未就绪」继续轮询——否则截断 token 会被
                // 当结果返回，测试在 Windows CI 上颤动（spec 观察③）。
                if t.len() == 32 && t.chars().all(|c| c.is_ascii_hexdigit()) {
                    return (p.parse().expect("port"), t.to_string());
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "等 port-file 超时: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn send(stream: &mut TcpStream, v: Value) {
    writeln!(stream, "{v}").unwrap();
    stream.flush().unwrap();
}

/// 连接 + 认证首行（无 ack：协议如此，成功与否由首个请求的响应区分）。
fn connect_authed(port: u16, token: &str) -> (TcpStream, BufReader<TcpStream>) {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream.try_clone().unwrap();
    writeln!(w, r#"{{"auth":"{token}"}}"#).unwrap();
    w.flush().unwrap();
    (stream, reader)
}

/// 「响应 ⇄ 通知」两序容忍读口（scan_ipc::Wire 的 socket 收敛版）：先到者寄存，不丢弃。
struct Lines {
    reader: BufReader<TcpStream>,
    pending: Vec<Value>,
}

impl Lines {
    fn new(reader: BufReader<TcpStream>) -> Self {
        Self {
            reader,
            pending: Vec::new(),
        }
    }

    fn next(&mut self) -> Value {
        if !self.pending.is_empty() {
            return self.pending.remove(0);
        }
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("socket read: {e}（超时 = 对端迟迟不写）"));
        assert!(n > 0, "连接被对端关闭");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("坏行 {line:?}: {e}"))
    }

    fn response(&mut self, id: i64) -> Value {
        if let Some(i) = self
            .pending
            .iter()
            .position(|v| v["id"].as_i64() == Some(id))
        {
            return self.pending.swap_remove(i);
        }
        loop {
            let v = self.next();
            if v["id"].as_i64() == Some(id) {
                return v;
            }
            self.pending.push(v);
        }
    }

    fn notification(&mut self, method: &str) -> Value {
        if let Some(i) = self
            .pending
            .iter()
            .position(|v| v["method"].as_str() == Some(method))
        {
            return self.pending.swap_remove(i);
        }
        loop {
            let v = self.next();
            if v["method"].as_str() == Some(method) {
                return v;
            }
            self.pending.push(v);
        }
    }
}

/// 与 scan_ipc 同构的 3 条目 exFAT 夹具（2 live + 1 删除）。
fn exfat_image_bytes() -> Vec<u8> {
    xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "LIVE_A.TXT", b"aaaa")
        .add_file("/", "LIVE_B.PNG", &[5u8; 100])
        .add_file("/", "DEL_ME.JPG", &[7u8; 9000])
        .delete("/", "DEL_ME.JPG")
        .build()
}

/// 注册镜像并返回 `device.list` 的 `devices[0].id`（`--image` 注册项恒在最前）。
fn first_image_device(stream: &mut TcpStream, lines: &mut Lines, id: i64) -> String {
    send(
        stream,
        json!({"jsonrpc":"2.0","id":id,"method":"device.list","params":null}),
    );
    let dl = lines.response(id);
    let dev = dl["result"]["devices"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(dev.starts_with("image:"), "{dl}");
    dev
}

#[test]
fn tcp_session_full_flow_and_wrong_token_disconnect() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("vol.img");
    std::fs::write(&img, exfat_image_bytes()).unwrap();
    let db = dir.path().join("t.db");
    let pf = dir.path().join("session.port");
    let mut daemon = spawn_tcp(
        &[
            "--image",
            img.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ],
        &pf,
    );

    let (port, token) = wait_port_file(&pf);
    // 令牌形态：32 位十六进制（16 字节 CSPRNG）
    assert_eq!(token.len(), 32, "token: {token}");
    assert!(
        token.chars().all(|c| c.is_ascii_hexdigit()),
        "token: {token}"
    );

    let (mut stream, reader) = connect_authed(port, &token);
    let mut lines = Lines::new(reader);
    send(
        &mut stream,
        json!({"jsonrpc":"2.0","id":1,"method":"ping","params":null}),
    );
    assert_eq!(lines.response(1)["result"]["pong"], true);
    let dev = first_image_device(&mut stream, &mut lines, 2);
    assert!(dev.ends_with("vol.img"), "{dev}");
    drop(stream);

    // 错误令牌：-32001 后立即断开（读到 EOF 才算数——不封闭则 5s 超时红）
    let bad = TcpStream::connect(("127.0.0.1", port)).unwrap();
    bad.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut bad_reader = BufReader::new(bad.try_clone().unwrap());
    let mut w = bad.try_clone().unwrap();
    writeln!(w, r#"{{"auth":"00000000000000000000000000000000"}}"#).unwrap();
    w.flush().unwrap();
    let mut line = String::new();
    bad_reader.read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["error"]["code"], -32001, "{v}");
    assert!(v["id"].is_null(), "{v}");
    let mut rest = Vec::new();
    assert_eq!(
        bad_reader.read_to_end(&mut rest).unwrap(),
        0,
        "认证失败必须断开"
    );

    // 首行不是 auth（直接发请求）同样 -32001 断开
    let bad2 = TcpStream::connect(("127.0.0.1", port)).unwrap();
    bad2.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut r2 = BufReader::new(bad2.try_clone().unwrap());
    let mut w2 = bad2.try_clone().unwrap();
    writeln!(
        w2,
        r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":null}}"#
    )
    .unwrap();
    w2.flush().unwrap();
    let mut l2 = String::new();
    r2.read_line(&mut l2).unwrap();
    let v2: Value = serde_json::from_str(&l2).unwrap();
    assert_eq!(v2["error"]["code"], -32001, "{v2}");

    daemon.kill_and_assert_stdout_empty();
}

#[cfg(unix)]
#[test]
fn port_file_is_0600_and_leaves_no_temp_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let pf = dir.path().join("session.port");
    let mut daemon = spawn_tcp(&[], &pf);

    let _ = wait_port_file(&pf);
    let md = std::fs::metadata(&pf).unwrap();
    let mode = md.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "port-file 含令牌，必须 0600（仅本用户可读）");
    // 同用户写入（本测试非提权）：属主 = 目录属主 = 写者，adopt_owner_of_dir 无事可做。
    use std::os::unix::fs::MetadataExt;
    let dir_uid = std::fs::metadata(dir.path()).unwrap().uid();
    assert_eq!(md.uid(), dir_uid, "非提权写入不得改属主");
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "rename 后不得残留临时文件: {leftovers:?}"
    );

    daemon.kill_and_assert_stdout_empty();
}

#[test]
fn tcp_scan_notifications_reach_client() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("vol.img");
    std::fs::write(&img, exfat_image_bytes()).unwrap();
    let db = dir.path().join("t.db");
    let pf = dir.path().join("session.port");
    let mut daemon = spawn_tcp(
        &[
            "--image",
            img.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ],
        &pf,
    );

    let (port, token) = wait_port_file(&pf);
    let (mut stream, reader) = connect_authed(port, &token);
    let mut lines = Lines::new(reader);
    let dev = first_image_device(&mut stream, &mut lines, 1);

    send(
        &mut stream,
        json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev,"mode":"quick"}}),
    );
    let st = lines.response(2);
    assert!(st.get("error").is_none(), "{st}");
    let task = st["result"]["taskId"].as_i64().unwrap();
    let fin = lines.notification("scan.finished");
    assert_eq!(fin["params"]["taskId"], task);
    assert_eq!(fin["params"]["state"], "completed");
    assert_eq!(fin["params"]["foundCount"], 3);

    daemon.kill_and_assert_stdout_empty();
}

// ---- 会话生命周期（T4）：回环 TCP 全链路可验，不需要真提权 ----

/// 属主（`--owner-pid`）消亡 ⇒ daemon 自退（退出码 0）并清理 port-file。
/// 属主用**另一个 daemon 进程**扮演（stdin 管道保持打开即存活；kill = 模拟 UI 退出）——
/// 跨平台同款，不引平台专有 sleep/dummy 进程。
#[test]
fn daemon_exits_and_cleans_port_file_when_owner_dies() {
    let dir = tempfile::tempdir().unwrap();
    let pf = dir.path().join("session.port");

    let mut owner = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
        .stdin(Stdio::piped()) // 保持打开 ⇒ 属主 daemon 阻塞读 stdin，存活
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn owner daemon");
    let owner_pid = owner.id().to_string();

    let mut daemon = spawn_tcp(&["--owner-pid", &owner_pid], &pf);
    let _ = wait_port_file(&pf);

    owner.kill().expect("kill owner");
    owner.wait().expect("reap owner");

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(st) = daemon.child.try_wait().unwrap() {
            break st;
        }
        assert!(
            Instant::now() < deadline,
            "属主消亡后 daemon 必须在 10s 内自退（监督轮询 500ms + 宽限）"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        status.success(),
        "自退是正常路径（非错误），退出码应为 0：{status:?}"
    );
    assert!(
        !pf.exists(),
        "自退时必须清理 port-file（令牌交接件不得滞留）"
    );
    daemon.kill_and_assert_stdout_empty();
}

/// 出现过已认证连接、随后全部断开 ⇒ 空转自退 + 清 port-file（同一 UI 多轮提权不累积 root daemon）。
#[test]
fn daemon_exits_after_last_authenticated_connection_drops() {
    let dir = tempfile::tempdir().unwrap();
    let pf = dir.path().join("session.port");
    let mut daemon = spawn_tcp(&[], &pf);
    let (port, token) = wait_port_file(&pf);

    {
        let (mut stream, reader) = connect_authed(port, &token);
        let mut lines = Lines::new(reader);
        // 认证无 ack：以一次往返确认「已注册」（注册先于 serve_lines ⇒ 收到响应即已注册）。
        send(
            &mut stream,
            json!({"jsonrpc":"2.0","id":1,"method":"ping","params":null}),
        );
        assert_eq!(lines.response(1)["result"]["pong"], true);
        // 关掉全部 fd（reader 持 try_clone 的 fd，必须一并 drop 才算断开）
        drop(lines);
        drop(stream);
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(st) = daemon.child.try_wait().unwrap() {
            break st;
        }
        assert!(
            Instant::now() < deadline,
            "连接全断后 daemon 必须在 10s 内空转自退（宽限 3s + 轮询 500ms）"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "退出码应为 0：{status:?}");
    assert!(!pf.exists(), "空转自退同样必须清理 port-file");
    daemon.kill_and_assert_stdout_empty();
}

/// `--owner-pid` 只在提权会话模式（`--listen/--port-file`）下有意义：单独给出 = exit 2（不静默忽略）。
#[test]
fn owner_pid_without_tcp_session_is_rejected() {
    let out = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
        .args(["--owner-pid", "1"])
        .stdin(Stdio::null())
        .output()
        .expect("run daemon");
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--owner-pid"), "错误信息须点名参数：{err}");
}

#[test]
fn notifications_broadcast_to_all_authenticated_connections() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("vol.img");
    std::fs::write(&img, exfat_image_bytes()).unwrap();
    let db = dir.path().join("t.db");
    let pf = dir.path().join("session.port");
    let mut daemon = spawn_tcp(
        &[
            "--image",
            img.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ],
        &pf,
    );

    let (port, token) = wait_port_file(&pf);
    let (mut a, ra) = connect_authed(port, &token);
    let mut la = Lines::new(ra);
    let (b, rb) = connect_authed(port, &token);
    let mut lb = Lines::new(rb);
    let dev = first_image_device(&mut a, &mut la, 1);

    // A 发起扫描：A、B 都应收到 finished（M1 广播给全部已认证连接）
    send(
        &mut a,
        json!({"jsonrpc":"2.0","id":2,"method":"scan.start","params":{"device":dev,"mode":"quick"}}),
    );
    let t1 = la.response(2)["result"]["taskId"].as_i64().unwrap();
    assert_eq!(la.notification("scan.finished")["params"]["taskId"], t1);
    // B 未发任何请求：广播同样到达
    assert_eq!(
        lb.notification("scan.finished")["params"]["taskId"],
        t1,
        "B 连接（已认证、未发请求）必须收到广播"
    );

    // B 断开：死连接从广播集剔除（读线程 EOF / 写失败两条路），A 的下一次扫描不受影响
    drop(b);
    drop(lb);
    std::thread::sleep(Duration::from_millis(100));
    send(
        &mut a,
        json!({"jsonrpc":"2.0","id":3,"method":"scan.start","params":{"device":dev,"mode":"quick"}}),
    );
    let t2 = la.response(3)["result"]["taskId"].as_i64().unwrap();
    assert_eq!(
        la.notification("scan.finished")["params"]["taskId"],
        t2,
        "B 断开后 A 的广播必须照常"
    );

    daemon.kill_and_assert_stdout_empty();
}

/// 提权形态（root daemon ↔ 普通用户 UI）的 port-file 属主与可读性 + 全链路（T4 落地发现的
/// T1 缺口：root 写 0600 ⇒ 属主=root ⇒ UI 读不到令牌，提权链断）：以 `sudo -n` 起 root daemon，
/// port-file 落在当前用户自建的 0700 目录 ⇒ 属主须交还当前用户、权限仍 0600、当前用户可读并
/// 握手 ping（与 UI 侧同路径）；属主进程亡 ⇒ root daemon 自退 + 清 port-file（属主监督在
/// root 下同样成立）。**未验证**：polkit 授权框本身（本测试以 `sudo -n` 代替提权器；
/// 真机 pkexec/osascript 链归出口手测）。无免密 sudo（开发机常态）⇒ 跳过并留痕；CI runner
/// 有免密 sudo ⇒ 真跑。
#[cfg(unix)]
#[test]
fn root_daemon_hands_port_file_back_to_owner_user() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let sudo_ok = Command::new("sudo")
        .args(["-n", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !sudo_ok {
        eprintln!(
            "skip: root_daemon_hands_port_file_back_to_owner_user 需免密 sudo（CI runner 有，开发机无）"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap(); // 0700 + 属主 = 当前用户（UI 侧 createTempSync 同款）
    let pf = dir.path().join("session.port");
    // 属主监督凭据：本测试持有且可杀的进程（= UI 进程的替身；kill 后必须 wait 回收，
    // 否则僵尸进程对 kill(pid,0) 恒存活，属主监督不触发）。
    let mut owner = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn owner stand-in");

    let mut child = Command::new("sudo")
        .args([
            "-n",
            env!("CARGO_BIN_EXE_xd-daemon"),
            "--listen",
            "127.0.0.1:0",
            "--port-file",
            pf.to_str().unwrap(),
            "--owner-pid",
            &owner.id().to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn root daemon via sudo");
    let stdout = child.stdout.take();
    let mut daemon = Daemon { child, stdout };

    // 当前用户读到 port-file 即属主已交还（root:0600 时 read_to_string 必然 EACCES → 轮询超时红）
    let (port, token) = wait_port_file(&pf);
    let md = std::fs::metadata(&pf).unwrap();
    let owner_uid = std::fs::metadata(dir.path()).unwrap().uid();
    assert_eq!(
        md.uid(),
        owner_uid,
        "root daemon 必须把 port-file 属主交还目录属主（UI 用户）"
    );
    assert_eq!(
        md.permissions().mode() & 0o777,
        0o600,
        "属主交还不得放松权限位（令牌仍仅属主可读）"
    );
    // `--nocapture` 下与上面 skip 分支二值可辨：passing test 的 stderr 默认被 cargo 捕获，
    // 单看 `... ok` 分不出「真执行」与「早退 skip」（macos_smoke 同款陷阱）。
    eprintln!("ran: root owner-handback exercised (port-file uid == dir owner)");

    let (mut stream, reader) = connect_authed(port, &token);
    let mut lines = Lines::new(reader);
    send(
        &mut stream,
        json!({"jsonrpc":"2.0","id":1,"method":"ping","params":null}),
    );
    assert_eq!(
        lines.response(1)["result"]["pong"],
        true,
        "root 提权会话按 UI 侧路径（读 port-file → 握手）可用"
    );
    drop(lines);
    drop(stream);

    owner.kill().expect("kill owner stand-in");
    owner.wait().expect("reap owner stand-in");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(st) = daemon.child.try_wait().unwrap() {
            break st;
        }
        assert!(
            Instant::now() < deadline,
            "属主消亡后 root daemon 必须在 10s 内自退"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "sudo/daemon 退出码应为 0：{status:?}");
    assert!(!pf.exists(), "自退必须清理 port-file");
    daemon.kill_and_assert_stdout_empty();
}
