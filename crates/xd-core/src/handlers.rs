// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! RPC 处理器：daemon（stdio）与 M4 的 ffi 共用同一入口。

use std::path::Path;
use std::sync::Arc;

use xd_device::{BlockDevice, DeviceInfo};

use crate::api::{
    ExportIdParams, ExportStartParams, FsReadParams, PROTOCOL_VERSION, Request, Response, RpcError,
    ScanResultsParams, ScanStartParams, TaskIdParams, err, ok,
};
use crate::export::{ExportError, ExportManager, MAX_IDXS, dedupe_idxs};
use crate::fs_read::{MAX_PREVIEW, MAX_READ, ReadError, read_entry_range, to_base64};
use crate::scan_task::{
    DeviceOpener, FsKind, NoopOpener, OpenError, Resume, ScanError, ScanManager,
};

pub struct CoreCtx {
    devices: Vec<Arc<dyn BlockDevice>>,
    /// 枚举到但**未打开**的设备（device.list 用；零 open()——M1e 契约要求）。
    list_only: Vec<DeviceInfo>,
    scans: Arc<ScanManager>,
    opener: Arc<dyn DeviceOpener>,
    /// 恢复导出（父侧）。默认内存库实例：export.start 诚实回 -32603（无 --db 无从起子进程）。
    exports: Arc<ExportManager>,
}

impl CoreCtx {
    pub fn new(devices: Vec<Arc<dyn BlockDevice>>) -> Self {
        Self {
            devices,
            list_only: Vec::new(),
            scans: Arc::new(ScanManager::new(
                crate::store::Store::open_memory().expect("sqlite in-memory"),
                Arc::new(|_| {}),
            )),
            opener: Arc::new(NoopOpener),
            exports: Arc::new(ExportManager::new(
                Arc::new(crate::store::Store::open_memory().expect("sqlite in-memory")),
                Arc::new(|_| {}),
                None,
            )),
        }
    }

    pub fn with_list_only(mut self, infos: Vec<DeviceInfo>) -> Self {
        self.list_only = infos;
        self
    }

    /// daemon 注入：真 store（文件/内存）+ stdout 通知通道 + 平台 opener。
    pub fn with_scan(mut self, scans: Arc<ScanManager>, opener: Arc<dyn DeviceOpener>) -> Self {
        self.scans = scans;
        self.opener = opener;
        self
    }

    /// daemon 注入：导出管理器（共享同一 store；`--db` 文件路径供子进程只读打开）。
    /// 选择「最小 with_export」而非改造 `with_scan` 签名：既有调用点零改动。
    pub fn with_export(mut self, exports: Arc<ExportManager>) -> Self {
        self.exports = exports;
        self
    }

    /// 解析 scan.start/resume 的设备：已打开优先；否则懒打开（唯一 open 出口）。
    fn resolve_device(&self, id: &str) -> Result<Arc<dyn BlockDevice>, RpcError> {
        if let Some(d) = self.devices.iter().find(|d| d.info().id == id) {
            return Ok(d.clone());
        }
        match self.opener.open(id) {
            Ok(d) => Ok(d),
            Err(OpenError::PermissionDenied) => Err(RpcError::device_permission(id)),
            Err(OpenError::Other(_)) => Err(RpcError::cannot_open(id)),
        }
    }

    /// 双向 first-wins 去重：打开项在前，同 id 只留首个（含打开项内部双开——
    /// `--device /dev/sda --device /dev/disk/by-id/…` canonicalize 同 id）；
    /// `list_only` 与已打开 id 重复的同样丢弃，否则同一块盘在 device.list 出现两次（qual-t1 I1 / qual-t2 Minor 5）。
    pub fn device_infos(&self) -> Vec<DeviceInfo> {
        let mut v: Vec<DeviceInfo> = Vec::new();
        for info in self.devices.iter().map(|d| d.info()).chain(&self.list_only) {
            if !v.iter().any(|e| e.id == info.id) {
                v.push(info.clone());
            }
        }
        v
    }
}

pub fn handle_request(ctx: &CoreCtx, req: &Request) -> Response {
    match req.method.as_str() {
        "ping" => ok(
            req,
            serde_json::json!({
                "pong": true,
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": PROTOCOL_VERSION,
            }),
        ),
        "device.list" => ok(req, serde_json::json!({ "devices": ctx.device_infos() })),
        "scan.start" => scan_start(ctx, req),
        "scan.status" => scan_status(ctx, req),
        "scan.results" => scan_results(ctx, req),
        "scan.pause" => scan_pause(ctx, req),
        "scan.resume" => scan_resume(ctx, req),
        "scan.cancel" => scan_cancel(ctx, req),
        "fs.read" => fs_read(ctx, req),
        "export.start" => export_start(ctx, req),
        "export.cancel" => export_cancel(ctx, req),
        other => err(req, RpcError::method_not_found(other)),
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(req: &Request) -> Result<T, Response> {
    // 注意：`RpcError::invalid_params(&str)` 是 M0 既有签名（输出 "Invalid params: {message}" 前缀）
    match req.params.clone() {
        Some(v) if !v.is_null() => serde_json::from_value(v)
            .map_err(|_| err(req, RpcError::invalid_params("missing or malformed params"))),
        _ => Err(err(
            req,
            RpcError::invalid_params("missing or malformed params"),
        )),
    }
}

fn scan_err(req: &Request, e: ScanError) -> Response {
    match e {
        ScanError::TaskNotFound(id) => err(req, RpcError::task_not_found(id)),
        ScanError::TaskNotActive(id) => err(req, RpcError::task_not_active(id)),
        ScanError::UnsupportedFs => err(req, RpcError::unsupported_fs()),
        ScanError::UnallocatedUnavailable => err(req, RpcError::unallocated_unavailable()),
        _ => err(req, RpcError::internal()),
    }
}

fn scan_start(ctx: &CoreCtx, req: &Request) -> Response {
    let p: ScanStartParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    // v1.1：mode ∈ {None→quick, "quick", "deep"}；其它值 -32602（未知模式不得静默降级）
    let mode = p.mode.as_deref().unwrap_or("quick");
    if mode != "quick" && mode != "deep" {
        return err(req, RpcError::invalid_params("unsupported mode"));
    }
    let dev = match ctx.resolve_device(&p.device) {
        Ok(d) => d,
        Err(e) => return err(req, e),
    };
    let started = if mode == "deep" {
        ctx.scans.start_deep(dev)
    } else {
        ctx.scans.start(dev)
    };
    match started {
        Ok(s) => ok(
            req,
            serde_json::json!({
                "taskId": s.task_id, "fs": s.fs.as_str(), "totalBytes": s.total_bytes,
            }),
        ),
        Err(e) => scan_err(req, e),
    }
}

fn scan_status(ctx: &CoreCtx, req: &Request) -> Response {
    let p: TaskIdParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match ctx.scans.status(p.task_id) {
        Ok(t) => ok(
            req,
            serde_json::json!({
                "taskId": t.id, "state": crate::store::state_str(t.state),
                "readBytes": t.read_bytes, "foundCount": t.found_count, "elapsedMs": t.elapsed_ms,
            }),
        ),
        Err(e) => scan_err(req, e),
    }
}

fn scan_results(ctx: &CoreCtx, req: &Request) -> Response {
    let p: ScanResultsParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !(1..=1000).contains(&p.limit) {
        return err(req, RpcError::invalid_params("limit out of range 1..=1000")); // 契约
    }
    match ctx
        .scans
        .results(p.task_id, p.offset, p.limit, p.deleted_only)
    {
        Ok((total, entries)) => ok(
            req,
            serde_json::json!({ "total": total, "entries": entries }),
        ),
        Err(e) => scan_err(req, e),
    }
}

fn scan_pause(ctx: &CoreCtx, req: &Request) -> Response {
    let p: TaskIdParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match ctx.scans.pause(p.task_id) {
        Ok(()) => ok(
            req,
            serde_json::json!({ "taskId": p.task_id, "state": "paused" }),
        ),
        Err(e) => scan_err(req, e),
    }
}

fn scan_resume(ctx: &CoreCtx, req: &Request) -> Response {
    let p: TaskIdParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match ctx.scans.resume(p.task_id) {
        Ok(Resume::InPlace) => ok(
            req,
            serde_json::json!({ "taskId": p.task_id, "state": "scanning" }),
        ),
        Ok(Resume::NeedsDevice { device_id }) => {
            let dev = match ctx.resolve_device(&device_id) {
                Ok(d) => d,
                Err(e) => return err(req, e),
            };
            match ctx.scans.restart(p.task_id, dev) {
                Ok(()) => ok(
                    req,
                    serde_json::json!({ "taskId": p.task_id, "state": "scanning" }),
                ),
                Err(e) => scan_err(req, e),
            }
        }
        Err(e) => scan_err(req, e),
    }
}

fn scan_cancel(ctx: &CoreCtx, req: &Request) -> Response {
    let p: TaskIdParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match ctx.scans.cancel(p.task_id) {
        Ok(()) => ok(
            req,
            serde_json::json!({ "taskId": p.task_id, "state": "canceled" }),
        ),
        Err(e) => scan_err(req, e),
    }
}

/// 导出错误 → 契约错误码（含 `export.cancel` 未知 id 的 -32602 裁定，见 proto/v1/README v1.2）。
fn export_err(req: &Request, e: ExportError) -> Response {
    match e {
        ExportError::TaskNotFound(id) => err(req, RpcError::task_not_found(id)),
        ExportError::EntryNotFound(idx) => err(req, RpcError::entry_not_found(idx)),
        ExportError::NoEntries => err(req, RpcError::invalid_params("idxs is empty after dedupe")),
        ExportError::ExportNotFound(id) => err(
            req,
            RpcError::invalid_params(&format!("unknown exportId: {id}")),
        ),
        ExportError::TargetOnSource(d) => err(req, RpcError::target_on_source(&d)),
        ExportError::TargetNotWritable(d) => err(req, RpcError::target_not_writable(&d)),
        ExportError::InsufficientSpace(need) => err(req, RpcError::insufficient_space(need)),
        // 平台不支持（非 unix 的 Windows 臂；M1d 只要求编译/矩阵，运行时归 M1e-tail）：契约无对应码，
        // 归 -32603 并**留痕**——静默成功会掩盖「同盘校验被跳过/取消没生效」。
        ExportError::PlatformUnsupported(msg) => {
            eprintln!("warn: export platform unsupported: {msg}");
            err(req, RpcError::internal())
        }
        ExportError::Internal(msg) => {
            eprintln!("warn: export internal: {msg}");
            err(req, RpcError::internal())
        }
    }
}

fn fs_read(ctx: &CoreCtx, req: &Request) -> Response {
    let p: FsReadParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !(1..=MAX_READ).contains(&p.length) {
        return err(
            req,
            RpcError::invalid_params("length out of range 1..=1048576"),
        ); // 契约
    }
    let row = match ctx.scans.status(p.task_id) {
        Ok(t) => t,
        Err(e) => return scan_err(req, e),
    };
    let entry = match ctx.scans.entry(p.task_id, p.idx) {
        Ok(Some(e)) => e,
        Ok(None) => return err(req, RpcError::entry_not_found(p.idx)),
        Err(e) => {
            eprintln!("warn: fs.read entry lookup failed: {e}");
            return err(req, RpcError::internal());
        }
    };
    if entry.size_bytes > MAX_PREVIEW {
        return err(req, RpcError::entry_too_large(entry.size_bytes)); // -32009（导出不受此限）
    }
    let Some(fs) = row.fs.parse::<FsKind>().ok() else {
        eprintln!("warn: fs.read unknown fs in task row: {}", row.fs);
        return err(req, RpcError::internal());
    };
    let dev = match ctx.resolve_device(&row.device_id) {
        Ok(d) => d,
        Err(e) => return err(req, e),
    };
    match read_entry_range(&*dev, fs, &entry, p.offset, p.length) {
        Ok((bytes, eof)) => ok(
            req,
            serde_json::json!({ "bytesBase64": to_base64(&bytes), "eof": eof }),
        ),
        Err(ReadError::TooLarge(size)) => err(req, RpcError::entry_too_large(size)),
        Err(ReadError::Internal(msg)) => {
            eprintln!("warn: fs.read failed: {msg}");
            err(req, RpcError::internal())
        }
    }
}

fn export_start(ctx: &CoreCtx, req: &Request) -> Response {
    let p: ExportStartParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !Path::new(&p.target_dir).is_absolute() {
        return err(
            req,
            RpcError::invalid_params("targetDir must be an absolute path"),
        );
    }
    // 契约：idxs 去重后非空且 ≤100000（manager 侧另有纵深防御，见 ExportManager::start）
    let idxs = dedupe_idxs(&p.idxs);
    if idxs.is_empty() {
        return err(req, RpcError::invalid_params("idxs is empty after dedupe"));
    }
    if idxs.len() > MAX_IDXS {
        return err(
            req,
            RpcError::invalid_params("idxs exceeds 100000 after dedupe"),
        );
    }
    let row = match ctx.scans.status(p.task_id) {
        Ok(t) => t,
        Err(e) => return scan_err(req, e),
    };
    // source_rdev 由已解析设备给出（镜像/未知 → None：不做同盘校验，契约 v1.2）
    let dev = match ctx.resolve_device(&row.device_id) {
        Ok(d) => d,
        Err(e) => return err(req, e),
    };
    match ctx
        .exports
        .start(p.task_id, &idxs, &p.target_dir, dev.source_rdev())
    {
        Ok(s) => ok(
            req,
            serde_json::json!({
                "exportId": s.export_id, "fileCount": s.file_count,
                "estimatedBytes": s.estimated_bytes,
            }),
        ),
        Err(e) => export_err(req, e),
    }
}

fn export_cancel(ctx: &CoreCtx, req: &Request) -> Response {
    let p: ExportIdParams = match parse_params(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match ctx.exports.cancel(p.export_id) {
        Ok(state) => ok(
            req,
            serde_json::json!({ "exportId": p.export_id, "state": state }),
        ),
        Err(e) => export_err(req, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use xd_device::image::ImageFileDevice;

    fn req(id: i64, method: &str) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(id),
            method: method.into(),
            params: None,
        }
    }

    #[test]
    fn ping_returns_pong_and_protocol() {
        let ctx = CoreCtx::new(vec![]);
        let resp = handle_request(&ctx, &req(1, "ping"));
        let Response::Ok(ok) = resp else {
            panic!("expected Ok")
        };
        assert_eq!(ok.id, serde_json::json!(1));
        assert_eq!(ok.result["pong"], serde_json::json!(true));
        assert_eq!(ok.result["protocol"], serde_json::json!(PROTOCOL_VERSION));
        assert_eq!(
            ok.result["version"],
            serde_json::json!(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn device_list_empty() {
        let ctx = CoreCtx::new(vec![]);
        let Response::Ok(ok) = handle_request(&ctx, &req(2, "device.list")) else {
            panic!()
        };
        assert_eq!(ok.result["devices"], serde_json::json!([]));
    }

    #[test]
    fn device_list_includes_image() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&[0u8; 4096]).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        let ctx = CoreCtx::new(vec![Arc::new(dev)]);
        let Response::Ok(ok) = handle_request(&ctx, &req(3, "device.list")) else {
            panic!()
        };
        let devices = ok.result["devices"].as_array().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["kind"], serde_json::json!("image"));
        assert_eq!(devices[0]["sizeBytes"], serde_json::json!(4096));
    }

    #[test]
    fn device_list_merges_list_only_after_opened() {
        use xd_device::{DeviceInfo, DeviceKind};
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&[0u8; 512]).unwrap();
        let img = ImageFileDevice::open(f.path()).unwrap();
        let ctx = CoreCtx::new(vec![Arc::new(img)]).with_list_only(vec![DeviceInfo {
            id: "unix:/dev/sda".into(),
            name: "Disk".into(),
            kind: DeviceKind::Physical,
            size_bytes: 1 << 40,
            removable: false,
            fs_guess: None,
            transport: None,
        }]);
        let infos = ctx.device_infos();
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].kind, DeviceKind::Image); // devices 在前（既有顺序不变）
        assert_eq!(infos[1].id, "unix:/dev/sda");
    }

    #[test]
    fn device_list_dedupes_by_id() {
        use xd_device::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};
        struct Stub;
        impl BlockDevice for Stub {
            fn info(&self) -> &DeviceInfo {
                static I: std::sync::OnceLock<DeviceInfo> = std::sync::OnceLock::new();
                I.get_or_init(|| DeviceInfo {
                    id: "unix:/dev/sda".into(),
                    name: "opened".into(),
                    kind: DeviceKind::Physical,
                    size_bytes: 42,
                    removable: false,
                    fs_guess: None,
                    transport: None,
                })
            }
            fn read_at(&self, _o: u64, _b: &mut [u8]) -> Result<usize, DeviceError> {
                Ok(0)
            }
        }
        let ctx = CoreCtx::new(vec![Arc::new(Stub)]).with_list_only(vec![DeviceInfo {
            id: "unix:/dev/sda".into(), // 与打开项同 id → 必须被去重
            name: "enumerated".into(),
            kind: DeviceKind::Physical,
            size_bytes: 42,
            removable: false,
            fs_guess: None,
            transport: None,
        }]);
        let infos = ctx.device_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "opened");
    }

    #[test]
    fn device_list_dedupes_double_open_same_id() {
        // 同一物理盘经两条路径双开（/dev/sda 与 /dev/disk/by-id/… canonicalize 同 id）→ 只留首个
        use xd_device::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};
        struct S(DeviceInfo);
        impl BlockDevice for S {
            fn info(&self) -> &DeviceInfo {
                &self.0
            }
            fn read_at(&self, _o: u64, _b: &mut [u8]) -> Result<usize, DeviceError> {
                Ok(0)
            }
        }
        let mk = |name: &str| -> Arc<dyn BlockDevice> {
            Arc::new(S(DeviceInfo {
                id: "unix:/dev/sda".into(),
                name: name.into(),
                kind: DeviceKind::Physical,
                size_bytes: 42,
                removable: false,
                fs_guess: None,
                transport: None,
            }))
        };
        let infos = CoreCtx::new(vec![mk("first"), mk("second")]).device_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "first");
    }

    #[test]
    fn unknown_method_returns_minus_32601() {
        // M1b：`scan.start` 已是合法方法（无 params 会走 -32602）——本用例钉真正不存在的方法名。
        let ctx = CoreCtx::new(vec![]);
        let Response::Err(e) = handle_request(&ctx, &req(7, "no.such.method")) else {
            panic!()
        };
        assert_eq!(e.id, serde_json::json!(7));
        assert_eq!(e.error.code, -32601);
        assert_eq!(e.error.message, "Method not found: no.such.method");
    }

    #[test]
    fn ping_round_trip_matches_response_golden() {
        // 走真实输出路径：请求 golden → handle_request → 与响应 golden 全等（含 jsonrpc 字段）。
        // version 是随发布变动的动态值：golden 存 "<VERSION>" 占位，先把实际值归一为占位再全等比对，
        // 然后单独断言实际值 == 本 crate 版本——golden 跨发布稳定，发版不再改契约文件（v0.2.0 注）。
        // M1b：当前协议为 v1（protocol=1），改指 proto/v1 golden；v0 golden 封存不再参与本断言。
        let ctx = CoreCtx::new(vec![]);
        let req: Request = serde_json::from_str(
            include_str!("../../../proto/v1/examples/ping.request.json").trim(),
        )
        .unwrap();
        let resp = handle_request(&ctx, &req);
        let mut actual = serde_json::to_value(&resp).unwrap();
        assert_eq!(
            actual["result"]["version"],
            serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string())
        );
        actual["result"]["version"] = serde_json::Value::String("<VERSION>".into());
        let expected: serde_json::Value = serde_json::from_str(
            include_str!("../../../proto/v1/examples/ping.response.json").trim(),
        )
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn error_round_trip_matches_response_golden() {
        // err() 输出路径同样钉死（jsonrpc 字面量在 Err 分支独立存在）。
        // M1b：v0 golden 的 message 修正为 "no.such.method"（scan.start 已从"不存在"变"存在"）。
        let ctx = CoreCtx::new(vec![]);
        let resp = handle_request(&ctx, &req(7, "no.such.method"));
        let expected: serde_json::Value = serde_json::from_str(
            include_str!("../../../proto/v0/examples/error_method_not_found.response.json").trim(),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&resp).unwrap(), expected);
    }

    fn req_with(id: i64, method: &str, params: serde_json::Value) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(id),
            method: method.into(),
            params: Some(params),
        }
    }

    fn ctx_with_fixture() -> (tempfile::NamedTempFile, CoreCtx) {
        let (f, dev) = crate::testutil::exfat_fixture();
        (f, CoreCtx::new(vec![dev]))
    }

    #[test]
    fn scan_start_happy_then_status_results() {
        let (_f, ctx) = ctx_with_fixture();
        let dev_id = ctx.devices[0].info().id.clone();
        let resp = handle_request(
            &ctx,
            &req_with(
                3,
                "scan.start",
                serde_json::json!({"device": dev_id, "mode": "quick"}),
            ),
        );
        let Response::Ok(ok) = resp else {
            panic!("{:?}", resp)
        };
        assert_eq!(ok.result["fs"], "exfat");
        assert_eq!(ok.result["taskId"], 1);
        let total_bytes = ok.result["totalBytes"].as_u64().unwrap();
        assert!(total_bytes > 0);
        // 轮询到 completed（handler 级小图秒级）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let r = handle_request(
                &ctx,
                &req_with(4, "scan.status", serde_json::json!({"taskId": 1})),
            );
            let Response::Ok(o) = r else { panic!() };
            if o.result["state"] == "completed" {
                assert_eq!(o.result["foundCount"], 3);
                break;
            }
            assert!(std::time::Instant::now() < deadline, "scan stuck: {o:?}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(
                5,
                "scan.results",
                serde_json::json!({"taskId": 1, "offset": 0, "limit": 10, "deletedOnly": true}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(o.result["total"], 1);
        assert_eq!(o.result["entries"][0]["name"], "DEL_ME.JPG");
        assert_eq!(o.result["entries"][0]["deleted"], true);
        assert_eq!(o.result["entries"][0]["quality"], "complete");
        // qual-t3 纵深防御：observer 1:1 ⇒ idx 集合恰为 0..found_count（防回调重复致库内双行）
        let Response::Ok(all) = handle_request(
            &ctx,
            &req_with(
                6,
                "scan.results",
                serde_json::json!({"taskId": 1, "offset": 0, "limit": 10}),
            ),
        ) else {
            panic!()
        };
        let mut idxs: Vec<u64> = all.result["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["idx"].as_u64().unwrap())
            .collect();
        idxs.sort_unstable();
        assert_eq!(idxs, vec![0, 1, 2], "idx 集合 == 0..found_count");
    }

    #[test]
    fn scan_start_response_matches_golden_normalized() {
        // taskId/totalBytes 是运行期值：与 v0.2.0 的 `<VERSION>` 归一化同款先例——把这两个键归一
        // 为 golden 值（taskId→1、totalBytes→整卷字节）后全等比对，其余字段逐字钉死。
        let (_f, ctx) = ctx_with_fixture();
        let dev_id = ctx.devices[0].info().id.clone();
        let size = ctx.devices[0].info().size_bytes;
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(
                3,
                "scan.start",
                serde_json::json!({"device": dev_id, "mode": "quick"}),
            ),
        ) else {
            panic!()
        };
        let mut actual = serde_json::to_value(&o).unwrap();
        assert_eq!(actual["result"]["totalBytes"], size);
        actual["result"]["taskId"] = serde_json::json!(1);
        actual["result"]["totalBytes"] = serde_json::json!(3907029168u64);
        let expected: serde_json::Value = serde_json::from_str(
            include_str!("../../../proto/v1/examples/scan_start.response.json").trim(),
        )
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn scan_start_permission_denied_maps_to_minus_32001() {
        struct Denied;
        impl crate::scan_task::DeviceOpener for Denied {
            fn open(&self, _id: &str) -> Result<Arc<dyn BlockDevice>, crate::scan_task::OpenError> {
                Err(crate::scan_task::OpenError::PermissionDenied)
            }
        }
        let ctx = CoreCtx::new(vec![]).with_scan(
            Arc::new(ScanManager::new(
                crate::store::Store::open_memory().unwrap(),
                Arc::new(|_| {}),
            )),
            Arc::new(Denied),
        );
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                3,
                "scan.start",
                serde_json::json!({"device": "unix:/dev/sdb"}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32001);
        assert_eq!(e.error.message, "Device permission denied: unix:/dev/sdb");
    }

    #[test]
    fn unknown_mode_rejected() {
        // "deep" 自 M1c 起合法（见 deep_scan_streams_carved_entries）；未知模式（"full"）
        // 必须 -32602，不得静默降级为 quick。
        let (_f, ctx) = ctx_with_fixture();
        let dev_id = ctx.devices[0].info().id.clone();
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                3,
                "scan.start",
                serde_json::json!({"device": dev_id, "mode": "full"}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32602);
        assert_eq!(e.error.message, "Invalid params: unsupported mode");
    }

    /// 轮询到终态（handler 级）。
    fn wait_state(ctx: &CoreCtx, id: u64, want: &str) -> serde_json::Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let Response::Ok(o) = handle_request(
                ctx,
                &req_with(99, "scan.status", serde_json::json!({"taskId": id})),
            ) else {
                panic!()
            };
            if o.result["state"] == want {
                return o.result.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "state stuck at {} (wanted {want}): {o:?}",
                o.result["state"]
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn deep_scan_streams_carved_entries() {
        // 全链：exfat 夹具算出空闲区间 → 在最大区间的绝对偏移处埋 mini_jpeg → mode:"deep"
        // → completed → scan.results 出 quality=="carved" 条目，byteOffset/size 精确。
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "LIVE_A.TXT", b"aaaa")
            .add_file("/", "DEL_ME.JPG", &[7u8; 9000])
            .delete("/", "DEL_ME.JPG")
            .build();
        let (_f0, dev0) = crate::testutil::dev_from_bytes(&image);
        let runs = xd_fs_exfat::freespace::unallocated_runs(&*dev0).unwrap();
        let run = runs
            .iter()
            .max_by_key(|r| r.end - r.start)
            .expect("夹具必有空闲区间")
            .clone();
        let total: u64 = runs.iter().map(|r| r.end - r.start).sum();
        let j = xd_fixtures::mini_jpeg(20000);
        assert!(j.len() as u64 <= run.end - run.start, "区间须容得下埋件");
        xd_fixtures::plant_in_run(&mut image, run.start, &j);
        let (_f, dev) = crate::testutil::dev_from_bytes(&image);
        let dev_id = dev.info().id.clone();
        let ctx = CoreCtx::new(vec![dev]);

        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(
                3,
                "scan.start",
                serde_json::json!({"device": dev_id, "mode": "deep"}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(o.result["fs"], "exfat");
        assert_eq!(
            o.result["totalBytes"], total,
            "深扫 totalBytes = Σ空闲区间长（进度分母）"
        );
        let id = o.result["taskId"].as_u64().unwrap();
        let status = wait_state(&ctx, id, "completed");
        assert_eq!(status["foundCount"], 1);
        assert_eq!(status["readBytes"], total, "进度收尾 == 100%");
        let Response::Ok(r) = handle_request(
            &ctx,
            &req_with(
                4,
                "scan.results",
                serde_json::json!({"taskId": id, "offset": 0, "limit": 10}),
            ),
        ) else {
            panic!()
        };
        let entries = r.result["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0]["quality"], "carved");
        assert_eq!(entries[0]["ext"], "jpg");
        assert_eq!(entries[0]["byteOffset"], run.start, "雕刻偏移逐字节精确");
        assert_eq!(entries[0]["sizeBytes"], j.len() as u64, "完整重组长");
        assert_eq!(entries[0]["deleted"], true);
        // qual-t6 纵深防御：雕刻 observer 1:1 ⇒ idx 集合恰为 0..foundCount（同 quick 口径，
        // 防回调重复/编号漂移——`found + 1` 类偏移变异在此必红）
        let mut idxs: Vec<u64> = entries.iter().map(|e| e["idx"].as_u64().unwrap()).collect();
        idxs.sort_unstable();
        let found = status["foundCount"].as_u64().unwrap();
        assert_eq!(
            idxs,
            (0..found).collect::<Vec<u64>>(),
            "idx 集合 == 0..foundCount"
        );
    }

    #[test]
    fn deep_without_free_space_info_fails_32005() {
        // 夹具布局：簇堆 32 扇区起、簇 5 = 根目录；根槽 1（0x81 位图项）FirstCluster@20
        // 改为越界 9999 → 位图不可读 → 深扫拒绝 -32005（绝不空跑）；快扫不受影响（对照）。
        const ROOT_OFF: usize = 32 * 512 + 3 * 4096;
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "LIVE_A.TXT", b"aaaa")
            .build();
        assert_eq!(image[ROOT_OFF + 32], 0x81, "前提：根槽 1 是位图项");
        image[ROOT_OFF + 32 + 20..ROOT_OFF + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
        let (_f, dev) = crate::testutil::dev_from_bytes(&image);
        let dev_id = dev.info().id.clone();
        let ctx = CoreCtx::new(vec![dev]);
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                3,
                "scan.start",
                serde_json::json!({"device": dev_id, "mode": "deep"}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32005);
        assert_eq!(e.error.message, "Cannot determine free space");
        // 对照：同设备快扫仍可成功（位图只堵深扫前置；目录链照读）
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(4, "scan.start", serde_json::json!({"device": dev_id})),
        ) else {
            panic!()
        };
        assert_eq!(o.result["fs"], "exfat");
        assert_eq!(
            o.result["totalBytes"],
            ctx.devices[0].info().size_bytes,
            "快扫 totalBytes 仍是整卷大小（深扫才改口径）"
        );
    }

    #[test]
    fn resolve_device_prefers_already_open_over_opener() {
        struct Denied;
        impl crate::scan_task::DeviceOpener for Denied {
            fn open(&self, _id: &str) -> Result<Arc<dyn BlockDevice>, crate::scan_task::OpenError> {
                Err(crate::scan_task::OpenError::PermissionDenied)
            }
        }
        let (_f, dev) = crate::testutil::exfat_fixture();
        let dev_id = dev.info().id.clone();
        let ctx = CoreCtx::new(vec![dev]).with_scan(
            Arc::new(ScanManager::new(
                crate::store::Store::open_memory().unwrap(),
                Arc::new(|_| {}),
            )),
            Arc::new(Denied),
        );
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(3, "scan.start", serde_json::json!({"device": dev_id})),
        ) else {
            panic!()
        };
        assert_eq!(
            o.result["fs"], "exfat",
            "已打开设备优先：不得落入 opener 的 -32001"
        );
    }

    #[test]
    fn scan_start_unsupported_and_unknown_device() {
        let (_f, zeros) = crate::testutil::dev_from_bytes(&[0u8; 4096]);
        let ctx = CoreCtx::new(vec![zeros]);
        let dev_id = ctx.devices[0].info().id.clone();
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(3, "scan.start", serde_json::json!({"device": dev_id})),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32002);
        let Response::Err(e2) = handle_request(
            &ctx,
            &req_with(
                4,
                "scan.start",
                serde_json::json!({"device": "unix:/dev/nope"}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e2.error.code, -32602);
        assert_eq!(e2.error.message, "Cannot open device: unix:/dev/nope");
    }

    #[test]
    fn scan_task_not_found_and_invalid_params() {
        let ctx = CoreCtx::new(vec![]);
        for m in ["scan.status", "scan.pause", "scan.resume", "scan.cancel"] {
            let Response::Err(e) =
                handle_request(&ctx, &req_with(9, m, serde_json::json!({"taskId": 42})))
            else {
                panic!()
            };
            assert_eq!(e.error.code, -32003, "{m}");
            assert_eq!(e.error.message, "Task not found: 42");
        }
        let Response::Err(e) =
            handle_request(&ctx, &req_with(9, "scan.start", serde_json::json!({})))
        else {
            panic!()
        };
        assert_eq!(e.error.code, -32602, "缺 device");
        // qual-t1 裁定 (c)：-32602 的 message 逐字钉前缀（reason 不属契约、前缀属之）
        assert_eq!(
            e.error.message,
            "Invalid params: missing or malformed params"
        );
        let Response::Err(e2) = handle_request(
            &ctx,
            &req_with(
                9,
                "scan.results",
                serde_json::json!({"taskId": 1, "offset": 0, "limit": 0}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e2.error.code, -32602, "limit=0 越契约");
        assert_eq!(
            e2.error.message,
            "Invalid params: limit out of range 1..=1000"
        );
    }

    #[test]
    fn scan_pause_on_completed_task_is_not_active() {
        use crate::api::ScanState;
        use crate::store::Store;
        let store = Store::open_memory().unwrap();
        let id = store
            .create_task("image:x.img", "exfat", "quick", 1)
            .unwrap();
        store.set_state(id, ScanState::Completed).unwrap();
        let ctx = CoreCtx::new(vec![]).with_scan(
            Arc::new(ScanManager::new(store, Arc::new(|_| {}))),
            Arc::new(NoopOpener),
        );
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(9, "scan.pause", serde_json::json!({"taskId": id})),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32004);
        assert_eq!(e.error.message, format!("Task not active: {id}"));
    }

    #[test]
    fn scan_response_goldens_round_trip() {
        // 确定性构造：store 直接建任务/写进度/插条目（handler 只读路径），逐字对 golden。
        use crate::store::Store;
        let store = Store::open_memory().unwrap();
        let id = store
            .create_task("unix:/dev/sdb", "exfat", "quick", 3907029168)
            .unwrap();
        assert_eq!(id, 1);
        store.set_progress(id, 123456, 42, 1500).unwrap();
        let mut entries: Vec<crate::api::ScanEntry> = serde_json::from_str(
            r#"[{"idx":0,"name":"IMG_0001.JPG","path":"/DCIM","ext":"jpg","sizeBytes":12000,"deleted":true,"isDir":false,"quality":"complete","firstCluster":6},
                {"idx":1,"name":"READ_ME.TXT","path":"/","ext":"txt","sizeBytes":7,"deleted":false,"isDir":false,"quality":"complete","firstCluster":9}]"#,
        )
        .unwrap();
        // golden 的 total=42 是「匹配总数」（分页语义）：补 40 条确定性填充使 COUNT(*) == 42；
        // 前两条保持 golden 逐字（limit=2 只回这两条），计划初稿漏了填充（total 会变 2，对不上 golden）。
        for i in 2..42u64 {
            entries.push(crate::api::ScanEntry {
                idx: i,
                name: format!("FILLER_{i:02}.BIN"),
                path: "/".into(),
                ext: "bin".into(),
                size_bytes: 0,
                deleted: false,
                is_dir: false,
                quality: "complete".into(),
                first_cluster: 0,
                byte_offset: None,
                contiguous: None,
            });
        }
        store.insert_entries(id, &entries).unwrap();
        let ctx = CoreCtx::new(vec![]).with_scan(
            Arc::new(ScanManager::new(store, Arc::new(|_| {}))),
            Arc::new(NoopOpener),
        );

        let golden = |name: &str| -> serde_json::Value {
            serde_json::from_str(match name {
                "status" => {
                    include_str!("../../../proto/v1/examples/scan_status.response.json").trim()
                }
                "results" => {
                    include_str!("../../../proto/v1/examples/scan_results.response.json").trim()
                }
                _ => unreachable!(),
            })
            .unwrap()
        };
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(4, "scan.status", serde_json::json!({"taskId": 1})),
        ) else {
            panic!()
        };
        assert_eq!(serde_json::to_value(&o).unwrap(), golden("status"));
        let Response::Ok(o2) = handle_request(
            &ctx,
            &req_with(
                5,
                "scan.results",
                serde_json::json!({"taskId": 1, "offset": 0, "limit": 2, "deletedOnly": false}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(serde_json::to_value(&o2).unwrap(), golden("results"));
    }

    #[test]
    fn scan_results_carved_golden_round_trip() {
        // 雕刻页 golden：独立 store + 独立 task（id=1，不与既有 golden 测试的 store 抢位），
        // 只走 scan.results 读路径——预置两条 carved 条目（含 byteOffset）→ 逐字对 golden。
        use crate::store::Store;
        let carved: serde_json::Value = serde_json::from_str(
            include_str!("../../../proto/v1/examples/scan_results_carved.response.json").trim(),
        )
        .unwrap();
        let entries: Vec<crate::api::ScanEntry> =
            serde_json::from_value(carved["result"]["entries"].clone()).unwrap();
        let store = Store::open_memory().unwrap();
        let id = store
            .create_task("unix:/dev/sdb", "exfat", "quick", 1)
            .unwrap();
        assert_eq!(id, 1);
        store.insert_entries(id, &entries).unwrap();
        let ctx = CoreCtx::new(vec![]).with_scan(
            Arc::new(ScanManager::new(store, Arc::new(|_| {}))),
            Arc::new(NoopOpener),
        );
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(
                11,
                "scan.results",
                serde_json::json!({"taskId": 1, "offset": 0, "limit": 10}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(serde_json::to_value(&o).unwrap(), carved);
    }

    #[test]
    fn device_list_v1_golden_round_trip() {
        // 确定性构造：image 设备（transport 省略）+ list_only 物理设备（transport:"usb"）。
        // 注意：真 ImageFileDevice 的 id 含绝对路径（`image:/tmp/…`），逐字对不上 golden 的
        // `image:test.img`——用 info 桩（device.list 零 open，只读 info，语义等价）钉全等。
        struct ImgStub;
        impl BlockDevice for ImgStub {
            fn info(&self) -> &DeviceInfo {
                static I: std::sync::OnceLock<DeviceInfo> = std::sync::OnceLock::new();
                I.get_or_init(|| DeviceInfo {
                    id: "image:test.img".into(),
                    name: "test.img".into(),
                    kind: xd_device::DeviceKind::Image,
                    size_bytes: 4096,
                    removable: false,
                    fs_guess: None,
                    transport: None,
                })
            }
            fn read_at(&self, _o: u64, _b: &mut [u8]) -> Result<usize, xd_device::DeviceError> {
                Ok(0)
            }
        }
        let ctx = CoreCtx::new(vec![Arc::new(ImgStub)]).with_list_only(vec![DeviceInfo {
            id: "unix:/dev/sdb".into(),
            name: "USB Disk".into(),
            kind: xd_device::DeviceKind::Physical,
            size_bytes: 3907029168,
            removable: true,
            fs_guess: None,
            transport: Some("usb".into()),
        }]);
        let Response::Ok(o) = handle_request(&ctx, &req(2, "device.list")) else {
            panic!()
        };
        let expected: serde_json::Value = serde_json::from_str(
            include_str!("../../../proto/v1/examples/device_list.response.json").trim(),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&o).unwrap(), expected);
    }

    /// M1d T3：真 exfat 夹具快扫至 completed 的 ctx——scans 与 exports 共享同一 store
    /// （exports 无 `--db`：导出起子进程臂在单测不可达，仅走校验/参数路径）。
    fn scanned_ctx() -> (tempfile::NamedTempFile, Arc<ScanManager>, CoreCtx) {
        let (f, dev) = crate::testutil::exfat_fixture();
        let dev_id = dev.info().id.clone();
        let mgr = Arc::new(ScanManager::new(
            crate::store::Store::open_memory().unwrap(),
            Arc::new(|_| {}),
        ));
        let ctx = CoreCtx::new(vec![dev])
            .with_scan(mgr.clone(), Arc::new(NoopOpener))
            .with_export(Arc::new(ExportManager::new(
                mgr.store_arc(),
                Arc::new(|_| {}),
                None,
            )));
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(1, "scan.start", serde_json::json!({"device": dev_id})),
        ) else {
            panic!()
        };
        let id = o.result["taskId"].as_u64().unwrap();
        wait_state(&ctx, id, "completed");
        (f, mgr, ctx)
    }

    #[test]
    fn fs_read_happy_slices_and_eof() {
        let (_f, _mgr, ctx) = scanned_ctx();
        // LIVE_A.TXT = "aaaa"（idx 0）：整读有 eof
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(
                21,
                "fs.read",
                serde_json::json!({"taskId": 1, "idx": 0, "offset": 0, "length": 16}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(o.result["bytesBase64"], "YWFhYQ==", "base64(aaaa)");
        assert_eq!(o.result["eof"], true);
        // LIVE_B.PNG = [5;100]（idx 1）：中段非 eof、恰到尾 eof
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(
                22,
                "fs.read",
                serde_json::json!({"taskId": 1, "idx": 1, "offset": 10, "length": 10}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(
            o.result["bytesBase64"],
            crate::fs_read::to_base64(&[5u8; 10])
        );
        assert_eq!(o.result["eof"], false);
        let Response::Ok(o) = handle_request(
            &ctx,
            &req_with(
                23,
                "fs.read",
                serde_json::json!({"taskId": 1, "idx": 1, "offset": 50, "length": 50}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(o.result["eof"], true, "恰到条目尾 → eof");
    }

    #[test]
    fn fs_read_error_paths() {
        let (_f, mgr, ctx) = scanned_ctx();
        // -32008：无此条目
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                21,
                "fs.read",
                serde_json::json!({"taskId": 1, "idx": 99, "offset": 0, "length": 16}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32008);
        assert_eq!(e.error.message, "Entry not found: 99");
        // -32003：无此任务（先于条目判定）
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                21,
                "fs.read",
                serde_json::json!({"taskId": 42, "idx": 0, "offset": 0, "length": 16}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32003);
        // -32602：length 越契约（0 与 >1MiB）
        for len in [0u64, (1 << 20) + 1] {
            let Response::Err(e) = handle_request(
                &ctx,
                &req_with(
                    21,
                    "fs.read",
                    serde_json::json!({"taskId": 1, "idx": 0, "offset": 0, "length": len}),
                ),
            ) else {
                panic!()
            };
            assert_eq!(e.error.code, -32602, "length={len}");
            assert_eq!(
                e.error.message,
                "Invalid params: length out of range 1..=1048576"
            );
        }
        // -32009：条目 > 64MiB（预览上限；导出不受此限）。字面量而非 MAX_PREVIEW——契约锚
        // （qual I2）：常量被改大时本测必红（fs_read 另有常量绝对锚）。
        mgr.store_arc()
            .insert_entries(
                1,
                &[crate::api::ScanEntry {
                    idx: 9,
                    name: "HUGE.BIN".into(),
                    path: "/".into(),
                    ext: "bin".into(),
                    size_bytes: 64 * 1024 * 1024 + 1,
                    deleted: false,
                    is_dir: false,
                    quality: "complete".into(),
                    first_cluster: 6,
                    byte_offset: None,
                    contiguous: None,
                }],
            )
            .unwrap();
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                21,
                "fs.read",
                serde_json::json!({"taskId": 1, "idx": 9, "offset": 0, "length": 16}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32009);
        assert_eq!(e.error.message, "Entry too large: 67108865");
    }

    #[test]
    fn export_start_param_errors_are_32602() {
        let ctx = CoreCtx::new(vec![]);
        let t = tempfile::tempdir().unwrap();
        let tdir = t.path().to_str().unwrap().to_string();
        // 空 idxs → -32602
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                31,
                "export.start",
                serde_json::json!({"taskId": 1, "idxs": [], "targetDir": tdir}),
            ),
        ) else {
            panic!("空 idxs 必须 -32602")
        };
        assert_eq!(e.error.code, -32602);
        // 重复项去重后非空（[5,5] → [5]）：不属参数层错误——继续走到任务判定（-32003 证去重语义）
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                31,
                "export.start",
                serde_json::json!({"taskId": 1, "idxs": [5, 5], "targetDir": tdir}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32003, "[5,5] 去重后为 [5]：须过参数层");
        // >100000（去重后）→ -32602
        let many: Vec<u64> = (0..(MAX_IDXS as u64 + 1)).collect();
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                31,
                "export.start",
                serde_json::json!({"taskId": 1, "idxs": many, "targetDir": tdir}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32602);
        // 相对路径 targetDir → -32602（契约：必须绝对路径）
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                31,
                "export.start",
                serde_json::json!({"taskId": 1, "idxs": [0], "targetDir": "relative/out"}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32602);
        // 缺字段
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(31, "export.start", serde_json::json!({"idxs": [0]})),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32602);
    }

    #[test]
    fn export_start_target_and_db_error_paths() {
        let (_f, _mgr, ctx) = scanned_ctx();
        // -32003：无此任务
        let t = tempfile::tempdir().unwrap();
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                31,
                "export.start",
                serde_json::json!({"taskId": 42, "idxs": [0], "targetDir": t.path().to_str().unwrap()}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32003);
        // 平台中立的「不存在目标」：由 tempdir 派生**绝对**路径——绝对性是 handlers 的边界校验
        // （`is_absolute()`，Windows 上字面量 "/nonexistent-x" 无盘符即非绝对 ⇒ 先吃 -32602，
        // 到不了本用例要钉的 -32008/-32007）。
        let missing_path = t.path().join("missing-dir");
        let missing = missing_path.to_str().unwrap();
        // -32008：无此条目（先于目标校验——错误码优先级）
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                31,
                "export.start",
                serde_json::json!({"taskId": 1, "idxs": [99], "targetDir": missing}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32008);
        // -32007：目标不存在（条目齐备）
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                31,
                "export.start",
                serde_json::json!({"taskId": 1, "idxs": [0], "targetDir": missing}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32007);
        assert_eq!(e.error.message, format!("Target not writable: {missing}"));
        // -32603：无 --db（内存库）——校验全过后诚实拒绝（子进程无从读库）
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(
                31,
                "export.start",
                serde_json::json!({"taskId": 1, "idxs": [0], "targetDir": t.path().to_str().unwrap()}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32603);
    }

    #[test]
    fn export_cancel_unknown_id_is_32602() {
        let ctx = CoreCtx::new(vec![]);
        let Response::Err(e) = handle_request(
            &ctx,
            &req_with(41, "export.cancel", serde_json::json!({"exportId": 7})),
        ) else {
            panic!()
        };
        assert_eq!(e.error.code, -32602);
        assert_eq!(e.error.message, "Invalid params: unknown exportId: 7");
        // 缺字段 → -32602（params 解析层）
        let Response::Err(e) =
            handle_request(&ctx, &req_with(41, "export.cancel", serde_json::json!({})))
        else {
            panic!()
        };
        assert_eq!(e.error.code, -32602);
    }
}
