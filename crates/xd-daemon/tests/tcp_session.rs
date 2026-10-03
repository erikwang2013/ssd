// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! TCP 回环传输全链路（M1e-tail Task 1 Step 4，三平台 CI 真跑）：
//! 1) 握手（ping/device.list）+ 错误令牌 -32001 即断（含「首行非 auth」形态）；
//! 2) port-file 0600 + 无临时文件残留（unix）；
//! 3) 通知到达**同连接**（scan.start → scan.finished）；
//! 4) 通知广播全部已认证连接 + 断连剔除后广播不受影响（transport.rs 头注的 M1 简化钉子）。
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
    let mode = std::fs::metadata(&pf).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "port-file 含令牌，必须 0600（仅本用户可读）");
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
