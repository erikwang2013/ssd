// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! qual-m1e-t1 补测（最小集）：封住四个现存测试套未钉住、变异可存活的缺口。
//! 放置：新文件 `crates/xd-daemon/tests/tcp_hardening_supp.rs`（与 tcp_session.rs 平级；
//! 若并入 tcp_session.rs 需去掉重复的 Daemon/wait_port_file 辅助，逐字逻辑不变）。
//!
//! 四个钉子与对应变异：
//! 1) `token_prefix_or_empty_is_rejected` —— 杀「ct_eq 去掉长度检查」（zip 折叠只比前缀 → 前缀即过）。
//! 2) `non_loopback_or_unpaired_listen_args_exit_2` —— 杀「--listen 白名单删除」与
//!    「--listen/--port-file 不成对静默退 stdio」。
//! 3) `blank_line_is_skipped_not_parse_error` —— 杀「serve_lines 空行跳过删除」（空行被回 -32700）。
//! 4) `port_file_replaces_symlink_target_not_write_through` —— 杀「port-file 去掉 tmp+rename 直写目标」
//!    （直写跟随符号链接 = 提权写穿任意受害文件）。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// 子进程守护：drop 即 kill+wait（panic 路径也不留孤儿 daemon）。
struct Daemon {
    child: Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_tcp(args: &[&str], port_file: &Path) -> Daemon {
    let child = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
        .args([
            "--listen",
            "127.0.0.1:0",
            "--port-file",
            port_file.to_str().unwrap(),
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Daemon { child }
}

fn wait_port_file(path: &Path) -> (u16, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            let mut it = s.split_whitespace();
            if let (Some(p), Some(t)) = (it.next(), it.next()) {
                return (p.parse().expect("port"), t.to_string());
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

/// 发一行 auth 后读回一条：返回 (读到的一行, 之后是否 EOF)。
fn try_auth(port: u16, auth_line: &str) -> (String, bool) {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream.try_clone().unwrap();
    writeln!(w, "{auth_line}").unwrap();
    w.flush().unwrap();
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .expect("读响应超时（对端既未回 -32001 也未断开 = 认证被错误放行）");
    assert!(n > 0, "对端在读响应前关闭（未回 -32001）");
    let mut rest = Vec::new();
    let eof = reader.read_to_end(&mut rest).expect("read_to_end") == 0;
    (line, eof)
}

/// 钉 1：令牌必须逐字节全长相等——前缀（长度差）与空串都必须 -32001 即断。
/// 杀变异：`ct_eq` 长度检查摘除（`a.iter().zip(b)` 只比短侧，前缀即通过认证）。
#[test]
fn token_prefix_or_empty_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let pf = dir.path().join("session.port");
    let _daemon = spawn_tcp(&["--db", db.to_str().unwrap()], &pf);
    let (port, token) = wait_port_file(&pf);
    assert_eq!(token.len(), 32, "token: {token}");

    // 真令牌的前缀（8 字符，短于 32）——长度检查摘除后 zip 折叠全 0 → 会被放行
    let prefix = &token[..8];
    let (line, eof) = try_auth(port, &format!(r#"{{"auth":"{prefix}"}}"#));
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["error"]["code"], -32001, "前缀令牌必须拒绝：{v}");
    assert!(eof, "前缀令牌被拒后必须断开");

    // 空令牌
    let (line, eof) = try_auth(port, r#"{"auth":""}"#);
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["error"]["code"], -32001, "空令牌必须拒绝：{v}");
    assert!(eof, "空令牌被拒后必须断开");

    // 全 0 全长令牌（对照组：长度相同、内容不同）
    let (line, _) = try_auth(port, r#"{"auth":"00000000000000000000000000000000"}"#);
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["error"]["code"], -32001, "等长错令牌必须拒绝：{v}");
}

/// 钉 2：`--listen` 白名单（仅回环）与成对约束都必须 exit 2。
/// 杀变异：① `is_loopback` 白名单删除（0.0.0.0 被放行并常驻）；② 不成对时静默退 stdio（exit 0）。
#[test]
fn non_loopback_or_unpaired_listen_args_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let pf = dir.path().join("session.port");

    // 非回环：deny by default —— 必须立即 exit 2，且不留下 port-file
    let mut child = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
        .args(["--listen", "0.0.0.0:0", "--port-file", pf.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("--listen 0.0.0.0:0 未被拒绝：进程仍存活（白名单被摘除）");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(2), "非回环监听必须 exit 2");
    assert!(!pf.exists(), "被拒的监听不得写 port-file");

    // 成对约束：只给 --listen / 只给 --port-file 都必须 exit 2（不得静默退 stdio 跑起来）
    for args in [
        vec!["--listen", "127.0.0.1:0"],
        vec!["--port-file", pf.to_str().unwrap()],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(2),
            "成对约束缺失：{args:?} stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("成对"),
            "stderr 应说明成对约束：{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// 钉 3：空行/纯空白行跳过而不产生 -32700——下一条可解析请求的响应必须是读到的第一条。
/// 杀变异：`serve_lines` 的 `line.trim().is_empty() { continue }` 摘除。
#[test]
fn blank_line_is_skipped_not_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let pf = dir.path().join("session.port");
    let _daemon = spawn_tcp(&["--db", db.to_str().unwrap()], &pf);
    let (port, token) = wait_port_file(&pf);

    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut w = stream.try_clone().unwrap();
    writeln!(w, r#"{{"auth":"{token}"}}"#).unwrap();
    // 握手后先发两行空行（含纯空白），再发 ping——空行不得占用任何一行响应
    writeln!(w).unwrap();
    writeln!(w, "   ").unwrap();
    writeln!(
        w,
        r#"{{"jsonrpc":"2.0","id":7,"method":"ping","params":null}}"#
    )
    .unwrap();
    w.flush().unwrap();

    let mut line = String::new();
    reader.read_line(&mut line).expect("读响应超时");
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        v["id"], 7,
        "空行被当作请求回错（-32700）插到 ping 响应之前：{v}"
    );
    assert_eq!(v["result"]["pong"], true, "{v}");
}

/// 钉 4：port-file 的 tmp+rename 必须「替换目标」而非「写穿目标」。
/// 杀变异：`write_port_file_impl` 改为直写目标路径（create/truncate，跟随符号链接）——
/// 预置符号链接即把 root 写原语引到任意受害文件（提权写任意用户可写路径）。
#[cfg(unix)]
#[test]
fn port_file_replaces_symlink_target_not_write_through() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, b"VICTIM-CONTENT").unwrap();
    let pf = dir.path().join("session.port");
    std::os::unix::fs::symlink(&victim, &pf).unwrap();

    let db = dir.path().join("t.db");
    let _daemon = spawn_tcp(&["--db", db.to_str().unwrap()], &pf);
    let (port, token) = wait_port_file(&pf); // rename 替换后照常可读
    assert!(port > 0 && token.len() == 32);

    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"VICTIM-CONTENT",
        "port-file 写穿了符号链接目标（提权 daemon 沦为任意路径写入器）"
    );
    let md = std::fs::symlink_metadata(&pf).unwrap();
    assert!(
        md.file_type().is_file(),
        "port-file 目标必须被 rename 原子替换为常规文件，而非仍是符号链接"
    );
}
