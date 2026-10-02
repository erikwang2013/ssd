// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! FAT 表访问：按需读取单个表项（不整表载入——32GB 卡的 FAT 可达 128MB）。
//! ponytail: 每次查询一次 read_at；如需扫描吞吐再引入扇区缓存/预读。

use crate::FatError;
use crate::bpb::{Bpb, FatType};
use xd_device::BlockDevice;

pub struct Fat<'d> {
    dev: &'d dyn BlockDevice,
    bpb: &'d Bpb,
}

impl<'d> Fat<'d> {
    pub fn new(dev: &'d dyn BlockDevice, bpb: &'d Bpb) -> Self {
        Self { dev, bpb }
    }

    /// 读取 cluster 的表项原值（已按类型掩码）。
    pub fn entry(&self, cluster: u32) -> Result<u32, FatError> {
        match self.bpb.fat_type {
            FatType::Fat32 => {
                let mut b = [0u8; 4];
                let n = self.dev.read_at(self.bpb.fat_entry_byte(cluster), &mut b)?;
                if n < 4 {
                    return Err(FatError::InvalidBpb(format!(
                        "fat entry {cluster} beyond device"
                    )));
                }
                Ok(u32::from_le_bytes(b) & 0x0FFF_FFFF)
            }
            FatType::Fat16 => {
                let mut b = [0u8; 2];
                let n = self.dev.read_at(self.bpb.fat_entry_byte(cluster), &mut b)?;
                if n < 2 {
                    return Err(FatError::InvalidBpb(format!(
                        "fat entry {cluster} beyond device"
                    )));
                }
                Ok(u16::from_le_bytes(b) as u32)
            }
            FatType::Fat12 => {
                let off = self.bpb.fat_start_sector as u64 * self.bpb.bytes_per_sector as u64
                    + (cluster + cluster / 2) as u64;
                let mut b = [0u8; 2];
                let n = self.dev.read_at(off, &mut b)?;
                if n < 2 {
                    return Err(FatError::InvalidBpb(format!(
                        "fat entry {cluster} beyond device"
                    )));
                }
                let pair = u16::from_le_bytes(b) as u32;
                Ok(if cluster.is_multiple_of(2) {
                    pair & 0x0FFF
                } else {
                    pair >> 4
                })
            }
        }
    }

    pub fn is_free(&self, cluster: u32) -> Result<bool, FatError> {
        Ok(self.entry(cluster)? == 0)
    }

    fn is_eoc(&self, value: u32) -> bool {
        match self.bpb.fat_type {
            FatType::Fat12 => value >= 0x0FF8,
            FatType::Fat16 => value >= 0xFFF8,
            FatType::Fat32 => value >= 0x0FFF_FFF8,
        }
    }

    /// `entry` 指向链尾（含 EOC/**保留值/坏簇**——都视为不可继续）。
    pub fn is_eoc_reachable(&self, cluster: u32) -> Result<bool, FatError> {
        let v = self.entry(cluster)?;
        Ok(self.is_eoc(v) || v == 1) // 1 = 保留值（坏簇标记亦按链尾处理）
    }

    /// 从 start 顺链读取簇号序列（含 start）。守卫：环/超长链（≤ 全盘簇数 + 2）。
    pub fn chain(&self, start: u32) -> Result<Vec<u32>, FatError> {
        let mut out = vec![start];
        let mut cur = start;
        let limit = self.bpb.data_cluster_count() + 2;
        while out.len() as u32 <= limit {
            let v = self.entry(cur)?;
            if v == 0 || self.is_eoc(v) || v == 1 {
                break;
            }
            if v < 2 || v > limit {
                break; // 越界值按断链处理
            }
            out.push(v);
            cur = v;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpb;
    use xd_device::image::ImageFileDevice;

    fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(image).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        (f, dev)
    }

    #[test]
    fn follows_fat16_chain_and_reports_eoc() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "A.BIN", &[0u8; 1200])
            .build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        assert_eq!(fat.entry(2).unwrap(), 3);
        assert!(fat.is_eoc_reachable(4).unwrap()); // 1200B → 3 簇：2→3→4(EOC)
        assert_eq!(fat.chain(2).unwrap(), vec![2, 3, 4]);
    }

    #[test]
    fn follows_fat32_chain_with_mask() {
        let image = xd_fixtures::FatImageBuilder::fat32()
            .add_file("/", "A.BIN", &[0u8; 1200])
            .build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        // 根目录占簇 2，文件从簇 3 起：1200B → 3 簇 3→4→5(EOC)
        assert_eq!(fat.chain(3).unwrap(), vec![3, 4, 5]);
        assert!(fat.is_eoc_reachable(5).unwrap());
    }

    #[test]
    fn follows_fat12_nibble_packing() {
        let image = xd_fixtures::FatImageBuilder::fat12()
            .add_file("/", "A.BIN", &[0u8; 600])
            .build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        assert_eq!(fat.chain(2).unwrap(), vec![2, 3]);
    }

    #[test]
    fn is_free_reports_freed_clusters() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &[0u8; 1024])
            .delete("/", "GONE.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        assert!(fat.is_free(2).unwrap());
        assert!(fat.is_free(3).unwrap());
    }

    #[test]
    fn broken_chain_for_deleted_file_yields_short_chain() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &[0u8; 1024])
            .delete("/", "GONE.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        assert_eq!(fat.chain(2).unwrap(), vec![2]); // 首跳即断（0=free）
    }
}
