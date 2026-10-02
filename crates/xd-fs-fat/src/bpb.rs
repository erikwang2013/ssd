// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 引导扇区（BPB）解析与几何计算。
//! FAT32 判定：结构优先（root_entry_count == 0 && fat16_size == 0 && root_cluster >= 2），
//! 否则按数据区簇数 4085/65525 区分 FAT12/16——与 fatfs 等主流实现一致。

use crate::FatError;
use xd_device::BlockDevice;

pub const SECTOR0_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Debug, Clone)]
pub struct Bpb {
    pub fat_type: FatType,
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u32,
    pub num_fats: u8,
    pub root_entry_count: u16,
    pub total_sectors: u32,
    pub fat_size_sectors: u32,
    pub root_cluster: u32,
    pub fat_start_sector: u32,
    pub root_start_sector: u32, // FAT12/16 专用；FAT32 为 0
    pub data_start_sector: u32,
    pub data_sectors: u32,
}

impl Bpb {
    pub fn cluster_bytes(&self) -> u32 {
        self.bytes_per_sector as u32 * self.sectors_per_cluster as u32
    }

    /// # Panics
    /// `cluster` 必须满足 `2 <= cluster <= data_cluster_count() + 1`（0/1 为保留簇）。
    pub fn cluster_to_sector(&self, cluster: u32) -> u32 {
        self.data_start_sector + (cluster - 2) * self.sectors_per_cluster as u32
    }

    pub fn cluster_to_byte(&self, cluster: u32) -> u64 {
        self.cluster_to_sector(cluster) as u64 * self.bytes_per_sector as u64
    }

    pub fn data_cluster_count(&self) -> u32 {
        self.data_sectors / self.sectors_per_cluster as u32
    }

    /// # Panics
    /// FAT12 上调用即 panic（编程错误，非输入错误）。
    pub fn fat_entry_byte(&self, cluster: u32) -> u64 {
        let per_entry: u64 = match self.fat_type {
            FatType::Fat32 => 4,
            FatType::Fat16 => 2,
            FatType::Fat12 => unreachable!("FAT12 用 12 位寻址，请走 Fat::entry 的专用路径"),
        };
        self.fat_start_sector as u64 * self.bytes_per_sector as u64 + cluster as u64 * per_entry
    }
}

pub fn parse(dev: &dyn BlockDevice) -> Result<Bpb, FatError> {
    let mut sector0 = [0u8; SECTOR0_LEN];
    let n = dev.read_at(0, &mut sector0)?;
    if n < SECTOR0_LEN {
        return Err(FatError::InvalidBpb("image smaller than 512 bytes".into()));
    }
    if sector0[510] != 0x55 || sector0[511] != 0xAA {
        return Err(FatError::InvalidBpb("missing 0x55AA boot signature".into()));
    }
    let bytes_per_sector = u16::from_le_bytes([sector0[11], sector0[12]]);
    if ![512u16, 1024, 2048, 4096].contains(&bytes_per_sector) {
        return Err(FatError::InvalidBpb(format!(
            "bad bytes per sector: {bytes_per_sector}"
        )));
    }
    let sectors_per_cluster = sector0[13];
    if sectors_per_cluster == 0
        || !sectors_per_cluster.is_power_of_two()
        || sectors_per_cluster > 128
    {
        return Err(FatError::InvalidBpb(format!(
            "bad sectors per cluster: {sectors_per_cluster}"
        )));
    }
    let reserved_sectors = u16::from_le_bytes([sector0[14], sector0[15]]) as u32;
    let num_fats = sector0[16];
    if num_fats == 0 {
        return Err(FatError::InvalidBpb("zero fat count".into()));
    }
    let root_entry_count = u16::from_le_bytes([sector0[17], sector0[18]]);
    let total_sectors = {
        let t16 = u16::from_le_bytes([sector0[19], sector0[20]]) as u32;
        let t32 = u32::from_le_bytes([sector0[32], sector0[33], sector0[34], sector0[35]]);
        if t16 != 0 { t16 } else { t32 }
    };
    if total_sectors == 0 {
        return Err(FatError::InvalidBpb("zero total sectors".into()));
    }
    let fat16_size = u16::from_le_bytes([sector0[22], sector0[23]]) as u32;
    let fat32_size =
        u32::from_le_bytes([sector0[36], sector0[37], sector0[38], sector0[39]]) & 0x0FFF_FFFF;

    // 44..48 仅在 FAT32 是 root_cluster；FAT12/16 该区间属卷标，须按结构分支才读。
    let (fat_type, fat_size_sectors, root_cluster) = if root_entry_count == 0 && fat16_size == 0 {
        let root_cluster =
            u32::from_le_bytes([sector0[44], sector0[45], sector0[46], sector0[47]]) & 0x0FFF_FFFF;
        if root_cluster < 2 {
            return Err(FatError::InvalidBpb("fat32 without root cluster".into()));
        }
        if fat32_size == 0 {
            return Err(FatError::InvalidBpb("fat32 without fat size".into()));
        }
        (FatType::Fat32, fat32_size, root_cluster)
    } else {
        if fat16_size == 0 {
            return Err(FatError::InvalidBpb("no fat size".into()));
        }
        (FatType::Fat16, fat16_size, 0) // 先按 16 占位，下面按簇数改为 12
    };

    let root_sectors = ((root_entry_count as u32) * 32).div_ceil(bytes_per_sector as u32);
    let fat_start_sector = reserved_sectors;
    let root_start_sector = if fat_type == FatType::Fat32 {
        0
    } else {
        fat_start_sector + fat_size_sectors * num_fats as u32
    };
    let data_start_sector = if fat_type == FatType::Fat32 {
        // FAT32 的 fat_size 是 28 位，u32 直乘可溢出——u64 计算并顺带完成越界检查
        let start = fat_start_sector as u64 + fat_size_sectors as u64 * num_fats as u64;
        if start >= total_sectors as u64 {
            return Err(FatError::InvalidBpb("data area beyond device".into()));
        }
        start as u32
    } else {
        let start = root_start_sector + root_sectors;
        if start >= total_sectors {
            return Err(FatError::InvalidBpb("data area beyond device".into()));
        }
        start
    };
    let data_sectors = total_sectors - data_start_sector;

    let fat_type = if fat_type == FatType::Fat32 {
        FatType::Fat32
    } else {
        let clusters = data_sectors / sectors_per_cluster as u32;
        if clusters < 4085 {
            FatType::Fat12
        } else {
            FatType::Fat16
        }
    };

    Ok(Bpb {
        fat_type,
        bytes_per_sector,
        sectors_per_cluster,
        reserved_sectors,
        num_fats,
        root_entry_count,
        total_sectors,
        fat_size_sectors,
        root_cluster,
        fat_start_sector,
        root_start_sector,
        data_start_sector,
        data_sectors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xd_device::image::ImageFileDevice;

    fn device_with(bytes: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        (f, dev)
    }

    #[test]
    fn parses_fat16_builder_image() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "A.TXT", b"x")
            .build();
        let (_f, dev) = device_with(&image);
        let bpb = parse(&dev).unwrap();
        assert_eq!(bpb.fat_type, FatType::Fat16);
        assert_eq!(bpb.bytes_per_sector, 512);
        assert_eq!(bpb.root_entry_count, 512);
        assert_eq!(bpb.root_cluster, 0);
        assert_eq!(bpb.data_start_sector, 50);
        assert_eq!(bpb.total_sectors, 4224);
    }

    #[test]
    fn parses_fat32_structurally() {
        let image = xd_fixtures::FatImageBuilder::fat32()
            .add_file("/", "A.TXT", b"x")
            .build();
        let (_f, dev) = device_with(&image);
        let bpb = parse(&dev).unwrap();
        assert_eq!(bpb.fat_type, FatType::Fat32);
        assert_eq!(bpb.root_entry_count, 0);
        assert_eq!(bpb.root_cluster, 2);
        assert_eq!(bpb.data_start_sector, 96);
    }

    #[test]
    fn parses_fat12_by_cluster_count() {
        let image = xd_fixtures::FatImageBuilder::fat12()
            .add_file("/", "A.TXT", b"x")
            .build();
        let (_f, dev) = device_with(&image);
        let bpb = parse(&dev).unwrap();
        assert_eq!(bpb.fat_type, FatType::Fat12);
    }

    #[test]
    fn rejects_bad_magic() {
        let image = xd_fixtures::FatImageBuilder::fat16().build();
        let mut bad = image.clone();
        bad[510] = 0;
        let (_f, dev) = device_with(&bad);
        assert!(matches!(parse(&dev), Err(FatError::InvalidBpb(_))));
    }

    #[test]
    fn rejects_bad_bytes_per_sector() {
        let image = xd_fixtures::FatImageBuilder::fat16().build();
        let mut bad = image.clone();
        bad[11] = 0;
        bad[12] = 1; // 256，非法
        let (_f, dev) = device_with(&bad);
        assert!(matches!(parse(&dev), Err(FatError::InvalidBpb(_))));
    }

    #[test]
    fn cluster_and_byte_math() {
        let image = xd_fixtures::FatImageBuilder::fat16().build();
        let (_f, dev) = device_with(&image);
        let bpb = parse(&dev).unwrap();
        assert_eq!(bpb.cluster_to_sector(2), 50);
        assert_eq!(bpb.cluster_to_sector(3), 51);
        assert_eq!(bpb.cluster_bytes(), 512);
        assert_eq!(bpb.data_cluster_count(), 4174);
    }

    #[test]
    fn total_sectors16_branch() {
        // 真实 FAT16 盘走 t16 分支（夹具恒写 total16=0+total32）
        let image = xd_fixtures::FatImageBuilder::fat16().build();
        let mut patched = image.clone();
        patched[19..21].copy_from_slice(&4224u16.to_le_bytes()); // total16
        patched[32..36].copy_from_slice(&0u32.to_le_bytes()); // total32 = 0 → 强制走 t16
        let (_f, dev) = device_with(&patched);
        let bpb = parse(&dev).unwrap();
        assert_eq!(bpb.total_sectors, 4224);
        assert_eq!(bpb.fat_type, FatType::Fat16);
    }

    #[test]
    fn rejects_overflowing_fat_geometry() {
        let image = xd_fixtures::FatImageBuilder::fat32().build();
        let mut bad = image.clone();
        bad[16] = 0xFF; // num_fats = 255
        bad[36..40].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes()); // fat32_size 拉满
        let (_f, dev) = device_with(&bad);
        assert!(matches!(parse(&dev), Err(FatError::InvalidBpb(_))));
    }

    #[test]
    fn fat_entry_byte_offsets() {
        let image = xd_fixtures::FatImageBuilder::fat16().build();
        let (_f, dev) = device_with(&image);
        let bpb = parse(&dev).unwrap();
        assert_eq!(bpb.fat_entry_byte(2), 516); // fat_start=1 → 512 + 2*2
        let image32 = xd_fixtures::FatImageBuilder::fat32().build();
        let (_f2, dev32) = device_with(&image32);
        let bpb32 = parse(&dev32).unwrap();
        assert_eq!(bpb32.fat_entry_byte(2), 16392); // 32*512 + 2*4
    }
}
