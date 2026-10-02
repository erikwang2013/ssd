// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

struct Daemon {
    child: Arc<Mutex<Child>>,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Daemon {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let child = Arc::new(Mutex::new(child));
        // watchdog：daemon 活着但不回话时 30 秒杀进程，避免测试挂死（CI 上会长挂而非失败）
        let watchdog = Arc::clone(&child);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(30));
            if let Ok(mut c) = watchdog.lock() {
                let _ = c.kill();
            }
        });
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn call(&mut self, line: &str) -> serde_json::Value {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
        let mut buf = String::new();
        self.stdout.read_line(&mut buf).unwrap();
        serde_json::from_str(buf.trim()).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[test]
fn ping_over_stdio() {
    let mut d = Daemon::start(&[]);
    let resp = d.call(r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":null}"#);
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["pong"], true);
    assert_eq!(resp["result"]["protocol"], 0);
}

#[test]
fn device_list_with_image() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&[0u8; 4096]).unwrap();
    f.flush().unwrap();
    let path = f.path().to_str().unwrap().to_string();
    let mut d = Daemon::start(&["--image", &path]);
    let resp = d.call(r#"{"jsonrpc":"2.0","id":2,"method":"device.list","params":null}"#);
    let devices = resp["result"]["devices"].as_array().unwrap();
    // M1e 起 Linux 启动时枚举物理盘，总数依宿主而异；断言收敛为「镜像设备在列且正确」
    let imgs: Vec<_> = devices
        .iter()
        .filter(|d| d["kind"] == serde_json::json!("image"))
        .collect();
    assert_eq!(imgs.len(), 1);
    assert_eq!(imgs[0]["sizeBytes"], 4096);
}

#[test]
fn unknown_method_returns_error_with_same_id() {
    let mut d = Daemon::start(&[]);
    let resp = d.call(r#"{"jsonrpc":"2.0","id":9,"method":"scan.start","params":null}"#);
    assert_eq!(resp["id"], 9);
    assert_eq!(resp["error"]["code"], -32601);
}

#[test]
fn malformed_line_does_not_kill_stream() {
    let mut d = Daemon::start(&[]);
    let resp = d.call("not json at all");
    assert_eq!(resp["error"]["code"], -32700);
    assert_eq!(resp["error"]["message"], "Parse error");
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], serde_json::Value::Null);
    // 流仍然存活
    let resp = d.call(r#"{"jsonrpc":"2.0","id":11,"method":"ping","params":null}"#);
    assert_eq!(resp["id"], 11);
    assert_eq!(resp["result"]["pong"], true);
}

#[test]
fn arg_errors_exit_with_code_2() {
    // 未知参数
    let out = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
        .arg("--bogus")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown argument"));

    // --image 缺路径
    let out = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
        .arg("--image")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires a path"));

    // --image 指向不存在的文件
    let out = Command::new(env!("CARGO_BIN_EXE_xd-daemon"))
        .args(["--image", "/nonexistent/xiaodun-test.img"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot open image"));
}
