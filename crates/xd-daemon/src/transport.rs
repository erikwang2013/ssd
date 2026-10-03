// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 传输层：stdio（缺省）与 TCP 回环（提权会话）。通知广播与逐行 JSON-RPC 循环在此，
//! stdio 与 TCP 共用同一份实现。
//!
//! **TCP 协议（三平台一致）**：daemon `--listen 127.0.0.1:0 --port-file F`：绑定回环随机端口
//! → 生成令牌 → 原子写 F（一行 `<port> <token>`，unix 0600）→ stderr 打印就绪。每条连接
//! **首行必须是 `{"auth":"<token>"}`**，否则回 -32001 并立即断开；通过后该连接按 stdio 同款
//! 逐行 JSON-RPC 处理。令牌比较常数时间。**M1 简化：通知广播给所有已认证连接**（连接路由/
//! 订阅模型归 M2，见 docs/security 提权会话传输节）。
//!
//! 注意：提权（root）daemon 的监听面**仅回环 + 令牌**——异用户读不到 0600 令牌文件即无法接入；
//! 同用户攻击者本就有 uaccess 直读设备权限（无提权增益）。stdout 仅协议：TCP 模式下 stdout
//! 不写任何东西，诊断全 stderr。

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use xd_core::api::{Request, Response, RpcErr, RpcError};
use xd_core::handlers::{CoreCtx, handle_request};

use crate::portfile::{random_token, write_port_file};

/// `--listen`/`--port-file` 成对给出的 TCP 模式参数。
pub(crate) struct TcpOptions {
    pub(crate) addr: SocketAddr,
    pub(crate) port_file: std::path::PathBuf,
}

/// TCP 服务循环（不返回）：绑定 → 写 port-file → 每连接一线程。
pub(crate) fn serve_tcp(opts: TcpOptions, ctx: Arc<CoreCtx>, notifier: Arc<Notifier>) -> ! {
    // 先 bind 再写 port-file：`--listen 127.0.0.1:0` 时端口由内核选定，port-file 必须记
    // 实际绑定端口（计划代码骨架里的 `opts.addr.port()` 会在 :0 时写入 0——以计划正文
    // 「绑定回环随机端口 → 生成令牌 → 原子写 F」为准）。
    let listener = TcpListener::bind(opts.addr).unwrap_or_else(|e| {
        eprintln!("error: cannot bind {}: {e}", opts.addr);
        std::process::exit(2);
    });
    let port = listener.local_addr().expect("local_addr").port();
    let token = random_token();
    if let Err(e) = write_port_file(&opts.port_file, port, &token) {
        eprintln!(
            "error: cannot write port file {}: {e}",
            opts.port_file.display()
        );
        std::process::exit(2);
    }
    eprintln!("xd-daemon: listening on 127.0.0.1:{port}");
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let (token, ctx, notifier) = (token.clone(), ctx.clone(), notifier.clone());
        std::thread::spawn(move || handle_conn(stream, &token, &ctx, &notifier));
    }
    unreachable!()
}

/// 单条连接：认证首行 → 逐行 JSON-RPC（与 stdio 同款）。断开/认证失败即注销通知写端。
fn handle_conn(stream: TcpStream, token: &str, ctx: &CoreCtx, notifier: &Arc<Notifier>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let writer = Arc::new(Mutex::new(stream));
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let ok = serde_json::from_str::<serde_json::Value>(&first)
        .ok()
        .and_then(|v| {
            v["auth"]
                .as_str()
                .map(|t| ct_eq(t.as_bytes(), token.as_bytes()))
        })
        .unwrap_or(false);
    if !ok {
        write_line(
            &writer,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {"code": -32001, "message": "Authentication failed"}
            }),
        );
        return; // 失败即断开（不读后续）
    }
    // 认证通过才注册广播写端：与响应写出共用同一把锁 ⇒ 通知行不会插进响应行中间。
    let _sink = notifier.register(Box::new(SharedSink(writer.clone())));
    serve_lines(reader, ctx, &writer);
}

/// 常数时间比较（长度差直接 false；M1 令牌恒定 32 字符）。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 逐行 JSON-RPC 主循环（stdio 与 TCP 共用）：解析 → handle_request → 同一写端出响应。
/// 写出失败（下游关闭）不退出：stdio 随 stdin EOF 结束，TCP 随连接 EOF 结束。
pub(crate) fn serve_lines<W: Write>(reader: impl BufRead, ctx: &CoreCtx, writer: &Mutex<W>) {
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                eprintln!("error: read failed: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle_request(ctx, &req),
            Err(_) => Response::Err(RpcErr {
                jsonrpc: "2.0".into(),
                id: serde_json::Value::Null,
                error: RpcError::parse_error(),
            }),
        };
        write_line(writer, &serde_json::to_value(&response).unwrap());
    }
}

/// 唯一写口（响应与通知共用；`Mutex` 串行化整行输出 + flush）。
pub(crate) fn write_line<W: Write>(out: &Mutex<W>, v: &serde_json::Value) {
    let mut w = out.lock().unwrap();
    let _ = writeln!(w, "{v}");
    let _ = w.flush();
}

/// 共享写端适配：同一把锁既服务该连接的响应写出又服务通知广播。
pub(crate) struct SharedSink<W>(pub(crate) Arc<Mutex<W>>);

impl<W: Write> Write for SharedSink<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }

    /// 整行（含格式化的多个 write 片段）持锁一次写完：广播行不会被另一线程的半行插断。
    fn write_fmt(&mut self, args: std::fmt::Arguments<'_>) -> std::io::Result<()> {
        self.0.lock().unwrap().write_fmt(args)
    }
}

/// 通知广播器：stdio 会话与各 TCP 连接各注册一个写端；`NotifyFn` 语义不变（仍是
/// `Arc<dyn Fn(Value)>`，内部遍历写）。计划裁定：`out` 由 `Arc<Mutex<Stdout>>` 升级为本类型。
pub(crate) struct Notifier {
    sinks: Mutex<Vec<(u64, Box<dyn Write + Send>)>>,
    next_id: AtomicU64,
}

impl Notifier {
    pub(crate) fn new() -> Self {
        Self {
            sinks: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// 注册一个通知写端；返回句柄 drop 即注销（TCP 连接断开路径）。
    pub(crate) fn register(self: &Arc<Self>, w: Box<dyn Write + Send>) -> SinkHandle {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.sinks.lock().unwrap().push((id, w));
        SinkHandle {
            id,
            notifier: self.clone(),
        }
    }

    /// 广播一行 JSON 到全部写端；写失败者剔除。
    /// ponytail: 持全局锁逐端写——慢读者（TCP 缓冲写满）会拖住其余连接；M2 做连接路由时
    /// 一并改每端独立缓冲/线程。
    pub(crate) fn broadcast(&self, v: &serde_json::Value) {
        let line = format!("{v}\n");
        let mut sinks = self.sinks.lock().unwrap();
        sinks.retain_mut(|(_, w)| {
            w.write_all(line.as_bytes())
                .and_then(|()| w.flush())
                .is_ok()
        });
    }
}

/// 注册句柄：drop 时从广播集移除（幂等——写失败已在 retain 中剔除）。
pub(crate) struct SinkHandle {
    id: u64,
    notifier: Arc<Notifier>,
}

impl Drop for SinkHandle {
    fn drop(&mut self) {
        self.notifier
            .sinks
            .lock()
            .unwrap()
            .retain(|(i, _)| *i != self.id);
    }
}
