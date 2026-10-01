use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Daemon {
    child: Child,
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
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["kind"], "image");
    assert_eq!(devices[0]["sizeBytes"], 4096);
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
    assert_eq!(resp["id"], serde_json::Value::Null);
    // 流仍然存活
    let resp = d.call(r#"{"jsonrpc":"2.0","id":11,"method":"ping","params":null}"#);
    assert_eq!(resp["id"], 11);
    assert_eq!(resp["result"]["pong"], true);
}
