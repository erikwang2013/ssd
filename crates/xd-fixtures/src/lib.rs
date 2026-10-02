// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 合成 FAT 镜像构建器：测试与 e2e 的全部输入来源（脱开真实硬件）。
//! 三类型布局见 [`FatType::layout`]；公共参数 bps=512、spc=1、fats=1。
//! FAT16：root 512 项、FAT 17 扇区、total 4224 扇区，4174 数据簇 ≥ 4085 →
//! 按微软簇数规则也是真 FAT16（避免真实驱动判为 FAT12）。
//!
//! 契约（测试作者必读）：
//! 1. 写入历史按调用顺序模拟：delete() 的时刻记为 files.len()，该文件的簇在
//!    「下一次 add 时」才被释放 → 只有删除**之后**添加的文件才可能复用其簇
//!    （与真实盘一致）。
//! 2. M1a 恢复按「连续簇假设」：若已删文件之后又有新文件绕过空洞占用其邻簇
//!    （碎裂），连续回退读可能读到他人的字节——这是刻意的夹具边界，勿在
//!    T6/T8 构造该形状（真实 FAT 恢复工具同样有此盲区）。
//!
//! 夹具边界（测试作者必读二）：
//! - 子目录含真实 "." / ".." 目录项；扫描器必须跳过（T5 parse_directory_bytes 已处理）。
//! - 目录簇取最低空闲簇并写 EOC（单簇目录，不产生多簇目录链）。
//! - 容量上限（超限触发 fail-fast 断言）：FAT32 根 16 项 / 子目录 14 成员 /
//!   FAT12 根 224 项 / FAT16 根 512 项；簇池 FAT12 1006 / FAT16 4174 /
//!   FAT32 1952（根目录占 1 → 文件可用 1951）。
//! - FAT32 为简化版扩展 BPB（fsinfo 声明但空、16 位簇字段、单 FAT 副本、
//!   簇数 < 65525 → 真实驱动会拒认；本引擎结构优先判型不受影响）。
//! - encode_sfn 只接受 ASCII ≤ 8.3（超长/非 ASCII 由 debug_assert 拦截）。

pub const BPS: u32 = 512; // bytes per sector
pub const TOTAL_SECTORS: u32 = 4224; // FAT16 布局的 total（FAT12/32 见 FatType::layout）

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

impl FatType {
    /// (root_entries, fat_size_sectors, reserved, root_cluster, total_sectors)
    fn layout(self) -> (u16, u32, u32, u32, u32) {
        match self {
            FatType::Fat12 => (224, 3, 1, 0, 1024),
            FatType::Fat16 => (512, 17, 1, 0, 4224),
            FatType::Fat32 => (0, 64, 32, 2, 2048),
        }
    }
}

struct BuildFile {
    dir: String,    // "/" 或 "/SUB"（一级子目录）
    name: [u8; 11], // 8.3 原始名（大写、空格填充）
    data: Vec<u8>,
    /// Some(时刻)：在该时刻（= delete() 调用时的 files.len()）释放其簇；None = 存活。
    /// 按真实时序释放，保证「先建后删」的文件各自可恢复。
    deleted_at: Option<usize>,
}

pub struct FatImageBuilder {
    fat_type: FatType,
    files: Vec<BuildFile>,
    subdirs: Vec<String>,
}

impl FatImageBuilder {
    pub fn fat12() -> Self {
        Self {
            fat_type: FatType::Fat12,
            files: Vec::new(),
            subdirs: Vec::new(),
        }
    }

    pub fn fat16() -> Self {
        Self {
            fat_type: FatType::Fat16,
            files: Vec::new(),
            subdirs: Vec::new(),
        }
    }

    pub fn fat32() -> Self {
        Self {
            fat_type: FatType::Fat32,
            files: Vec::new(),
            subdirs: Vec::new(),
        }
    }

    pub fn add_subdir(&mut self, dir: &str, name: &str) -> &mut Self {
        assert_eq!(dir, "/", "M1a 构建器仅支持一级子目录");
        self.subdirs.push(name.to_string());
        self
    }

    pub fn add_file(&mut self, dir: &str, name: &str, data: &[u8]) -> &mut Self {
        assert!(
            dir == "/" || self.subdirs.iter().any(|d| format!("/{d}") == dir),
            "add_file: 目录 {dir} 不存在（先 add_subdir）"
        );
        self.files.push(BuildFile {
            dir: dir.to_string(),
            name: encode_sfn(name),
            data: data.to_vec(),
            deleted_at: None,
        });
        self
    }

    pub fn delete(&mut self, dir: &str, name: &str) -> &mut Self {
        let target = encode_sfn(name);
        let now = self.files.len();
        let f = self
            .files
            .iter_mut()
            .find(|f| f.deleted_at.is_none() && f.dir == dir && f.name == target)
            .expect("delete: file not found");
        f.deleted_at = Some(now);
        self
    }

    pub fn build(&self) -> Vec<u8> {
        // 布局（扇区）：reserved / FAT#1 / [固定根目录区] / 数据区（簇 N → 扇区
        // data_start + (N-2)）；FAT32 无固定根目录区，根目录占簇 2。
        let (root_entries, fat_size, reserved, root_cluster, total_sectors) =
            self.fat_type.layout();
        let root_sectors = ((root_entries as u32) * 32).div_ceil(BPS);
        let fat_start = reserved;
        let root_start = fat_start + fat_size;
        let data_start = root_start + if root_entries > 0 { root_sectors } else { 0 };
        let max_cluster = 2 + (total_sectors - data_start);
        let mut image = vec![0u8; (total_sectors * BPS) as usize];

        // FAT[0]=媒体描述符、FAT[1]=EOC（三种类型各自的表项宽度）
        match self.fat_type {
            FatType::Fat12 => {
                set_fat12(&mut image, fat_start, 0, 0xFF8);
                set_fat12(&mut image, fat_start, 1, 0xFFF);
            }
            FatType::Fat16 => {
                let o = (fat_start * BPS) as usize;
                image[o..o + 2].copy_from_slice(&0xFFF8u16.to_le_bytes());
                image[o + 2..o + 4].copy_from_slice(&0xFFFFu16.to_le_bytes());
            }
            FatType::Fat32 => {
                let o = (fat_start * BPS) as usize;
                image[o..o + 4].copy_from_slice(&0x0FFF_FFF8u32.to_le_bytes());
                image[o + 4..o + 8].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
            }
        }

        // 空闲簇池（最低优先）。FAT32 的根目录占簇 2，先从池中划走。
        let mut next_free: Vec<u32> = (2..max_cluster).collect();
        let root_cluster_actual = if root_cluster >= 2 {
            next_free.remove(0)
        } else {
            0
        };
        // FAT32 根目录簇自身的表项 = EOC（规范要求）
        if root_cluster_actual >= 2 {
            let o = (fat_start * BPS) as usize + root_cluster_actual as usize * 4;
            image[o..o + 4].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
        }
        // 子目录各占一簇
        let mut dir_clusters: Vec<(String, u32)> = Vec::new();
        for d in &self.subdirs {
            dir_clusters.push((d.clone(), next_free.remove(0)));
        }
        // 目录簇写 EOC（单簇目录；与真实 FAT 一致）
        for (_, c) in &dir_clusters {
            match self.fat_type {
                FatType::Fat12 => set_fat12(&mut image, fat_start, *c, 0xFFF),
                FatType::Fat16 => {
                    let o = (fat_start * BPS + c * 2) as usize;
                    image[o..o + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
                }
                FatType::Fat32 => {
                    let o = (fat_start * BPS + c * 4) as usize;
                    image[o..o + 4].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
                }
            }
        }

        // ---- 分配簇（最低空闲优先；删除按真实时序释放：在分配文件 i 前，
        // 先释放所有 deleted_at == Some(i) 的文件簇 → 只有删除之后的文件才会复用）----
        let mut placed: Vec<(Vec<u32>, Vec<u8>, usize)> = Vec::new(); // (clusters, data, file_idx)
        for (i, f) in self.files.iter().enumerate() {
            for (clusters, _data, fi) in placed.iter() {
                if self.files[*fi].deleted_at == Some(i) {
                    for &c in clusters {
                        next_free.push(c);
                    }
                }
            }
            next_free.sort_unstable();
            let count = (f.data.len() as u32).div_ceil(BPS).max(1);
            assert!(
                next_free.len() >= count as usize,
                "簇池不足：需要 {count} 簇，仅剩 {}（夹具卷太小）",
                next_free.len()
            );
            let take: Vec<u32> = next_free.drain(..count as usize).collect();
            if f.deleted_at.is_none() {
                // 存活文件写 FAT 链（末簇 EOC）；删除文件不写链（已释放）
                for (j, &c) in take.iter().enumerate() {
                    if self.fat_type == FatType::Fat12 {
                        let value: u32 = if j + 1 == take.len() {
                            0xFFF
                        } else {
                            take[j + 1]
                        };
                        set_fat12(&mut image, fat_start, c, value);
                    } else {
                        let width = if self.fat_type == FatType::Fat32 {
                            4
                        } else {
                            2
                        };
                        let off = (fat_start * BPS + c * width) as usize;
                        let value: u32 = if j + 1 == take.len() {
                            if width == 4 { 0x0FFF_FFFF } else { 0xFFFF }
                        } else {
                            take[j + 1]
                        };
                        if width == 4 {
                            image[off..off + 4].copy_from_slice(&value.to_le_bytes());
                        } else {
                            image[off..off + 2].copy_from_slice(&(value as u16).to_le_bytes());
                        }
                    }
                }
            }
            placed.push((take, f.data.clone(), i));
        }

        // ---- 写数据（按 FAT 链逐簇落盘：释放簇复用可能碎裂，从首簇线性写会
        // 溢出到相邻文件的数据簇，导致内容与链不一致）----
        for (clusters, data, _) in &placed {
            for (j, &c) in clusters.iter().enumerate() {
                let off = j * BPS as usize;
                if off >= data.len() {
                    break; // 空文件；末簇不足 512B 时下面按实际长度截断
                }
                let end = (off + BPS as usize).min(data.len());
                let start = (data_start * BPS + (c - 2) * BPS) as usize;
                image[start..start + (end - off)].copy_from_slice(&data[off..end]);
            }
        }

        // ---- 根目录槽（FAT32 时 root_start 字节地址恰好落在簇 2 = 根目录簇）----
        // 槽位容量 fail-fast（静默溢出会覆盖活文件数据簇）
        let root_file_count = placed
            .iter()
            .filter(|(_, _, fi)| self.files[*fi].dir == "/")
            .count();
        if root_entries == 0 {
            assert!(
                dir_clusters.len() + root_file_count <= BPS as usize / 32,
                "FAT32 根目录单簇容量不足（最多 {} 项）",
                BPS as usize / 32
            );
        } else {
            assert!(
                dir_clusters.len() + root_file_count <= root_entries as usize,
                "根目录项超出 root_entries={root_entries}"
            );
        }
        let mut slot = (root_start * BPS) as usize;
        for (dir, cluster) in &dir_clusters {
            let mut e = [0u8; 32];
            e[..11].copy_from_slice(&encode_sfn(dir));
            e[11] = 0x10; // ATTR_DIRECTORY
            e[26..28].copy_from_slice(&(*cluster as u16).to_le_bytes());
            image[slot..slot + 32].copy_from_slice(&e);
            slot += 32;
        }
        for (clusters, data, idx) in &placed {
            if self.files[*idx].dir != "/" {
                continue;
            }
            let mut e = [0u8; 32];
            e[..11].copy_from_slice(&self.files[*idx].name);
            if self.files[*idx].deleted_at.is_some() {
                e[0] = 0xE5;
            }
            e[11] = 0x20; // ATTR_ARCHIVE
            e[26..28].copy_from_slice(&(clusters[0] as u16).to_le_bytes());
            e[28..32].copy_from_slice(&(data.len() as u32).to_le_bytes());
            image[slot..slot + 32].copy_from_slice(&e);
            slot += 32;
        }

        // ---- 子目录内容区（每个一簇）："." ".." + 成员项 ----
        for (dir, cluster) in &dir_clusters {
            let members = placed
                .iter()
                .filter(|(_, _, fi)| self.files[*fi].dir == format!("/{dir}"))
                .count();
            assert!(
                members + 2 <= BPS as usize / 32,
                "子目录 {dir} 成员超出单簇容量（最多 {} + 2）",
                BPS as usize / 32 - 2
            );
            let base = (data_start * BPS + (cluster - 2) * BPS) as usize;
            let parent_cluster = root_cluster_actual; // FAT12/16 为 0
            let mut e = [0u8; 32];
            e[..11].copy_from_slice(b".          ");
            e[11] = 0x10;
            e[26..28].copy_from_slice(&(*cluster as u16).to_le_bytes());
            image[base..base + 32].copy_from_slice(&e);
            let mut e = [0u8; 32];
            e[..11].copy_from_slice(b"..         ");
            e[11] = 0x10;
            e[26..28].copy_from_slice(&(parent_cluster as u16).to_le_bytes());
            image[base + 32..base + 64].copy_from_slice(&e);
            let mut off = base + 64;
            for (clusters, data, fi) in &placed {
                if self.files[*fi].dir != format!("/{dir}") {
                    continue;
                }
                let mut e = [0u8; 32];
                e[..11].copy_from_slice(&self.files[*fi].name);
                if self.files[*fi].deleted_at.is_some() {
                    e[0] = 0xE5;
                }
                e[11] = 0x20;
                e[26..28].copy_from_slice(&(clusters[0] as u16).to_le_bytes());
                e[28..32].copy_from_slice(&(data.len() as u32).to_le_bytes());
                image[off..off + 32].copy_from_slice(&e);
                off += 32;
            }
        }

        // ---- 引导扇区（按类型）----
        let mut bs = [0u8; 512];
        bs[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
        bs[3..11].copy_from_slice(b"MSDOS5.0");
        bs[11..13].copy_from_slice(&(BPS as u16).to_le_bytes());
        bs[13] = 1; // sectors per cluster
        bs[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
        bs[16] = 1; // num fats
        bs[17..19].copy_from_slice(&root_entries.to_le_bytes());
        bs[19..21].copy_from_slice(&0u16.to_le_bytes()); // total16 = 0（用 total32）
        bs[21] = 0xF8; // media descriptor
        bs[22..24]
            .copy_from_slice(&(if root_entries > 0 { fat_size as u16 } else { 0 }).to_le_bytes());
        bs[24..26].copy_from_slice(&63u16.to_le_bytes()); // sectors per track（惯例值）
        bs[26..28].copy_from_slice(&255u16.to_le_bytes()); // heads
        bs[28..32].copy_from_slice(&data_start.to_le_bytes()); // hidden sectors（夹具惯例值）
        bs[32..36].copy_from_slice(&total_sectors.to_le_bytes());
        bs[36] = 0x80;
        if root_cluster_actual >= 2 {
            // FAT32 扩展 BPB（字段映射与 FAT12/16 不同，独立填写避免残留）
            bs[36..40].copy_from_slice(&fat_size.to_le_bytes());
            bs[44..48].copy_from_slice(&root_cluster_actual.to_le_bytes());
            bs[48..50].copy_from_slice(&1u16.to_le_bytes()); // fsinfo sector（声明，内容简化）
            bs[50..52].copy_from_slice(&6u16.to_le_bytes()); // 惯例备份引导扇区
            bs[64] = 0x80;
            bs[66] = 0x29;
            bs[67..71].copy_from_slice(&0x1234_5678u32.to_le_bytes());
            bs[71..82].copy_from_slice(b"XIAODUN    ");
            bs[82..90].copy_from_slice(b"FAT32   ");
        } else {
            bs[38] = 0x29;
            bs[39..43].copy_from_slice(&0x1234_5678u32.to_le_bytes());
            bs[43..54].copy_from_slice(b"XIAODUN    ");
            bs[54..62].copy_from_slice(b"FAT16   ");
        }
        bs[510] = 0x55;
        bs[511] = 0xAA;
        image[..512].copy_from_slice(&bs);

        image
    }
}

/// 写 FAT12 表项（12 位半字节打包：entry N 位于字节 N + N/2）。
fn set_fat12(image: &mut [u8], fat_start: u32, cluster: u32, value: u32) {
    let off = (fat_start * BPS + cluster + cluster / 2) as usize;
    let v = (value & 0x0FFF) as u16;
    if cluster.is_multiple_of(2) {
        image[off] = (v & 0xFF) as u8;
        image[off + 1] = (image[off + 1] & 0xF0) | ((v >> 8) as u8 & 0x0F);
    } else {
        image[off] = (image[off] & 0x0F) | (((v << 4) & 0xF0) as u8);
        image[off + 1] = ((v >> 4) & 0xFF) as u8;
    }
}

/// "HELLO.TXT" → b"HELLO   TXT"（大写、空格填充、无扩展名时全空格）
pub fn encode_sfn(name: &str) -> [u8; 11] {
    debug_assert!(name.is_ascii(), "encode_sfn 仅支持 ASCII 8.3 名: {name}");
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

    #[test]
    fn empty_file_builds_without_panic() {
        let image = FatImageBuilder::fat16()
            .add_file("/", "EMPTY.TXT", b"")
            .build();
        let de = image
            .windows(32)
            .position(|w| w[0] == b'E' && &w[8..11] == b"TXT")
            .unwrap();
        assert_eq!(
            u32::from_le_bytes([
                image[de + 28],
                image[de + 29],
                image[de + 30],
                image[de + 31]
            ]),
            0
        );
        assert_eq!(u16::from_le_bytes([image[de + 26], image[de + 27]]), 2); // 仍占 1 簇
    }

    #[test]
    fn deletion_timing_respected() {
        let img = FatImageBuilder::fat16()
            .add_file("/", "IMG.JPG", &[7u8; 500]) // 簇 2
            .add_file("/", "READ.TXT", b"keep me") // 簇 3
            .delete("/", "IMG.JPG") // 删除发生在两个 add 之后
            .build();
        // 照片字节仍在簇 2（未被后续文件覆盖）；数据区首字节 = 50*512
        let data_start = 50usize * 512;
        assert_eq!(&img[data_start..data_start + 4], &[7u8; 4]);
        // READ.TXT 在簇 3
        let rd = img
            .windows(32)
            .position(|w| &w[..8] == b"READ    ")
            .unwrap();
        assert_eq!(u16::from_le_bytes([img[rd + 26], img[rd + 27]]), 3);
        // IMG 的 FAT 链未写（已释放）
        assert_eq!(u16::from_le_bytes([img[512 + 4], img[512 + 5]]), 0);
    }

    #[test]
    fn fragmented_reuse_writes_data_per_cluster() {
        // 删 A 后建 3 簇的 C：C 复用 [2,3] + 新簇 6（碎裂），数据必须按链逐簇落盘
        let img = FatImageBuilder::fat16()
            .add_file("/", "A.BIN", &[0xA1; 1024]) // 簇 2,3
            .add_file("/", "B.BIN", &[0xB2; 1024]) // 簇 4,5（存活，不得被写穿）
            .delete("/", "A.BIN")
            .add_file("/", "C.BIN", &[0xC3; 1536])
            .build();
        let cl = |n: usize| (50 + (n - 2)) * 512;
        assert_eq!(u16::from_le_bytes([img[512 + 6], img[512 + 7]]), 6); // FAT[3]=6（跳过洞）
        assert_eq!(u16::from_le_bytes([img[512 + 12], img[512 + 13]]), 0xFFFF); // FAT[6]=EOC
        assert_eq!([img[cl(2)], img[cl(3)], img[cl(6)]], [0xC3; 3]); // C 三分片各就其簇
        assert_eq!([img[cl(4)], img[cl(5)]], [0xB2; 2]); // B 完好
    }

    #[test]
    fn fat32_structural_fields() {
        let image = FatImageBuilder::fat32()
            .add_file("/", "A.TXT", b"abc")
            .build();
        assert_eq!(u16::from_le_bytes([image[17], image[18]]), 0); // root_entries = 0
        assert_eq!(u16::from_le_bytes([image[22], image[23]]), 0); // fat16_size = 0
        assert_eq!(
            u32::from_le_bytes([image[36], image[37], image[38], image[39]]),
            64
        ); // fat32_size
        assert_eq!(
            u32::from_le_bytes([image[44], image[45], image[46], image[47]]),
            2
        ); // root_cluster
        // 根簇 EOC 与根目录内容确实在簇 2（reserved=32 → FAT[2] 在 32*512+8）
        let fat = 32usize * 512;
        assert_eq!(
            u32::from_le_bytes([
                image[fat + 8],
                image[fat + 9],
                image[fat + 10],
                image[fat + 11]
            ]) & 0x0FFF_FFFF,
            0x0FFF_FFFF
        );
        let de = 96 * 512; // root_start(=data_start)=96
        assert_eq!(&image[de..de + 11], b"A       TXT");
        assert_eq!(u16::from_le_bytes([image[de + 26], image[de + 27]]), 3);
        assert_eq!(&image[97 * 512..97 * 512 + 3], b"abc");
    }

    #[test]
    fn fat12_boot_sector_and_packing() {
        let image = FatImageBuilder::fat12()
            .add_file("/", "A.BIN", &[5u8; 600])
            .build();
        assert_eq!(u16::from_le_bytes([image[11], image[12]]), 512);
        assert_eq!(u16::from_le_bytes([image[17], image[18]]), 224); // root entries
        // FAT12 链：簇 2→3，entry(2)=3 与 entry(3)=EOC 的半字节打包
        let fat = 512usize; // reserved=1
        let e2 = ((image[fat + 3] as u32) & 0xFF) | (((image[fat + 4] as u32) & 0x0F) << 8);
        assert_eq!(e2, 3);
        let e3 = ((image[fat + 4] as u32) >> 4) | ((image[fat + 5] as u32) << 4);
        assert_eq!(e3, 0xFFF);
    }

    #[test]
    fn subdir_has_dot_entries_and_files() {
        let image = FatImageBuilder::fat16()
            .add_subdir("/", "DIR")
            .add_file("/DIR", "IN.TXT", b"inner")
            .build();
        // 根下有 DIR 目录项（ATTR_DIRECTORY=0x10，first_cluster ≥ 2）
        let de = image.windows(32).position(|w| &w[..3] == b"DIR").unwrap();
        assert_eq!(image[de + 11] & 0x10, 0x10);
        // 子目录内容区含 "." 与 ".."，以及 IN.TXT；内容可定位
        let dir_cluster = u16::from_le_bytes([image[de + 26], image[de + 27]]) as usize;
        let base = (50 + (dir_cluster - 2)) * 512;
        assert_eq!(&image[base..base + 11], b".          ");
        assert_eq!(
            u16::from_le_bytes([image[base + 26], image[base + 27]]) as usize,
            dir_cluster
        );
        assert_eq!(&image[base + 32..base + 43], b"..         ");
        assert_eq!(u16::from_le_bytes([image[base + 58], image[base + 59]]), 0);
        assert_eq!(&image[base + 64..base + 75], b"IN      TXT");
        assert_eq!(u16::from_le_bytes([image[base + 90], image[base + 91]]), 3);
        assert_eq!(&image[(50 + (3 - 2)) * 512..][..5], b"inner");
    }
}
