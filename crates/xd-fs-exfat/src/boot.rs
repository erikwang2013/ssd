// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! exFAT 引导区解析：EXFAT 签名分派、几何交叉校验、BootChecksum（跳 106/107/112）、
//! Main 失败回退 Backup（backup_used=true，scan 据此降级）。
//! 实证：跳过规则与真实 mkfs.exfat 产物 stored==calc 验证一致（见 M1a2 计划头部）。

use crate::ExfatError;
use xd_device::BlockDevice;

const EXFAT_SIG: &[u8; 8] = b"EXFAT   ";

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExfatBoot {
    pub bytes_per_sector_shift: u8,
    pub sectors_per_cluster_shift: u8,
    pub number_of_fats: u8,
    pub fat_offset: u32,
    pub fat_length: u32,
    pub cluster_heap_offset: u32,
    pub cluster_count: u32,
    pub root_cluster: u32,
    pub volume_length: u64,
    pub volume_flags: u16,
    /// Main 区校验失败、几何取自 Backup 区（卷级降级信号）。
    pub backup_used: bool,
}

impl ExfatBoot {
    pub fn sector_bytes(&self) -> u64 {
        1u64 << self.bytes_per_sector_shift
    }
    pub fn cluster_bytes(&self) -> u64 {
        self.sector_bytes() << self.sectors_per_cluster_shift
    }
    /// # Panics
    /// `cluster` 必须满足 `2 <= cluster <= cluster_count + 1`（与 M1a `Bpb::cluster_to_byte` 同契约）。
    pub fn cluster_to_byte(&self, cluster: u32) -> u64 {
        self.cluster_heap_offset as u64 * self.sector_bytes()
            + (cluster as u64 - 2) * self.cluster_bytes()
    }
    pub fn active_fat_index(&self) -> u8 {
        if self.number_of_fats == 2 && self.volume_flags & 0x1 == 1 {
            1
        } else {
            0
        }
    }
    pub fn active_fat_offset(&self) -> u32 {
        self.fat_offset + self.active_fat_index() as u32 * self.fat_length
    }
}

fn u16le(s: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([s[o], s[o + 1]])
}
fn u32le(s: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]])
}
fn u64le(s: &[u8], o: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&s[o..o + 8]);
    u64::from_le_bytes(b)
}

/// 32 位"循环右移 1 位累加"，跳过区域内绝对字节 106/107/112（VolumeFlags + PercentInUse）。
fn boot_checksum(region: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for (i, b) in region.iter().enumerate() {
        if i == 106 || i == 107 || i == 112 {
            continue;
        }
        sum = (if sum & 1 != 0 { 0x8000_0000u32 } else { 0u32 })
            .wrapping_add(sum >> 1)
            .wrapping_add(*b as u32);
    }
    sum
}

/// 校验一个引导区域的 checksum 扇区（区域起点 sector_offset，取所在 11 扇区范围）。
fn region_valid(dev: &dyn BlockDevice, sector_offset: u64, bps: u64) -> Result<bool, ExfatError> {
    let mut region = vec![0u8; (bps * 11) as usize];
    let n = read_at(dev, sector_offset * bps, &mut region)?;
    if n < region.len() {
        return Ok(false); // 读不满 → 视为无效（不是 Err：可能只是 Backup 在设备外）
    }
    let mut cks = [0u8; 4];
    if read_at(dev, (sector_offset + 11) * bps, &mut cks)? < 4 {
        return Ok(false);
    }
    Ok(boot_checksum(&region) == u32::from_le_bytes(cks))
}

fn read_at(dev: &dyn BlockDevice, off: u64, buf: &mut [u8]) -> Result<usize, ExfatError> {
    dev.read_at(off, buf)
        .map_err(|e| ExfatError::Io(e.to_string()))
}

/// 从引导扇区字节解析几何并做交叉校验（不含 checksum）。
fn geometry(sector: &[u8]) -> Result<ExfatBoot, ExfatError> {
    if &sector[3..11] != EXFAT_SIG {
        return Err(ExfatError::InvalidBoot("EXFAT 签名不符".into()));
    }
    let volume_length = u64le(sector, 72);
    let fat_offset = u32le(sector, 80);
    let fat_length = u32le(sector, 84);
    let cluster_heap_offset = u32le(sector, 88);
    let cluster_count = u32le(sector, 92);
    let root_cluster = u32le(sector, 96);
    let volume_flags = u16le(sector, 106);
    let bytes_per_sector_shift = sector[108];
    let sectors_per_cluster_shift = sector[109];
    let number_of_fats = sector[110];

    let bad = |m: &str| ExfatError::InvalidBoot(m.to_string());
    if !(9..=12).contains(&bytes_per_sector_shift) {
        return Err(bad(&format!(
            "bytes_per_sector_shift 越界: {bytes_per_sector_shift}"
        )));
    }
    if sectors_per_cluster_shift > 25 - bytes_per_sector_shift {
        return Err(bad(&format!(
            "sectors_per_cluster_shift 越界: {sectors_per_cluster_shift}"
        )));
    }
    if number_of_fats != 1 && number_of_fats != 2 {
        return Err(bad(&format!("number_of_fats 非法: {number_of_fats}")));
    }
    if fat_offset < 24 || fat_length < 1 {
        return Err(bad(&format!(
            "fat_offset/fat_length 非法: {fat_offset}/{fat_length}"
        )));
    }
    let sector_bytes = 1u64 << bytes_per_sector_shift;
    let heap_min = fat_offset as u64 + fat_length as u64 * number_of_fats as u64;
    if (cluster_heap_offset as u64) < heap_min {
        return Err(bad("cluster_heap_offset 小于 FAT 区末端"));
    }
    if volume_length < cluster_heap_offset as u64 {
        return Err(bad("volume_length 小于 cluster_heap_offset"));
    }
    // 上界 2^32−11：不得把 BAD_CLUSTER(0xFFFFFFF7) 等保留值放进合法簇域
    if !(1..=0xFFFF_FFF5).contains(&cluster_count) {
        return Err(bad(&format!("cluster_count 越界: {cluster_count}")));
    }
    if !(2..=cluster_count as u64 + 1).contains(&(root_cluster as u64)) {
        return Err(bad(&format!("root_cluster 越界: {root_cluster}")));
    }
    let fat_min_bytes = ((cluster_count as u64 + 2) * 4).div_ceil(sector_bytes);
    if fat_length as u64 * sector_bytes < fat_min_bytes {
        return Err(bad("fat_length 不足以容纳 cluster_count+2 个表项"));
    }
    // cluster_count 与簇堆的精确关系：真实卡偶见 ±1，宽容接受（doc 说明，不拒绝）
    Ok(ExfatBoot {
        bytes_per_sector_shift,
        sectors_per_cluster_shift,
        number_of_fats,
        fat_offset,
        fat_length,
        cluster_heap_offset,
        cluster_count,
        root_cluster,
        volume_length,
        volume_flags,
        backup_used: false,
    })
}

pub fn parse(dev: &dyn BlockDevice) -> Result<ExfatBoot, ExfatError> {
    // 先读 512 字节拿 bps_shift（所有合法 exFAT 引导扇区这部分相同）
    let mut head = [0u8; 512];
    if read_at(dev, 0, &mut head)? < 512 {
        return Err(ExfatError::InvalidBoot("设备过短，读不到引导扇区".into()));
    }
    if &head[3..11] != EXFAT_SIG {
        return Err(ExfatError::InvalidBoot("EXFAT 签名不符".into()));
    }
    let bps_shift = head[108];
    if !(9..=12).contains(&bps_shift) {
        return Err(ExfatError::InvalidBoot(format!(
            "bytes_per_sector_shift 越界: {bps_shift}"
        )));
    }
    let bps = 1u64 << bps_shift;
    // bps > 512 时补齐再解析
    let mut sector = head.to_vec();
    if bps > 512 {
        sector.resize(bps as usize, 0);
        if read_at(dev, 0, &mut sector)? < bps as usize {
            return Err(ExfatError::InvalidBoot("引导扇区读取不完整".into()));
        }
    }
    // 主区优先；校验失败回退备区（比内核宽容：内核只校验 Main）
    if region_valid(dev, 0, bps)? {
        return geometry(&sector);
    }
    let mut backup = vec![0u8; bps as usize];
    if read_at(dev, 12 * bps, &mut backup)? < bps as usize || !region_valid(dev, 12, bps)? {
        return Err(ExfatError::InvalidBoot(
            "Main 与 Backup 引导区校验均失败".into(),
        ));
    }
    let mut boot = geometry(&backup)?;
    boot.backup_used = true;
    Ok(boot)
}

#[cfg(test)]
pub(crate) mod testutil {
    use xd_device::image::ImageFileDevice;

    pub(crate) fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(image).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        (f, dev)
    }

    /// 同时对主/备引导区打补丁并**重算两个 Boot Checksum 扇区**（保持镜像自洽）。
    pub(crate) fn patch_boot_both(img: &mut [u8], patches: &[(usize, &[u8])]) {
        for region in [0usize, 12] {
            for (off, bytes) in patches {
                let o = region * 512 + off;
                img[o..o + bytes.len()].copy_from_slice(bytes);
            }
            // 重算该区域 checksum 扇区
            let sum = xd_fixtures::boot_checksum(&img[region * 512..region * 512 + 512 * 11]);
            let cb = (region + 11) * 512;
            for k in 0..128 {
                img[cb + k * 4..cb + k * 4 + 4].copy_from_slice(&sum.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot::testutil::{dev_for, patch_boot_both};

    #[test]
    fn parse_ok_fixture() {
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let b = parse(&dev).unwrap();
        assert_eq!(b.bytes_per_sector_shift, 9);
        assert_eq!(b.sectors_per_cluster_shift, 3);
        assert_eq!(b.number_of_fats, 1);
        assert_eq!(b.fat_offset, 24);
        assert_eq!(b.fat_length, 2);
        assert_eq!(b.cluster_heap_offset, 32);
        assert_eq!(b.cluster_count, 252);
        assert_eq!(b.root_cluster, 5);
        assert!(!b.backup_used);
    }

    #[test]
    fn geometry_helpers() {
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let b = parse(&dev).unwrap();
        assert_eq!(b.sector_bytes(), 512);
        assert_eq!(b.cluster_bytes(), 4096);
        assert_eq!(b.cluster_to_byte(2), 32 * 512);
        assert_eq!(b.cluster_to_byte(5), 32 * 512 + 3 * 4096);
        assert_eq!(b.cluster_to_byte(253), 32 * 512 + 251 * 4096); // 末簇 = heap + (count-1)*cb
    }

    #[test]
    fn rejects_non_exfat() {
        let (_f, dev) = dev_for(&vec![0u8; 4096]);
        assert!(matches!(parse(&dev), Err(ExfatError::InvalidBoot(m)) if m.contains("EXFAT")));
    }

    #[test]
    fn rejects_bad_geometry() {
        let cases: &[(usize, Vec<u8>, &str, &str)] = &[
            (108, vec![8], "bytes_per_sector_shift", "bps_shift=8"),
            (109, vec![17], "sectors_per_cluster_shift", "spc_shift=17"),
            (110, vec![3], "number_of_fats", "number_of_fats=3"),
            (
                80,
                23u32.to_le_bytes().to_vec(),
                "fat_offset",
                "fat_offset=23",
            ),
            (
                92,
                0u32.to_le_bytes().to_vec(),
                "cluster_count",
                "cluster_count=0",
            ),
            (
                96,
                300u32.to_le_bytes().to_vec(),
                "root_cluster",
                "root=300",
            ),
        ];
        for (off, bytes, expect, what) in cases {
            let mut img = xd_fixtures::ExfatImageBuilder::new().build();
            patch_boot_both(&mut img, &[(*off, bytes.as_slice())]);
            let (_f, dev) = dev_for(&img);
            assert!(
                matches!(parse(&dev), Err(ExfatError::InvalidBoot(m)) if m.contains(expect)),
                "{what}: 实际 {:?}",
                parse(&dev)
            );
        }
    }

    #[test]
    fn rejects_bad_bps_shift_without_panic() {
        // 108=64：修复前 debug 移位溢出 panic / release 容量溢出
        let mut img = xd_fixtures::ExfatImageBuilder::new().build();
        patch_boot_both(&mut img, &[(108, &64u8.to_le_bytes())]);
        let (_f, dev) = dev_for(&img);
        assert!(
            matches!(parse(&dev), Err(ExfatError::InvalidBoot(m)) if m.contains("bytes_per_sector_shift"))
        );
    }

    #[test]
    fn backup_geometry_bps_guard_load_bearing() {
        // 主区失效（byte80 篡改）+ 备区 byte108=13（按 512 宽重算备区 checksum）
        // → 必须拒绝备区几何，不得返回 Ok{bps_shift:13}
        let mut img = xd_fixtures::ExfatImageBuilder::new().build();
        img[80] = 25;
        img[12 * 512 + 108] = 13;
        let sum = xd_fixtures::boot_checksum(&img[12 * 512..12 * 512 + 512 * 11]);
        for k in 0..128 {
            img[(12 + 11) * 512 + k * 4..(12 + 11) * 512 + k * 4 + 4]
                .copy_from_slice(&sum.to_le_bytes());
        }
        let (_f, dev) = dev_for(&img);
        assert!(
            matches!(parse(&dev), Err(ExfatError::InvalidBoot(m)) if m.contains("bytes_per_sector_shift"))
        );
    }

    #[test]
    fn boot_checksum_mismatch_falls_back_to_backup() {
        let mut img = xd_fixtures::ExfatImageBuilder::new().build();
        // 只改主区（fat_offset 24→25，仍结构合法），不重算主区 checksum → 主区校验失败
        img[80] = 25;
        let (_f, dev) = dev_for(&img);
        let b = parse(&dev).unwrap();
        assert!(b.backup_used, "必须回退 Backup 几何");
        assert_eq!(b.fat_offset, 24, "几何来自 Backup（未被污染）");
    }

    #[test]
    fn both_regions_bad_is_err() {
        let mut img = xd_fixtures::ExfatImageBuilder::new().build();
        img[200] ^= 0xFF; // 主区
        img[12 * 512 + 200] ^= 0xFF; // 备区
        let (_f, dev) = dev_for(&img);
        assert!(matches!(parse(&dev), Err(ExfatError::InvalidBoot(_))));
    }

    #[test]
    fn huge_volume_length_no_overflow() {
        // u64 几何：极大 volume_length 不得 panic/溢出；cluster_count 偏差只降容忍不硬拒
        let mut img = xd_fixtures::ExfatImageBuilder::new().build();
        let huge = (0xFFFF_FFFF_FFFF_FFFFu64).to_le_bytes();
        patch_boot_both(&mut img, &[(72, &huge)]);
        let (_f, dev) = dev_for(&img);
        let b = parse(&dev).unwrap(); // 不 panic 即达标（宽容语义见 doc）
        assert_eq!(b.cluster_count, 252);
        assert_eq!(b.volume_length, u64::MAX);
    }

    #[test]
    fn active_fat_second_when_flagged() {
        let mut img = xd_fixtures::ExfatImageBuilder::new().build();
        patch_boot_both(
            &mut img,
            &[(110, &2u8.to_le_bytes()), (106, &1u16.to_le_bytes())],
        ); // texFAT + ActiveFat=1
        let (_f, dev) = dev_for(&img);
        let b = parse(&dev).unwrap();
        assert_eq!(b.number_of_fats, 2);
        assert_eq!(b.active_fat_index(), 1);
        // active_fat_offset：Second FAT 起于 fat_offset + fat_length
        assert_eq!(b.active_fat_offset(), 24 + 2);
    }

    #[test]
    fn active_fat_first_by_default() {
        let image = xd_fixtures::ExfatImageBuilder::new().build();
        let (_f, dev) = dev_for(&image);
        let b = parse(&dev).unwrap();
        assert_eq!(b.active_fat_index(), 0);
        assert_eq!(b.active_fat_offset(), 24);
    }
}
