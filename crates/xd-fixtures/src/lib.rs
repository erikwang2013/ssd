// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 合成 FAT 镜像构建器：测试与 e2e 的全部输入来源（脱开真实硬件）。
//! 参数固定（T1 只支持 FAT16；T2 扩展 FAT12/32）：bps=512、spc=1、reserved=1、
//! fats=1、root_entries=512、fat_size=4 扇区、total=4096 扇区（2 MiB）。

pub const BPS: u32 = 512; // bytes per sector
pub const TOTAL_SECTORS: u32 = 4096;

#[derive(Clone)]
struct BuildFile {
    dir: String,    // "/" 或 "/SUB"（T1 仅 "/"）
    name: [u8; 11], // 8.3 原始名（大写、空格填充）
    data: Vec<u8>,
    deleted: bool,
    first_cluster: u32, // 预留：build 期分配结果经 placed 传递，字段供后续任务读取
}

pub struct FatImageBuilder {
    files: Vec<BuildFile>,
}

impl FatImageBuilder {
    pub fn fat16() -> Self {
        Self { files: Vec::new() }
    }

    pub fn add_file(&mut self, dir: &str, name: &str, data: &[u8]) -> &mut Self {
        self.files.push(BuildFile {
            dir: dir.to_string(),
            name: encode_sfn(name),
            data: data.to_vec(),
            deleted: false,
            first_cluster: 0,
        });
        self
    }

    pub fn delete(&mut self, dir: &str, name: &str) -> &mut Self {
        let target = encode_sfn(name);
        let f = self
            .files
            .iter_mut()
            .find(|f| !f.deleted && f.dir == dir && f.name == target)
            .expect("delete: file not found");
        f.deleted = true;
        self
    }

    pub fn build(&self) -> Vec<u8> {
        // 布局（扇区）：
        //  0        reserved / 引导扇区
        //  1..5     FAT#1（4 扇区）
        //  5..37    根目录（512 项 × 32B = 32 扇区）
        //  37..    数据区（簇 N → 扇区 37 + (N-2)）
        const FAT_START: u32 = 1;
        const FAT_SIZE: u32 = 4;
        const ROOT_START: u32 = FAT_START + FAT_SIZE; // 5
        const ROOT_SECTORS: u32 = 32;
        const DATA_START: u32 = ROOT_START + ROOT_SECTORS; // 37
        const MAX_CLUSTER: u32 = 2 + (TOTAL_SECTORS - DATA_START); // spc=1

        let mut image = vec![0u8; (TOTAL_SECTORS * BPS) as usize];

        // ---- 分配簇（最低空闲优先；删除释放后会被后续文件复用）----
        let mut next_free: Vec<u32> = (2..MAX_CLUSTER).collect();
        let mut placed: Vec<(u32, u32, bool, Vec<u8>, usize)> = Vec::new(); // (first,count,deleted,data,file_idx)
        for (i, f) in self.files.iter().enumerate() {
            let _ = f.first_cluster; // 预留字段（见 BuildFile 注释）
            let count = ((f.data.len() as u32) + BPS - 1).max(1) / BPS;
            let take: Vec<u32> = next_free.drain(..count as usize).collect();
            let first = take[0];
            if !f.deleted {
                // 存活文件写 FAT 链（末簇 EOC=0xFFFF）；删除文件不写链（已释放）
                for (j, &c) in take.iter().enumerate() {
                    let entry_off = (FAT_START * BPS + c * 2) as usize;
                    let value: u16 = if j + 1 == take.len() {
                        0xFFFF
                    } else {
                        take[j + 1] as u16
                    };
                    image[entry_off..entry_off + 2].copy_from_slice(&value.to_le_bytes());
                }
            } else {
                // 释放：把簇还回空闲池（插回列表低端以模拟"优先复用"）
                for c in take.iter().rev() {
                    next_free.insert(0, *c);
                }
            }
            placed.push((first, count, f.deleted, f.data.clone(), i));
        }

        // ---- 写数据 ----
        for (first, count, _, data, _) in &placed {
            let start = (DATA_START * BPS + (first - 2) * BPS) as usize;
            let _ = count;
            image[start..start + data.len()].copy_from_slice(data);
        }

        // ---- 根目录项（32B 槽；删除项首字节 0xE5）----
        let mut slot = ROOT_START * BPS;
        for (first, _count, deleted, data, idx) in &placed {
            let f = &self.files[*idx];
            let mut entry = [0u8; 32];
            entry[..11].copy_from_slice(&f.name);
            if *deleted {
                entry[0] = 0xE5;
            }
            entry[11] = 0x20; // ATTR_ARCHIVE
            entry[26..28].copy_from_slice(&(*first as u16).to_le_bytes());
            entry[28..32].copy_from_slice(&(data.len() as u32).to_le_bytes());
            image[slot as usize..slot as usize + 32].copy_from_slice(&entry);
            slot += 32;
        }

        // ---- 引导扇区 ----
        let mut bs = [0u8; 512];
        bs[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
        bs[3..11].copy_from_slice(b"MSDOS5.0");
        bs[11..13].copy_from_slice(&(BPS as u16).to_le_bytes());
        bs[13] = 1; // sectors per cluster
        bs[14..16].copy_from_slice(&1u16.to_le_bytes()); // reserved
        bs[16] = 1; // num fats
        bs[17..19].copy_from_slice(&512u16.to_le_bytes()); // root entries
        bs[19..21].copy_from_slice(&0u16.to_le_bytes()); // total16 = 0（用 total32）
        bs[21] = 0xF8; // media descriptor
        bs[22..24].copy_from_slice(&(FAT_SIZE as u16).to_le_bytes());
        bs[24..26].copy_from_slice(&63u16.to_le_bytes()); // sectors per track（惯例值）
        bs[26..28].copy_from_slice(&255u16.to_le_bytes()); // heads
        bs[28..32].copy_from_slice(&37u32.to_le_bytes()); // hidden sectors（惯例）
        bs[32..36].copy_from_slice(&TOTAL_SECTORS.to_le_bytes());
        bs[36] = 0x80; // drive number
        bs[38] = 0x29; // boot signature
        bs[39..43].copy_from_slice(&0x1234_5678u32.to_le_bytes()); // volume id
        bs[43..54].copy_from_slice(b"XIAODUN    ");
        bs[54..62].copy_from_slice(b"FAT16   ");
        bs[510] = 0x55;
        bs[511] = 0xAA;
        image[..512].copy_from_slice(&bs);

        image
    }
}

/// "HELLO.TXT" → b"HELLO   TXT"（大写、空格填充、无扩展名时全空格）
pub fn encode_sfn(name: &str) -> [u8; 11] {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    let mut out = [b' '; 11];
    for (i, c) in base.bytes().take(8).enumerate() {
        out[i] = c.to_ascii_uppercase();
    }
    for (i, c) in ext.bytes().take(3).enumerate() {
        out[8 + i] = c.to_ascii_uppercase();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fat16_layout_bytes_are_sane() {
        let image = FatImageBuilder::fat16()
            .add_file("/", "HELLO.TXT", b"hello world")
            .build();
        // 引导扇区
        assert_eq!(image[510], 0x55);
        assert_eq!(image[511], 0xAA);
        assert_eq!(u16::from_le_bytes([image[11], image[12]]), 512); // bytes/sector
        assert_eq!(image[13], 1); // sectors/cluster
        assert_eq!(u16::from_le_bytes([image[17], image[18]]), 512); // root entries
        assert_eq!(image[16], 1); // num fats
        // 文件内容落盘（数据从 data_start 之后的簇 2 开始）
        let pos = image.windows(11).position(|w| w == b"hello world").unwrap();
        assert!(pos > 512);
        // 目录项：存活文件首字节 'H'，属性 0x20
        let de = image
            .windows(32)
            .position(|w| &w[..11] == b"HELLO   TXT")
            .unwrap();
        assert_eq!(image[de + 11], 0x20);
    }

    #[test]
    fn deleted_file_has_0xe5_and_freed_fat() {
        let image = FatImageBuilder::fat16()
            .add_file("/", "A.BIN", &[1u8; 1000])
            .delete("/", "A.BIN")
            .build();
        let de = image
            .windows(32)
            .position(|w| w[0] == 0xE5 && &w[8..11] == b"BIN")
            .unwrap();
        assert_eq!(image[de], 0xE5);
        // FAT 里原文件两簇已被释放（entry(2)=0, entry(3)=0）
        let fat_start = 512usize; // reserved=1 → FAT 在第 2 个扇区
        assert_eq!(
            u16::from_le_bytes([image[fat_start + 4], image[fat_start + 5]]),
            0
        );
        assert_eq!(
            u16::from_le_bytes([image[fat_start + 6], image[fat_start + 7]]),
            0
        );
    }

    #[test]
    fn reuse_of_freed_clusters_overlaps() {
        let image = FatImageBuilder::fat16()
            .add_file("/", "OLD.BIN", &[7u8; 1024]) // 簇 2..3
            .delete("/", "OLD.BIN")
            .add_file("/", "NEW.BIN", &[9u8; 1024]) // 复用 2..3
            .build();
        // 删除项首字节 0xE5，其后是 SFN 的 "LD     BIN"（8.3 原名不存点）
        let old = image.windows(32).position(|w| &w[1..5] == b"LD  ").unwrap();
        assert_eq!(image[old], 0xE5);
        // 第一个数据簇现在属于 NEW.BIN：NEW 目录项 first_cluster == 2
        let new = image
            .windows(32)
            .position(|w| &w[..7] == b"NEW    ")
            .unwrap();
        assert_eq!(u16::from_le_bytes([image[new + 26], image[new + 27]]), 2);
    }
}
