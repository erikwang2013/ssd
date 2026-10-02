// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 只读镜像文件后端：M0 的测试与开发全部基于镜像，不依赖真实硬件。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use crate::{BlockDevice, DeviceError, DeviceInfo, DeviceKind};

pub struct ImageFileDevice {
    info: DeviceInfo,
    file: Mutex<File>,
}

impl ImageFileDevice {
    pub fn open(path: &Path) -> Result<Self, DeviceError> {
        let file = File::open(path)?; // 永远只读打开
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(DeviceError::NotAFile(path.display().to_string()));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self {
            info: DeviceInfo {
                id: format!("image:{}", path.display()),
                name,
                kind: DeviceKind::Image,
                size_bytes: meta.len(),
                removable: false,
                fs_guess: None,
            },
            file: Mutex::new(file),
        })
    }
}

impl BlockDevice for ImageFileDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        if buf.is_empty() || offset >= self.info.size_bytes {
            return Ok(0);
        }
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(offset))?;
        let mut total = 0;
        while total < buf.len() {
            let n = file.read(&mut buf[total..])?;
            if n == 0 {
                break;
            }
            total += n;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_image(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn open_rejects_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ImageFileDevice::open(dir.path()).is_err());
    }

    #[test]
    fn read_at_returns_correct_bytes() {
        let pattern: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let f = temp_image(&pattern);
        let dev = ImageFileDevice::open(f.path()).unwrap();
        assert_eq!(dev.size_bytes(), 4096);
        assert_eq!(dev.info().kind, crate::DeviceKind::Image);
        assert_eq!(dev.info().id, format!("image:{}", f.path().display()));
        let mut buf = [0u8; 100];
        let n = dev.read_at(1000, &mut buf).unwrap();
        assert_eq!(n, 100);
        assert_eq!(&buf[..], &pattern[1000..1100]);
    }

    #[test]
    fn read_at_eof_returns_short_then_zero() {
        let f = temp_image(&[7u8; 64]);
        let dev = ImageFileDevice::open(f.path()).unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(dev.read_at(60, &mut buf).unwrap(), 4);
        assert_eq!(dev.read_at(64, &mut buf).unwrap(), 0);
        assert_eq!(dev.read_at(100, &mut buf).unwrap(), 0);
    }

    #[test]
    fn reads_do_not_modify_file() {
        let pattern: Vec<u8> = (0..256u32).map(|i| i as u8).collect();
        let f = temp_image(&pattern);
        let dev = ImageFileDevice::open(f.path()).unwrap();
        let mut buf = [0u8; 256];
        dev.read_at(0, &mut buf).unwrap();
        assert_eq!(std::fs::read(f.path()).unwrap(), pattern);
    }
}
