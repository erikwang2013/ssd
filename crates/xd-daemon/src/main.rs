//! 小盾桌面特权进程：stdio JSON-RPC 服务（每行一条 JSON，见 proto/v0/README.md）。
//! 提权与设备枚举在 M1/M2 接入；M0 只支持 --image 注册镜像设备。

use std::io::{BufRead, Write};
use std::path::PathBuf;

use xd_core::api::{Request, Response, RpcErr, RpcError};
use xd_core::handlers::{CoreCtx, handle_request};
use xd_device::BlockDevice;
use xd_device::image::ImageFileDevice;

fn main() {
    let mut devices: Vec<Box<dyn BlockDevice>> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--image" => {
                let Some(path) = args.next() else {
                    eprintln!("error: --image requires a path");
                    std::process::exit(2);
                };
                match ImageFileDevice::open(&PathBuf::from(&path)) {
                    Ok(dev) => devices.push(Box::new(dev)),
                    Err(e) => {
                        eprintln!("error: cannot open image {path}: {e}");
                        std::process::exit(2);
                    }
                }
            }
            other => {
                eprintln!("error: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }

    let ctx = CoreCtx::new(devices);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle_request(&ctx, &req),
            Err(_) => Response::Err(RpcErr {
                jsonrpc: "2.0".into(),
                id: serde_json::Value::Null,
                error: RpcError::parse_error(),
            }),
        };
        if writeln!(stdout, "{}", serde_json::to_string(&response).unwrap()).is_err() {
            break;
        }
        let _ = stdout.flush();
    }
}
