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
    /// 逐块写（`Write` 必选方法，不可省）：当前无直接调用者——广播走 `write_all`、响应走
    /// `write_line` 直接持锁。保留是为让本适配器的契约完整：凡经此端写出的都整段持锁
    /// （`write_vectored` 等默认方法仍会落到这里）。
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }

    /// **整段持锁**一次写完：默认 `write_all` 会按块反复调 `write`，每块取放一次连接锁——
    /// 慢读者部分写时，广播行会被该连接的响应行插断。广播走的正是这个入口
    /// （`Notifier::broadcast` → `Box<dyn Write>::write_all`）。
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.0.lock().unwrap().write_all(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }

    /// 格式化写：当前同样无调用路径（响应侧 `write_line` 直接持锁写），**保留为纵深**——
    /// 若将来有写者改为经本端格式化写，其多个 write 片段仍整段持锁；删掉则默认实现按块
    /// 取放锁，会静默复活本轮修掉的行插断形态（qual-m1e-t1 观察④ 裁定：留）。
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    /// 每次 `write` 只吃 1 字节的写端：强制分块，把「部分写」窗口放到最大。
    struct Chunky {
        out: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for Chunky {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = buf.len().min(1);
            self.out.lock().unwrap().extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 并发「广播行 × 响应行」共用一条连接写端：任一行被插断即成坏行（JSON 不可解析）。
    ///
    /// 钉法（确定性判据）：第三个线程不停 `try_lock` 连接锁，**持锁时刻输出必须停在行边界**。
    /// 修复后（`SharedSink::write_all` 整段持锁）该判据是恒真式——行未写完时锁始终在写者手里，
    /// 观测者只能在整行落定后取到锁，故本测试不可能因调度而假红；未修复时（默认 `write_all`
    /// 按块取放锁）观测者能在两块之间取到锁并看到半行，即被抓住（钉测有效性见提交报告：
    /// 临时摘除 `write_all` 覆写实跑为红）。
    #[test]
    fn broadcast_and_response_lines_never_interleave() {
        const LINES: usize = 40;
        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let conn = Arc::new(Mutex::new(Chunky { out: out.clone() }));
        let notifier = Arc::new(Notifier::new());
        let _sink = notifier.register(Box::new(SharedSink(conn.clone())));

        let done = AtomicBool::new(false);
        let interleaved: Mutex<Option<String>> = Mutex::new(None);

        std::thread::scope(|s| {
            let broadcaster = s.spawn(|| {
                for i in 0..LINES {
                    notifier.broadcast(&serde_json::json!({"n": i}));
                }
            });
            let responder = s.spawn(|| {
                for i in 0..LINES {
                    write_line(&conn, &serde_json::json!({"resp": i}));
                }
            });
            let watcher = s.spawn(|| {
                while !done.load(Ordering::SeqCst) {
                    if let Ok(_guard) = conn.try_lock() {
                        let seen = out.lock().unwrap();
                        if seen.last().is_some_and(|b| *b != b'\n') {
                            *interleaved.lock().unwrap() =
                                Some(String::from_utf8_lossy(&seen).into_owned());
                            return;
                        }
                    }
                    std::thread::yield_now();
                }
            });
            broadcaster.join().unwrap();
            responder.join().unwrap();
            done.store(true, Ordering::SeqCst);
            watcher.join().unwrap();
        });

        let partial = interleaved.lock().unwrap().take();
        assert!(
            partial.is_none(),
            "持锁时输出停在半行（广播/响应行被插断）：\n{}",
            partial.unwrap_or_default()
        );

        // 终局校验：整段输出逐行可解析，两条序列各 LINES 行且以 `\n` 收尾。
        let bytes = out.lock().unwrap().clone();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.ends_with('\n'), "输出必须以整行收尾：{text:?}");
        let (mut broadcasts, mut responses) = (0, 0);
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("坏行（被插断）{line:?}: {e}"));
            match v.get("n").is_some() {
                true => broadcasts += 1,
                false => {
                    assert!(v.get("resp").is_some(), "未知行 {v}");
                    responses += 1;
                }
            }
        }
        assert_eq!(
            (broadcasts, responses),
            (LINES, LINES),
            "行数与写入次数不符（丢行或合并行）：{text:?}"
        );
    }

    // qual-m1e-t1 补测（最小集）：封住 transport.rs 内两条未钉住的生命周期行为。
    // 两个钉子与对应变异：
    // 1) `sink_handle_drop_unregisters` —— 杀「SinkHandle::drop 注销摘除（no-op）」。
    // 2) `failed_sink_is_evicted_without_handle_drop` —— 杀「broadcast 写失败不剔除（吞错继续写）」。

    /// 恒失败的写端：模拟 TCP 写端断开（EPIPE）而读线程尚未退出的窗口。
    struct DeadSink;

    impl Write for DeadSink {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "dead sink",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 计数写端：每次 `write` 计数 +1，全部收下（一次 broadcast 恰好一次调用）。
    #[derive(Clone)]
    struct CountSink(std::sync::Arc<AtomicUsize>);

    impl Write for CountSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// `SinkHandle` drop ⇒ 立刻从广播集摘除（TCP 连接断开的主注销路径）。
    #[test]
    fn sink_handle_drop_unregisters() {
        let notifier = Arc::new(Notifier::new());
        let count = std::sync::Arc::new(AtomicUsize::new(0));
        let handle = notifier.register(Box::new(CountSink(count.clone())));

        notifier.broadcast(&serde_json::json!({"n": 1}));
        let after_first = count.load(Ordering::SeqCst);
        assert!(after_first > 0, "注册后广播必须到达写端");
        let registered = notifier.sinks.lock().unwrap().len();
        assert_eq!(registered, 1, "register 后应在册");

        drop(handle);
        // 注意：先把持锁读数落成局部量，再断言——断言失败若与持锁临时量同语句，
        // mutex 中毒会让 SinkHandle::drop 的 unwrap 二次 panic（abort 而非干净失败）。
        let registered = notifier.sinks.lock().unwrap().len();
        assert_eq!(registered, 0, "SinkHandle drop 必须注销");
        notifier.broadcast(&serde_json::json!({"n": 2}));
        assert_eq!(
            count.load(Ordering::SeqCst),
            after_first,
            "drop 后不得再收到广播（注销失败 = 死写端常驻）"
        );
    }

    /// 写失败 ⇒ 移除该写端且不影响其余写端（读线程未察觉断开时的兜底剔除路径）。
    #[test]
    fn failed_sink_is_evicted_without_handle_drop() {
        let notifier = Arc::new(Notifier::new());
        let _dead = notifier.register(Box::new(DeadSink)); // 句柄存活：只允许经写失败剔除
        let count = std::sync::Arc::new(AtomicUsize::new(0));
        let _live = notifier.register(Box::new(CountSink(count.clone())));

        notifier.broadcast(&serde_json::json!({"n": 1}));
        let registered = notifier.sinks.lock().unwrap().len();
        assert_eq!(registered, 1, "写失败端必须被剔除（不依赖句柄 drop）");
        notifier.broadcast(&serde_json::json!({"n": 2}));
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "存活写端两次广播都必须收到（剔除不得误伤）"
        );
    }
}
