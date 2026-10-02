// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 分配位图（§7.1）：**exFAT 的分配权威**（删除后 FAT 是 stale）。
//! 位下标 = cluster - 2；字节内 LSB 优先；位 = 1 已分配/坏簇、0 空闲。
//! 位下标 ≥ cluster_count 为保留位——不得解读（is_free 直接拒绝越界簇号）。
//! DataLength ≥ ceil(cluster_count/8)（小于 = 错误）；大于合法（多出为保留位）。

use crate::ExfatError;
use crate::boot::ExfatBoot;
use crate::fattab::{Fat32, read_allocation};
use xd_device::BlockDevice;

pub struct Bitmap {
    bytes: Vec<u8>,
    cluster_count: u32,
}

impl Bitmap {
    pub fn load(
        dev: &dyn BlockDevice,
        boot: &ExfatBoot,
        fat: &Fat32,
        first_cluster: u32,
        data_length: u64,
    ) -> Result<Bitmap, ExfatError> {
        let min = (boot.cluster_count as u64).div_ceil(8);
        if data_length < min {
            return Err(ExfatError::InvalidBoot(
                "bitmap DataLength 小于 ceil(count/8)".into(),
            ));
        }
        let bytes = read_allocation(dev, boot, fat, first_cluster, data_length)?;
        Ok(Bitmap {
            bytes,
            cluster_count: boot.cluster_count,
        })
    }

    /// 簇 `c` 是否空闲。`c` 须满足 2..=cluster_count+1；保留位/越界 → Err（不得解读）。
    pub fn is_free(&self, c: u32) -> Result<bool, ExfatError> {
        if !(2..=self.cluster_count as u64 + 1).contains(&(c as u64)) {
            return Err(ExfatError::InvalidBoot(format!(
                "cluster {c} 越界（保留位不得解读）"
            )));
        }
        let idx = (c - 2) as usize;
        let byte = self
            .bytes
            .get(idx / 8)
            .ok_or_else(|| ExfatError::InvalidBoot("bitmap 数据不足（不应到达）".into()))?;
        Ok(byte & (1 << (idx % 8)) == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot;
    use crate::boot::testutil::dev_for;

    #[test]
    fn loads_and_queries_fixture_bitmap() {
        // A.TXT 占簇 6 → 位已置；位图项在根目录
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"hello")
            .build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let bm = Bitmap::load(&dev, &boot, &fat, 2, 32).unwrap();
        assert!(!bm.is_free(2).unwrap(), "位图自身簇已分配");
        assert!(!bm.is_free(5).unwrap(), "根目录簇已分配");
        assert!(!bm.is_free(6).unwrap(), "A.TXT 数据簇已分配");
        assert!(bm.is_free(7).unwrap());
        assert!(bm.is_free(253).unwrap()); // 末簇
    }

    #[test]
    fn is_free_range_and_reserved_discipline() {
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let bm = Bitmap::load(&dev, &boot, &fat, 2, 32).unwrap();
        assert!(bm.is_free(1).is_err());
        assert!(bm.is_free(254).is_err()); // count+2 → 保留/越界：不得解读
        assert!(bm.is_free(300).is_err());
    }

    #[test]
    fn short_datalength_is_err() {
        // DataLength < ceil(count/8) = 32 → 加载必须报错（部分位图不得用于判空闲）
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        assert!(Bitmap::load(&dev, &boot, &fat, 2, 31).is_err());
    }

    #[test]
    fn oversized_datalength_allowed_and_extra_bits_ignored() {
        // DataLength 64（> ceil(252/8)=32）合法；多出的位是保留位，不解读
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let bm = Bitmap::load(&dev, &boot, &fat, 2, 64).unwrap();
        assert!(bm.is_free(7).unwrap());
        assert!(bm.is_free(253).unwrap());
    }

    #[test]
    fn unreadable_bitmap_is_err() {
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        assert!(Bitmap::load(&dev, &boot, &fat, 0, 32).is_err()); // first_cluster < 2
        assert!(Bitmap::load(&dev, &boot, &fat, 9999, 32).is_err()); // 越界
    }

    #[test]
    fn read_allocation_chain_vs_contiguous_fallback() {
        // 链覆盖：链 [3,4] 覆盖 2 簇需求 → 按链读（3 在前 4 在后，与物理序不同才能区分——夹具里 upcase 链物理连续，
        // 本测试改用 DataLength=8192 从簇 3 起：链 [3,4] 恰好覆盖 → 内容 == 簇3+簇4）
        // 短链回退：FAT[3]=EOC → 链 [3] 不足 → 连续回退读簇 3,4（物理连续）
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let a = read_allocation(&dev, &boot, &fat, 3, 8192).unwrap();
        assert_eq!(a.len(), 8192);
        assert_eq!(&a[..5836], xd_fixtures::UPCASE_TABLE);
        let mut img2 = image.clone();
        img2[24 * 512 + 3 * 4..24 * 512 + 3 * 4 + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let (_f2, dev2) = dev_for(&img2);
        let boot2 = boot::parse(&dev2).unwrap();
        let fat2 = Fat32::new(&dev2, &boot2);
        let b = read_allocation(&dev2, &boot2, &fat2, 3, 8192).unwrap();
        assert_eq!(b, a, "短链回退读物理连续簇，内容应与链读一致");
    }

    #[test]
    fn read_allocation_partial_device_is_err() {
        // 要求读满 DataLength；截断设备 → Err（不允许用部分位图判空闲）
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        assert!(read_allocation(&dev, &boot, &fat, 3, 10 * 1024 * 1024).is_err());
    }
}
