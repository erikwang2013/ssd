// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! exFAT 32 位 FAT：EOC 是精确值 0xFFFFFFFF（非区间！）、坏簇 0xFFFFFFF7、
//! 0 视为链断裂；ActiveFat 由 VolumeFlags bit0 选择（texFAT）。

use crate::ExfatError;
use crate::boot::ExfatBoot;
use xd_device::BlockDevice;

pub const EOC: u32 = 0xFFFF_FFFF;
pub const BAD_CLUSTER: u32 = 0xFFFF_FFF7;

pub fn is_eoc(v: u32) -> bool {
    v == EOC
}

pub struct Fat32<'d> {
    dev: &'d dyn BlockDevice,
    boot: &'d ExfatBoot,
}

impl<'d> Fat32<'d> {
    pub fn new(dev: &'d dyn BlockDevice, boot: &'d ExfatBoot) -> Self {
        Self { dev, boot }
    }

    fn entry_bytes(&self, c: u32) -> u64 {
        self.boot.active_fat_offset() as u64 * self.boot.sector_bytes() + c as u64 * 4
    }

    pub fn next_raw(&self, c: u32) -> Result<u32, ExfatError> {
        let mut b = [0u8; 4];
        let n = self
            .dev
            .read_at(self.entry_bytes(c), &mut b)
            .map_err(|e| ExfatError::Io(e.to_string()))?;
        if n < 4 {
            return Err(ExfatError::InvalidBoot(format!(
                "fat entry {c} beyond device"
            )));
        }
        Ok(u32::from_le_bytes(b))
    }

    /// 顺链收集。起点须 2..=count+1；EOC/0/1（保留值）/坏簇/越界值即停；环 → 有界长链（len == count+2）。
    pub fn chain(&self, start: u32) -> Result<Vec<u32>, ExfatError> {
        let max_cluster = self.boot.cluster_count as u64 + 1;
        if !(2..=max_cluster).contains(&(start as u64)) {
            return Err(ExfatError::InvalidBoot(format!(
                "chain start {start} out of range"
            )));
        }
        let mut out = vec![start];
        let mut cur = start;
        while (out.len() as u64) <= max_cluster {
            let v = self.next_raw(cur)?;
            // is_eoc/BAD_CLUSTER 在合法几何下被上界覆盖（非承重，保留以对齐规范可读性）
            if v < 2 || is_eoc(v) || v == BAD_CLUSTER || (v as u64) > max_cluster {
                break;
            }
            out.push(v);
            cur = v;
        }
        Ok(out)
    }
}

/// 读取一段分配（DataLength 字节）：FAT 链能覆盖需求则按链读，否则退化为物理连续读
/// （位图在真实卷上按连续扇区访问——内核 balloc.c 同款；规范上是 FAT 链描述，两条路都兼容）。
/// 读不满 DataLength → Err（部分数据不得当作权威分配信息）。
pub fn read_allocation(
    dev: &dyn BlockDevice,
    boot: &ExfatBoot,
    fat: &Fat32,
    first_cluster: u32,
    data_length: u64,
) -> Result<Vec<u8>, ExfatError> {
    if data_length == 0 {
        // dl==0：空分配直接 Ok(0)，不校验起点（调用点均先验；顺序语义见 qual-t3 Minor 7）
        return Ok(Vec::new());
    }
    if data_length > 64 * 1024 * 1024 {
        return Err(ExfatError::InvalidBoot("allocation 过大（>64MiB）".into()));
    }
    let cb = boot.cluster_bytes();
    let need = data_length.div_ceil(cb);
    let max_cluster = boot.cluster_count as u64 + 1;
    if !(2..=max_cluster).contains(&(first_cluster as u64)) {
        return Err(ExfatError::InvalidBoot("allocation 起点越界".into()));
    }
    let mut clusters: Vec<u32> = Vec::new();
    if let Ok(chain) = fat.chain(first_cluster)
        && chain.len() as u64 >= need
    {
        clusters = chain[..need as usize].to_vec();
    }
    if clusters.is_empty() {
        // 连续回退（界与 chain 同源：u64 累积防溢出）
        let mut c = first_cluster as u64;
        while (clusters.len() as u64) < need && c <= max_cluster {
            clusters.push(c as u32);
            c += 1;
        }
        if (clusters.len() as u64) < need {
            return Err(ExfatError::InvalidBoot("allocation 超出簇堆".into()));
        }
    }
    let mut out = Vec::with_capacity(data_length.min(clusters.len() as u64 * cb) as usize);
    let mut buf = vec![0u8; cb as usize];
    for c in clusters {
        let n = dev
            .read_at(boot.cluster_to_byte(c), &mut buf)
            .map_err(|e| ExfatError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() as u64 >= data_length || n < buf.len() {
            break;
        }
    }
    if (out.len() as u64) < data_length {
        return Err(ExfatError::InvalidBoot(
            "allocation 读不满（设备截断？）".into(),
        ));
    }
    out.truncate(data_length as usize);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot;
    use crate::boot::testutil::{dev_for, patch_boot_both};

    #[test]
    fn next_raw_kat_and_chain() {
        // 夹具：upcase 链 3→4→EOC，root 5→EOC
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        assert_eq!(fat.next_raw(3).unwrap(), 4);
        assert_eq!(fat.next_raw(4).unwrap(), 0xFFFF_FFFF);
        assert!(is_eoc(fat.next_raw(4).unwrap()));
        assert_eq!(fat.chain(3).unwrap(), vec![3, 4]);
        assert_eq!(fat.chain(5).unwrap(), vec![5]);
    }

    #[test]
    fn chain_rejects_bad_start() {
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        assert!(fat.chain(0).is_err());
        assert!(fat.chain(1).is_err());
        assert!(fat.chain(300).is_err()); // > count+1
        assert_eq!(fat.chain(253).unwrap(), vec![253]); // count+1 = 末簇，合法端点
        assert!(fat.chain(254).is_err());
    }

    #[test]
    fn chain_breaks_on_reserved_value_one() {
        // FAT[6]=1（保留值）：不得推进链——cluster_to_byte(1) 在 debug panic / release 回绕读错区
        let mut img = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "B.BIN", &[7u8; 100])
            .build();
        let fat_b = 24 * 512;
        img[fat_b + 6 * 4..fat_b + 6 * 4 + 4].copy_from_slice(&1u32.to_le_bytes());
        let (_f, dev) = dev_for(&img);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        assert_eq!(fat.chain(6).unwrap(), vec![6]);
    }

    #[test]
    fn chain_breaks_on_zero_not_treats_as_cluster_zero() {
        // B.BIN 链式 6→7→EOC；把 FAT[7] 清零 → 链应为 [6,7]（0 视为断裂），不是 [6,7,0,...]
        let mut img = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "B.BIN", &[7u8; 5000])
            .build();
        let fat_b = 24 * 512;
        img[fat_b + 7 * 4..fat_b + 7 * 4 + 4].copy_from_slice(&0u32.to_le_bytes());
        let (_f, dev) = dev_for(&img);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        assert_eq!(fat.chain(6).unwrap(), vec![6, 7]);
    }

    #[test]
    fn chain_loop_is_bounded() {
        // 自环 6→6：链长必须有界（count+2 = 254），不挂起
        let mut img = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "B.BIN", &[7u8; 100])
            .build();
        let fat_b = 24 * 512;
        img[fat_b + 6 * 4..fat_b + 6 * 4 + 4].copy_from_slice(&6u32.to_le_bytes());
        let (_f, dev) = dev_for(&img);
        let boot = boot::parse(&dev).unwrap();
        let fat = Fat32::new(&dev, &boot);
        let chain = fat.chain(6).unwrap();
        assert_eq!(chain.len() as u32, 252 + 2);
        assert!(chain.iter().all(|c| *c == 6));
    }

    #[test]
    fn entry_beyond_device_is_err_not_panic() {
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let truncated = image[..24 * 512 + 8].to_vec(); // FAT 只剩 2 个表项
        let (_f, dev) = dev_for(&truncated);
        // 用完整几何包一层？截断后 parse 会因 checksum 读不满而失败——改为构造最小几何：
        // 直接在 dev 上手工构造 ExfatBoot（pub 字段）以隔离测试 entry 读取路径
        let boot = ExfatBoot {
            bytes_per_sector_shift: 9,
            sectors_per_cluster_shift: 3,
            number_of_fats: 1,
            fat_offset: 24,
            fat_length: 2,
            cluster_heap_offset: 32,
            cluster_count: 252,
            root_cluster: 5,
            volume_length: 2048,
            volume_flags: 0,
            backup_used: false,
        };
        let fat = Fat32::new(&dev, &boot);
        assert!(matches!(fat.next_raw(6), Err(ExfatError::InvalidBoot(_))));
    }

    #[test]
    fn active_fat_selection_reads_second_fat() {
        // 主 FAT entry(6)=7（B.BIN 两簇 6→7）；第二 FAT entry(6)=EOC；
        // ActiveFat=1 → 若正确读第二 FAT：chain(6)==[6]；若错读第一 FAT：[6,7]
        let mut img = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "B.BIN", &[7u8; 5000])
            .build();
        // 第二 FAT 区（fat_offset+fat_length = 26 扇区起）
        let second = (24 + 2) * 512;
        img[second + 6 * 4..second + 6 * 4 + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        patch_boot_both(
            &mut img,
            &[(110, &2u8.to_le_bytes()), (106, &1u16.to_le_bytes())],
        );
        let (_f, dev) = dev_for(&img);
        let boot = boot::parse(&dev).unwrap();
        assert_eq!(boot.active_fat_index(), 1);
        let fat = Fat32::new(&dev, &boot);
        assert_eq!(fat.chain(6).unwrap(), vec![6]);
    }
}
