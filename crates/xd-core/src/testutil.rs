// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
use std::sync::Arc;
use std::time::{Duration, Instant};

use xd_device::image::ImageFileDevice;
use xd_device::{BlockDevice, DeviceError, DeviceInfo};

use crate::api::ScanState;
use crate::scan_task::ScanManager;

/// 每次 read_at 睡 `delay`：让扫描的条目边界在测试里可观测、可驻停。
pub struct SlowDev {
    inner: Arc<dyn BlockDevice>,
    delay: Duration,
}

impl SlowDev {
    /// 包装为 `Arc<dyn BlockDevice>`（不叫 `new`：返回的不是 `Self`，clippy 的 `new_ret_no_self`）。
    pub fn wrap(inner: Arc<dyn BlockDevice>, delay: Duration) -> Arc<dyn BlockDevice> {
        Arc::new(Self { inner, delay })
    }
}

impl BlockDevice for SlowDev {
    fn info(&self) -> &DeviceInfo {
        self.inner.info()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        std::thread::sleep(self.delay);
        self.inner.read_at(offset, buf)
    }
}

/// 写镜像字节到临时文件并打开为设备（临时文件由调用方持有存活）。
pub fn dev_from_bytes(bytes: &[u8]) -> (tempfile::NamedTempFile, Arc<dyn BlockDevice>) {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(bytes).unwrap();
    f.flush().unwrap();
    let dev = ImageFileDevice::open(f.path()).unwrap();
    (f, Arc::new(dev))
}

/// 小 exfat 夹具：2 live 文件 + 1 删除文件（**共 3 条目**——T5/T8 全链断言 `foundCount==3`、idx 集合 0..3；
/// 计划初稿曾只放 2 条目，与断言不一致，已修正）。
pub fn exfat_fixture() -> (tempfile::NamedTempFile, Arc<dyn BlockDevice>) {
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "LIVE_A.TXT", b"aaaa")
        .add_file("/", "LIVE_B.PNG", &[5u8; 100])
        .add_file("/", "DEL_ME.JPG", &[7u8; 9000])
        .delete("/", "DEL_ME.JPG")
        .build();
    dev_from_bytes(&image)
}

/// 轮询到目标态（超时 panic 带现场）。
pub fn wait_for_state(
    mgr: &ScanManager,
    id: u64,
    want: ScanState,
    timeout: Duration,
) -> crate::store::TaskRow {
    let deadline = Instant::now() + timeout;
    loop {
        let t = mgr.status(id).unwrap();
        if t.state == want {
            return t;
        }
        assert!(
            Instant::now() < deadline,
            "state stuck at {:?} (wanted {want:?}), task={t:?}",
            t.state
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
