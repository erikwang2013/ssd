// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! exFAT 合成镜像 builder（确定性、fail-fast）。
//!
//! 几何：1 MiB 卷（规范最小值）/ 512B 扇区 / **8 扇区 = 4KB 簇** / 252 簇；
//! 簇 2=分配位图、簇 3..4=Up-case 表（规范推荐表，FAT 链 3→4）、簇 5=根目录，文件簇从 6 起。
//!
//! 实证锚点（2026-10-02 用 mkfs.exfat/exfatprogs 1.2.8 真实格式化产物双向核对）：
//! - BootChecksum 跳过区域内绝对字节 106/107/112：真实镜像 stored==calc（912DFBC6）验证通过；
//! - Up-case 表 = 规范推荐表（5836B，TableChecksum=0xE619D30D，sha256 8344f27a…）；
//! - 目录项字段偏移：0x81 与 0x82 均为 FirstCluster@20、DataLength@24（调研简报的 "28/32"
//!   与其 32 字节槽宽矛盾，已由真实产物裁决为 20/24）。

use std::collections::HashMap;

pub const BPS_SHIFT: u8 = 9;
pub const SPC_SHIFT: u8 = 3; // 8 扇区 = 4KB 簇
pub const VOLUME_LENGTH: u64 = 2048; // 扇区（1 MiB，规范最小值）
pub const FAT_OFFSET: u32 = 24;
pub const FAT_LENGTH: u32 = 2;
pub const HEAP_OFFSET: u32 = 32;
pub const CLUSTER_COUNT: u32 = 252;
pub const BITMAP_CLUSTER: u32 = 2;
pub const UPCASE_CLUSTERS: [u32; 2] = [3, 4];
pub const ROOT_CLUSTER: u32 = 5;
pub const FIRST_FILE_CLUSTER: u32 = 6;
pub const VOLUME_SERIAL: u32 = 0x1234_5678;

/// 规范推荐 Up-case 表（5836 字节；资产与校验见模块头）。
pub const UPCASE_TABLE: &[u8] = include_bytes!("exfat_upcase.bin");
pub const UPCASE_TABLE_CHECKSUM: u32 = 0xE619_D30D;

const BPS: usize = 1 << BPS_SHIFT;
const CBS: usize = BPS << SPC_SHIFT; // 4096
const EOC: u32 = 0xFFFF_FFFF;

/// 32 位"循环右移 1 位累加"折叠（exFAT §3.4 / §7.2.4）。`skip` 为区域内绝对字节下标。
pub fn fold32(bytes: &[u8], skip: &[usize]) -> u32 {
    let mut sum: u32 = 0;
    for (i, b) in bytes.iter().enumerate() {
        if skip.contains(&i) {
            continue;
        }
        sum = (if sum & 1 != 0 { 0x8000_0000u32 } else { 0u32 })
            .wrapping_add(sum >> 1)
            .wrapping_add(*b as u32);
    }
    sum
}

/// 16 位变体（EntrySet/NameHash 用）。
pub fn fold16(bytes: &[u8], skip: &[usize]) -> u16 {
    let mut sum: u16 = 0;
    for (i, b) in bytes.iter().enumerate() {
        if skip.contains(&i) {
            continue;
        }
        sum = (if sum & 1 != 0 { 0x8000u16 } else { 0u16 })
            .wrapping_add(sum >> 1)
            .wrapping_add(*b as u16);
    }
    sum
}

pub fn boot_checksum(region11: &[u8]) -> u32 {
    fold32(region11, &[106, 107, 112])
}
pub fn entry_set_checksum(set: &[u8]) -> u16 {
    fold16(set, &[2, 3])
}
pub fn table_checksum(table: &[u8]) -> u32 {
    fold32(table, &[])
}
pub fn name_hash(upcased_utf16le: &[u8]) -> u16 {
    fold16(upcased_utf16le, &[])
}

/// 前 128 码元强制映射（§7.2.5 Table 24）：a-z → A-Z，其余恒等。
pub fn upcase_ascii(c: u16) -> u16 {
    if (0x61..=0x7A).contains(&c) {
        c - 0x20
    } else {
        c
    }
}

/// 解码压缩 up-case 表（§7.2.5：uni==index 恒等；0xFFFF → 下一 u16 为恒等个数）。
/// 仅测试用于校验资产自洽性（引擎不做全表解码，非 ASCII 名按 §7.2.5 Table 24 恒等处理）。
#[cfg(test)]
pub fn decode_upcase(compressed: &[u8]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    let mut skip = false;
    let mut i = 0usize;
    while i + 1 < compressed.len() {
        let uni = u16::from_le_bytes([compressed[i], compressed[i + 1]]);
        i += 2;
        if skip {
            for _ in 0..uni {
                out.push(out.len() as u16);
            }
            skip = false;
        } else if uni as usize == out.len() {
            out.push(uni);
        } else if uni == 0xFFFF {
            skip = true;
        } else {
            out.push(uni);
        }
    }
    out
}

struct FileRec {
    parent: String,
    name: String,
    data: Vec<u8>,
    clusters: Vec<u32>, // 物理簇序 == 数据顺序（碎片化用例靠此）
    contiguous: bool,   // NoFatChain
    vdl: u64,
    deleted: bool,
}

pub struct ExfatImageBuilder {
    cursor: u32,
    allocated: Vec<u32>,
    files: Vec<FileRec>,
    dirs: HashMap<String, Vec<u32>>, // 路径 → 簇序列；根 "/" = [5..)
    label: String,
}

impl ExfatImageBuilder {
    pub fn new() -> Self {
        let mut dirs = HashMap::new();
        dirs.insert("/".to_string(), vec![ROOT_CLUSTER]);
        Self {
            cursor: FIRST_FILE_CLUSTER,
            allocated: vec![BITMAP_CLUSTER, ROOT_CLUSTER]
                .into_iter()
                .chain(UPCASE_CLUSTERS)
                .collect(),
            files: Vec::new(),
            dirs,
            label: "XIAODUN".to_string(),
        }
    }

    fn take_cluster(&mut self) -> u32 {
        while self.allocated.contains(&self.cursor) {
            self.cursor += 1;
        }
        assert!(
            self.cursor <= CLUSTER_COUNT + 1,
            "cluster 耗尽（需要更多簇）"
        );
        let c = self.cursor;
        self.cursor += 1;
        self.allocated.push(c);
        c
    }

    fn take_clusters(&mut self, n: usize) -> Vec<u32> {
        (0..n).map(|_| self.take_cluster()).collect()
    }

    fn check_name(&self, name: &str) {
        let units = name.encode_utf16().count();
        assert!(
            (1..=255).contains(&units),
            "name 长度非法（{units} 个 UTF-16 码元）：{name}"
        );
        assert!(!name.contains('/'), "name 不得含 /：{name}");
    }

    fn check_dir(&self, dir: &str) {
        assert!(self.dirs.contains_key(dir), "dir 不存在：{dir}");
    }

    /// 追加文件（NoFatChain=1：连续分配，不写 FAT——exFAT 最常见形态）。
    pub fn add_file(&mut self, dir: &str, name: &str, data: &[u8]) -> &mut Self {
        self.push_file(dir, name, data, data.len() as u64, true, None)
    }

    /// 追加文件（NoFatChain=0：FAT 链描述分配）。
    pub fn add_file_chained(&mut self, dir: &str, name: &str, data: &[u8]) -> &mut Self {
        self.push_file(dir, name, data, data.len() as u64, false, None)
    }

    /// 追加文件到显式簇序列（可乱序 → 碎片化；clusters[i] 存放数据第 i 段）。
    pub fn add_file_in_clusters(
        &mut self,
        dir: &str,
        name: &str,
        data: &[u8],
        clusters: &[u32],
        contiguous: bool,
    ) -> &mut Self {
        self.check_name(name);
        self.check_dir(dir);
        assert!(!clusters.is_empty(), "clusters 不得为空");
        for c in clusters {
            assert!((2..=CLUSTER_COUNT + 1).contains(c), "cluster 越界：{c}");
            assert!(!self.allocated.contains(c), "cluster 已占用：{c}");
        }
        self.allocated.extend(clusters);
        self.cursor = self.cursor.max(clusters.iter().max().unwrap() + 1);
        self.files.push(FileRec {
            parent: dir.to_string(),
            name: name.to_string(),
            data: data.to_vec(),
            clusters: clusters.to_vec(),
            contiguous,
            vdl: data.len() as u64,
            deleted: false,
        });
        self
    }

    /// 追加文件并指定 ValidDataLength（VDL ≤ len；[VDL,DL) 磁盘内容"未定义"——夹具写真实数据）。
    pub fn add_file_with_vdl(&mut self, dir: &str, name: &str, data: &[u8], vdl: u64) -> &mut Self {
        assert!(vdl <= data.len() as u64, "vdl 不得大于数据长度");
        self.push_file(dir, name, data, vdl, true, None)
    }

    fn push_file(
        &mut self,
        dir: &str,
        name: &str,
        data: &[u8],
        vdl: u64,
        contiguous: bool,
        _pad: Option<()>,
    ) -> &mut Self {
        self.check_name(name);
        self.check_dir(dir);
        let clusters = if data.is_empty() {
            vec![]
        } else {
            let n = data.len().div_ceil(CBS);
            self.take_clusters(n)
        };
        self.files.push(FileRec {
            parent: dir.to_string(),
            name: name.to_string(),
            data: data.to_vec(),
            clusters,
            contiguous,
            vdl,
            deleted: false,
        });
        self
    }

    /// 建子目录（连续 1 簇；内容超 1 簇时 build 内 fail-fast）。
    pub fn add_subdir(&mut self, parent: &str, name: &str) -> &mut Self {
        self.check_name(name);
        self.check_dir(parent);
        let path = if parent == "/" {
            format!("/{name}")
        } else {
            format!("{parent}/{name}")
        };
        assert!(!self.dirs.contains_key(&path), "dir 已存在：{path}");
        let c = self.take_cluster();
        self.dirs.insert(path, vec![c]);
        self
    }

    /// 删除：位图清位、类型清 bit7（FAT **不动**——exFAT 语义）；簇释放可被显式 API 复用。
    pub fn delete(&mut self, dir: &str, name: &str) -> &mut Self {
        let f = self
            .files
            .iter_mut()
            .find(|f| f.parent == dir && f.name == name && !f.deleted)
            .unwrap_or_else(|| panic!("delete: 找不到 {dir}/{name}"));
        f.deleted = true;
        let freed = f.clusters.clone();
        self.allocated.retain(|c| !freed.contains(c));
        self
    }

    fn cluster_byte(c: u32) -> usize {
        HEAP_OFFSET as usize * BPS + (c as usize - 2) * CBS
    }

    pub fn build(&mut self) -> Vec<u8> {
        let mut img = vec![0u8; VOLUME_LENGTH as usize * BPS];

        // ---- 引导区（主 0..11 / 备 12..23，几何相同）----
        for region in [0usize, 12] {
            let b = region * BPS;
            img[b] = 0xEB;
            img[b + 1] = 0x76;
            img[b + 2] = 0x90;
            img[b + 3..b + 11].copy_from_slice(b"EXFAT   ");
            img[b + 72..b + 80].copy_from_slice(&VOLUME_LENGTH.to_le_bytes());
            img[b + 80..b + 84].copy_from_slice(&FAT_OFFSET.to_le_bytes());
            img[b + 84..b + 88].copy_from_slice(&FAT_LENGTH.to_le_bytes());
            img[b + 88..b + 92].copy_from_slice(&HEAP_OFFSET.to_le_bytes());
            img[b + 92..b + 96].copy_from_slice(&CLUSTER_COUNT.to_le_bytes());
            img[b + 96..b + 100].copy_from_slice(&ROOT_CLUSTER.to_le_bytes());
            img[b + 100..b + 104].copy_from_slice(&VOLUME_SERIAL.to_le_bytes());
            img[b + 104..b + 106].copy_from_slice(&0x0100u16.to_le_bytes());
            img[b + 106] = 0x00; // VolumeFlags（ActiveFat=0）
            img[b + 108] = BPS_SHIFT;
            img[b + 109] = SPC_SHIFT;
            img[b + 110] = 1; // NumberOfFats
            img[b + 111] = 0x80; // DriveSelect
            img[b + 112] = 0xFF; // PercentInUse = 未知
            img[b + 510] = 0x55;
            img[b + 511] = 0xAA;
            for s in 1..=8usize {
                let end = (region + s + 1) * BPS - 4;
                img[end..end + 4].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
            }
            let sum = boot_checksum(&img[region * BPS..region * BPS + BPS * 11]);
            let cb = (region + 11) * BPS;
            for k in 0..(BPS / 4) {
                img[cb + k * 4..cb + k * 4 + 4].copy_from_slice(&sum.to_le_bytes());
            }
        }

        // ---- 目录槽缓冲（先组装；根目录按需跨簇）----
        let mut dir_bufs: HashMap<String, Vec<u8>> = HashMap::new();
        let mut paths: Vec<String> = self.dirs.keys().cloned().collect();
        paths.sort(); // 确定性（HashMap 迭代序不定）
        for p in &paths {
            dir_bufs.insert((*p).clone(), Vec::new());
        }

        {
            let r = dir_bufs.get_mut("/").unwrap();
            // 槽 0：0x83 卷标
            let mut lb = [0u8; 32];
            lb[0] = 0x83;
            let units: Vec<u16> = self.label.encode_utf16().collect();
            lb[1] = units.len() as u8;
            for (i, u) in units.iter().enumerate() {
                lb[2 + i * 2..2 + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
            }
            r.extend_from_slice(&lb);
            // 槽 1：0x81 位图（FirstCluster@20 / DataLength@24）
            let mut bm = [0u8; 32];
            bm[0] = 0x81;
            bm[20..24].copy_from_slice(&BITMAP_CLUSTER.to_le_bytes());
            bm[24..32]
                .copy_from_slice(&((CLUSTER_COUNT as usize).div_ceil(8) as u64).to_le_bytes());
            r.extend_from_slice(&bm);
            // 槽 2：0x82 Up-case（同偏移规则；TableChecksum@4）
            debug_assert_eq!(
                table_checksum(UPCASE_TABLE),
                UPCASE_TABLE_CHECKSUM,
                "资产与 KAT 常量不符"
            );
            let mut uc = [0u8; 32];
            uc[0] = 0x82;
            uc[4..8].copy_from_slice(&UPCASE_TABLE_CHECKSUM.to_le_bytes());
            uc[20..24].copy_from_slice(&UPCASE_CLUSTERS[0].to_le_bytes());
            uc[24..32].copy_from_slice(&(UPCASE_TABLE.len() as u64).to_le_bytes());
            r.extend_from_slice(&uc);
        }

        // 子目录项集（写入父目录；确定性顺序）
        for p in &paths {
            if *p == "/" {
                continue;
            }
            let name = p.rsplit('/').next().unwrap();
            let clusters = &self.dirs[p];
            let parent = match p.rfind('/') {
                Some(0) => "/".to_string(),
                Some(i) => p[..i].to_string(),
                None => unreachable!(),
            };
            let bytes = build_entry_set(
                name,
                0x10,
                clusters[0],
                (clusters.len() * CBS) as u64,
                true,
                (clusters.len() * CBS) as u64,
            );
            push_dir_unit(dir_bufs.get_mut(&parent).unwrap(), &bytes);
        }
        // 文件项集（插入序）
        for f in &self.files {
            let first = f.clusters.first().copied().unwrap_or(0);
            let bytes = build_entry_set(
                &f.name,
                0x20,
                first,
                f.data.len() as u64,
                f.contiguous && !f.clusters.is_empty(),
                f.vdl,
            );
            let mut bytes = bytes;
            if f.deleted {
                for i in (0..bytes.len()).step_by(32) {
                    bytes[i] &= 0x7F;
                }
            }
            push_dir_unit(dir_bufs.get_mut(&f.parent).unwrap(), &bytes);
        }
        // 子目录容量（1 簇）fail-fast
        for p in &paths {
            if *p != "/" {
                assert!(
                    dir_bufs[p].len() <= CBS,
                    "子目录 {p} 槽位超出 1 簇（{}B）——夹具不支持子目录扩容",
                    dir_bufs[p].len()
                );
            }
        }
        // 根目录按需扩容
        let root_need = dir_bufs["/"].len().div_ceil(CBS).max(1);
        while self.dirs["/"].len() < root_need {
            let c = self.take_cluster();
            self.dirs.get_mut("/").unwrap().push(c);
        }

        // ---- FAT ----
        let fat = FAT_OFFSET as usize * BPS;
        img[fat..fat + 4].copy_from_slice(&0xFFFF_FFF8u32.to_le_bytes()); // 媒体项
        img[fat + 4..fat + 8].copy_from_slice(&EOC.to_le_bytes()); // entry[1]
        let mut write_chain = |clusters: &[u32]| {
            for w in clusters.windows(2) {
                let e = fat + w[0] as usize * 4;
                img[e..e + 4].copy_from_slice(&w[1].to_le_bytes());
            }
            let e = fat + *clusters.last().unwrap() as usize * 4;
            img[e..e + 4].copy_from_slice(&EOC.to_le_bytes());
        };
        write_chain(&UPCASE_CLUSTERS); // 3→4→EOC
        write_chain(&self.dirs["/"]); // 5→…→EOC（含扩容链）
        for p in &paths {
            if *p != "/" {
                write_chain(&self.dirs[p]);
            }
        }
        for f in &self.files {
            if !f.contiguous && !f.clusters.is_empty() {
                write_chain(&f.clusters);
            }
        }

        // ---- 位图 ----
        let total_bits = (CLUSTER_COUNT as usize).div_ceil(8);
        let mut bm = vec![0u8; total_bits];
        for &c in &self.allocated {
            let idx = c - 2;
            bm[(idx / 8) as usize] |= 1 << (idx % 8);
        }
        let bm_off = Self::cluster_byte(BITMAP_CLUSTER);
        img[bm_off..bm_off + total_bits].copy_from_slice(&bm);

        // ---- up-case 表 ----
        let uc_off = Self::cluster_byte(UPCASE_CLUSTERS[0]);
        img[uc_off..uc_off + UPCASE_TABLE.len()].copy_from_slice(UPCASE_TABLE);

        // ---- 文件数据（含删除项——数据仍在盘上是恢复前提；[VDL,DL) 亦写真实数据便于测试）----
        for f in &self.files {
            for (i, c) in f.clusters.iter().enumerate() {
                let start = i * CBS;
                let end = (start + CBS).min(f.data.len());
                if start >= end {
                    break;
                }
                let off = Self::cluster_byte(*c);
                img[off..off + (end - start)].copy_from_slice(&f.data[start..end]);
            }
        }

        // ---- 目录槽落盘 ----
        for p in &paths {
            let buf = &dir_bufs[p];
            for (i, chunk) in buf.chunks(CBS).enumerate() {
                let off = Self::cluster_byte(self.dirs[p][i]);
                img[off..off + chunk.len()].copy_from_slice(chunk);
            }
        }

        img
    }
}

impl Default for ExfatImageBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// 追加一个项集到目录缓冲：项集不得跨簇边界（规范）；本簇剩余槽不足时，
/// 先用 unused 项（0x01——扫描器跳过且不终止目录）补齐，使项集从下一簇首槽开始。
fn push_dir_unit(buf: &mut Vec<u8>, unit: &[u8]) {
    let off = buf.len() % CBS;
    if off != 0 && off + unit.len() > CBS {
        for _ in 0..(CBS - off) / 32 {
            let mut slot = [0u8; 32];
            slot[0] = 0x01; // unused entry
            buf.extend_from_slice(&slot);
        }
    }
    buf.extend_from_slice(unit);
}

/// 组装一个 0x85/0xC0/0xC1×N 项集（含 SetChecksum 与 NameHash）。
fn build_entry_set(
    name: &str,
    attr: u16,
    first_cluster: u32,
    dl: u64,
    no_fat_chain: bool,
    vdl: u64,
) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let name_entries = units.len().div_ceil(15);
    let mut set: Vec<u8> = Vec::with_capacity((2 + name_entries) * 32);
    let mut file = [0u8; 32];
    file[0] = 0x85;
    file[1] = (1 + name_entries) as u8; // SecondaryCount
    file[4..6].copy_from_slice(&attr.to_le_bytes());
    let mut stream = [0u8; 32];
    stream[0] = 0xC0;
    // AllocationPossible(bit0) | NoFatChain(bit1)；首簇为 0 的零长文件 NoFatChain 必须为 0
    stream[1] = if no_fat_chain && first_cluster != 0 {
        0x03
    } else {
        0x01
    };
    stream[3] = units.len() as u8; // NameLength
    stream[8..16].copy_from_slice(&vdl.to_le_bytes());
    stream[20..24].copy_from_slice(&first_cluster.to_le_bytes());
    stream[24..32].copy_from_slice(&dl.to_le_bytes());
    set.extend_from_slice(&file);
    set.extend_from_slice(&stream);
    for chunk in units.chunks(15) {
        let mut ne = [0u8; 32];
        ne[0] = 0xC1;
        // 名字码元从偏移 2 起（偏移 1 保留）——勘误 #3，fsck 实证
        for (i, u) in chunk.iter().enumerate() {
            ne[2 + i * 2..2 + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        set.extend_from_slice(&ne);
    }
    // NameHash（上转型后 UTF-16LE 字节）
    let upcased: Vec<u8> = units
        .iter()
        .flat_map(|c| upcase_ascii(*c).to_le_bytes())
        .collect();
    set[32 + 4..32 + 6].copy_from_slice(&name_hash(&upcased).to_le_bytes());
    // SetChecksum（在"未删除"形态下计算；删除只清 bit7 不重算）
    let sum = entry_set_checksum(&set);
    set[2..4].copy_from_slice(&sum.to_le_bytes());
    set
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// 根目录字节偏移（簇 5，4KB 簇：HEAP=32 扇区×512 + (5-2)×4096）
    const ROOT_OFF: usize = 32 * 512 + 3 * 4096;
    fn root(img: &[u8]) -> &[u8] {
        &img[ROOT_OFF..ROOT_OFF + 4096]
    }
    /// 文件项集固定从根槽 3 起（槽 0=0x83、槽 1=0x81、槽 2=0x82）
    const SET_OFF: usize = 3 * 32;

    #[test]
    fn build_is_deterministic() {
        let a = ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"hello")
            .build();
        let b = ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"hello")
            .build();
        assert_eq!(a, b);
        assert_eq!(a.len(), 2048 * 512); // 1 MiB
    }

    #[test]
    fn boot_region_valid() {
        let img = ExfatImageBuilder::new().build();
        assert_eq!(&img[0..3], &[0xEB, 0x76, 0x90]);
        assert_eq!(&img[3..11], b"EXFAT   ");
        assert_eq!(u16le(&img, 510), 0xAA55);
        for s in 1..=8usize {
            // 扩展引导扇区尾部签名
            let end = (s + 1) * 512 - 4;
            assert_eq!(u32le(&img, end), 0xAA55_0000, "ext boot sector {s}");
        }
        // 主 checksum 扇区 = 11 扇区范围 BootChecksum 的小端重复
        let expect = boot_checksum(&img[..512 * 11]);
        for k in 0..128usize {
            assert_eq!(u32le(&img, 512 * 11 + k * 4), expect);
        }
        // 备份区（12..23）同构且自洽
        let backup = &img[512 * 12..512 * 24];
        assert_eq!(&backup[3..11], b"EXFAT   ");
        assert_eq!(u32le(backup, 512 * 11), boot_checksum(&backup[..512 * 11]));
    }

    #[test]
    fn boot_checksum_skips_volume_flags_and_percent_in_use() {
        let mut img = ExfatImageBuilder::new().build();
        let before = boot_checksum(&img[..512 * 11]);
        img[106] ^= 0xFF;
        img[107] ^= 0xFF;
        img[112] ^= 0xFF; // 跳过区
        assert_eq!(boot_checksum(&img[..512 * 11]), before, "跳字节后必须不变");
        img[100] ^= 0xFF; // VolumeSerial 在范围内
        assert_ne!(boot_checksum(&img[..512 * 11]), before);
    }

    #[test]
    fn checksum_fold_kat() {
        // 手算：循环右移 1 位累加
        assert_eq!(boot_checksum(&[0x01]), 1);
        assert_eq!(boot_checksum(&[0x80, 0x01]), 0x41);
        assert_eq!(boot_checksum(&[0x03, 0x03]), 0x8000_0004); // 第二字节起 sum 为奇数 → 回绕分支
        assert_eq!(entry_set_checksum(&[0x01]), 1);
        assert_eq!(entry_set_checksum(&[0x80, 0x01]), 0x41);
        assert_eq!(entry_set_checksum(&[0x03, 0x03]), 0x8004);
    }

    #[test]
    fn geometry_fields_parse_back() {
        let img = ExfatImageBuilder::new().build();
        assert_eq!(img[108], 9); // BytesPerSectorShift
        assert_eq!(img[109], 3); // SectorsPerClusterShift（8 扇区 = 4KB 簇）
        assert_eq!(img[110], 1); // NumberOfFats
        assert_eq!(u32le(&img, 80), 24); // FatOffset
        assert_eq!(u32le(&img, 84), 2); // FatLength
        assert_eq!(u32le(&img, 88), 32); // ClusterHeapOffset
        assert_eq!(u32le(&img, 92), 252); // ClusterCount
        assert_eq!(u32le(&img, 96), 5); // RootDirCluster
        assert_eq!(u64le(&img, 72), 2048); // VolumeLength
        assert_eq!(u32le(&img, 104), 0x0100);
        assert_eq!(u32le(&img, 100), 0x1234_5678); // VolumeSerial
    }

    #[test]
    fn upcase_table_asset_and_self_consistency() {
        // 资产 KAT（真实世界锚点：规范推荐表）
        assert_eq!(UPCASE_TABLE.len(), 5836);
        assert_eq!(table_checksum(UPCASE_TABLE), UPCASE_TABLE_CHECKSUM); // 0xE619D30D
        // 压缩表解码后必须覆盖全 Unicode 区间，且前 128 映射合规
        let decoded = decode_upcase(UPCASE_TABLE);
        assert_eq!(decoded.len(), 65_536);
        for c in 0x0000u16..=0x0060 {
            assert_eq!(decoded[c as usize], c);
        }
        for c in 0x61u16..=0x7A {
            assert_eq!(decoded[c as usize], c - 0x20, "a-z → A-Z");
        }
        assert_eq!(decoded[0x7B], 0x7B);
        assert_eq!(decoded[0xFFFF], 0xFFFF);
    }

    #[test]
    fn root_special_entries_present() {
        let img = ExfatImageBuilder::new().build();
        let r = root(&img);
        // 槽 0：0x83 卷标 "XIAODUN"
        assert_eq!(r[0], 0x83);
        assert_eq!(r[1], 7);
        let label: Vec<u16> = (0..7).map(|i| u16le(r, 2 + i * 2)).collect();
        assert_eq!(String::from_utf16(&label).unwrap(), "XIAODUN");
        // 槽 1：0x81 位图（FirstCluster@20 / DataLength@24）
        assert_eq!(r[32], 0x81);
        assert_eq!(u32le(r, 32 + 20), 2);
        assert_eq!(u64le(r, 32 + 24), 32); // ceil(252/8)
        // 槽 2：0x82 Up-case（**同一偏移规则**；实证勘误见模块头）
        assert_eq!(r[64], 0x82);
        assert_eq!(u32le(r, 64 + 4), UPCASE_TABLE_CHECKSUM); // TableChecksum@4
        assert_eq!(u32le(r, 64 + 20), 3); // FirstCluster@20
        assert_eq!(u64le(r, 64 + 24), 5836); // DataLength@24
        // 字节级：表内容确实在簇 3（4KB 簇，链 3→4）
        let table_off = 32 * 512 + 4096;
        assert_eq!(&img[table_off..table_off + 5836], UPCASE_TABLE);
        // FAT 链与位图：upcase 3→4→EOC，root=5→EOC；簇 2..=5 全部置位
        let fat = 24 * 512;
        assert_eq!(u32le(&img, fat), 0xFFFF_FFF8);
        assert_eq!(u32le(&img, fat + 4), 0xFFFF_FFFF);
        assert_eq!(u32le(&img, fat + 3 * 4), 4);
        assert_eq!(u32le(&img, fat + 4 * 4), 0xFFFF_FFFF);
        assert_eq!(u32le(&img, fat + 5 * 4), 0xFFFF_FFFF);
        let bm = &img[32 * 512..32 * 512 + 32];
        for c in 2..=5u32 {
            let bit = 1u8 << ((c - 2) % 8);
            assert_eq!(
                bm[((c - 2) / 8) as usize] & bit,
                bit,
                "cluster {c} 应已分配"
            );
        }
    }

    #[test]
    fn contiguous_file_keeps_fat_untouched() {
        let img = ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"hello")
            .build();
        let r = root(&img);
        let set = &r[SET_OFF..SET_OFF + 3 * 32];
        assert_eq!(set[0], 0x85);
        assert_eq!(set[1], 2); // SecondaryCount = 0xC0 + 1×0xC1
        assert_eq!(u16le(set, 4) & 0x20, 0x20); // Archive
        assert_eq!(set[32], 0xC0);
        assert_eq!(set[32 + 1], 0x03, "AllocationPossible|NoFatChain");
        assert_eq!(set[32 + 3], 5); // NameLength
        assert_eq!(u64le(set, 32 + 8), 5); // ValidDataLength
        assert_eq!(u32le(set, 32 + 20), 6); // FirstCluster（文件簇从 6 起）
        assert_eq!(u64le(set, 32 + 24), 5); // DataLength
        assert_eq!(set[64], 0xC1);
        let name: Vec<u16> = (0..5).map(|i| u16le(set, 64 + 2 + i * 2)).collect();
        assert_eq!(String::from_utf16(&name).unwrap(), "A.TXT");
        // SetChecksum 与 NameHash 自洽
        assert_eq!(entry_set_checksum(set), u16le(set, 2));
        let upcased: Vec<u8> = "A.TXT"
            .encode_utf16()
            .flat_map(|c| upcase_ascii(c).to_le_bytes())
            .collect();
        assert_eq!(name_hash(&upcased), u16le(set, 32 + 4));
        // NameHash KAT（由 fsck.exfat 实证过的参考值）
        assert_eq!(
            name_hash(
                &"TEST.TXT"
                    .encode_utf16()
                    .flat_map(|c| c.to_le_bytes())
                    .collect::<Vec<u8>>()
            ),
            0x3368
        );
        assert_eq!(
            name_hash(
                &"AB"
                    .encode_utf16()
                    .flat_map(|c| c.to_le_bytes())
                    .collect::<Vec<u8>>()
            ),
            0x2029
        );
        // 名字码元布局：偏移 1 保留、码元从偏移 2 起（勘误 #3）
        assert_eq!(set[64 + 1], 0, "偏移 1 应为保留字节");
        // NoFatChain=1 → FAT 未被写（entry(6) 仍为 0）；位图已置位
        assert_eq!(u32le(&img, 24 * 512 + 6 * 4), 0);
        let bm = &img[32 * 512..32 * 512 + 32];
        let cl = 6u32;
        let bit = 1u8 << ((cl - 2) % 8);
        assert_eq!(
            bm[((cl - 2) / 8) as usize] & bit,
            bit,
            "cluster {cl} 应已分配"
        );
        // 数据落盘
        let data_off = 32 * 512 + (6 - 2) * 4096;
        assert_eq!(&img[data_off..data_off + 5], b"hello");
    }

    #[test]
    fn chained_allocation_writes_fat() {
        let img = ExfatImageBuilder::new()
            .add_file_chained("/", "B.BIN", &[7u8; 5000])
            .build();
        let r = root(&img);
        let set = &r[SET_OFF..SET_OFF + 3 * 32];
        assert_eq!(set[32 + 1], 0x01, "add_file_chained 须 NoFatChain=0");
        assert_eq!(u32le(set, 32 + 20), 6);
        // 5000B → ceil(5000/4096)=2 簇：6→7→EOC（精确值）
        assert_eq!(u32le(&img, 24 * 512 + 6 * 4), 7);
        assert_eq!(u32le(&img, 24 * 512 + 7 * 4), 0xFFFF_FFFF);
    }

    #[test]
    fn fragmented_allocation_respects_order() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let img = ExfatImageBuilder::new()
            .add_file_in_clusters("/", "F.BIN", &data, &[7, 6, 8], false)
            .build();
        let r = root(&img);
        let set = &r[SET_OFF..SET_OFF + 3 * 32];
        assert_eq!(u32le(set, 32 + 20), 7, "首簇 = 数据首段所在簇");
        let fat = 24 * 512;
        assert_eq!(u32le(&img, fat + 7 * 4), 6);
        assert_eq!(u32le(&img, fat + 6 * 4), 8);
        assert_eq!(u32le(&img, fat + 8 * 4), 0xFFFF_FFFF);
        // 数据按簇序 7→6→8 分段落盘
        let c = |n: usize| 32 * 512 + (n - 2) * 4096;
        assert_eq!(&img[c(7)..c(7) + 4096], &data[..4096]);
        assert_eq!(&img[c(6)..c(6) + 4096], &data[4096..8192]);
        assert_eq!(&img[c(8)..c(8) + 808], &data[8192..9000]);
        // 位图三簇均置位
        let bm = &img[32 * 512..32 * 512 + 32];
        for cl in [7u32, 6, 8] {
            let bit = 1u8 << ((cl - 2) % 8);
            assert_eq!(bm[((cl - 2) / 8) as usize] & bit, bit);
        }
    }

    #[test]
    fn delete_clears_bitmap_keeps_dirset_and_stale_fat() {
        let img = ExfatImageBuilder::new()
            .add_file_chained("/", "G.BIN", &[9u8; 9000])
            .delete("/", "G.BIN")
            .build();
        let r = root(&img);
        let set = &r[SET_OFF..SET_OFF + 3 * 32];
        assert_eq!(set[0], 0x05, "0x85 → 0x05（只清 bit7）");
        assert_eq!(set[32], 0x40, "0xC0 → 0x40");
        assert_eq!(set[64], 0x41, "0xC1 → 0x41");
        assert_eq!(set[1], 2, "SecondaryCount 保留");
        assert_eq!(
            u32le(set, 32 + 20),
            6,
            "FirstCluster 保留（恢复的黄金信息）"
        );
        assert_eq!(u64le(set, 32 + 24), 9000);
        // SetChecksum 未重算：还原类型位后重算 == 存储值
        let mut restored = set.to_vec();
        restored[0] |= 0x80;
        restored[32] |= 0x80;
        restored[64] |= 0x80;
        assert_eq!(
            entry_set_checksum(&restored),
            u16le(set, 2),
            "还原校验必须通过"
        );
        // 位图已清（簇 6/7/8 空闲）
        let bm = &img[32 * 512..32 * 512 + 32];
        for cl in [6u32, 7, 8] {
            let bit = 1u8 << ((cl - 2) % 8);
            assert_eq!(bm[((cl - 2) / 8) as usize] & bit, 0, "cluster {cl} 应空闲");
        }
        // FAT stale：删除不写 FAT，链仍在
        let fat = 24 * 512;
        assert_eq!(u32le(&img, fat + 6 * 4), 7);
        assert_eq!(u32le(&img, fat + 7 * 4), 8);
        assert_eq!(u32le(&img, fat + 8 * 4), 0xFFFF_FFFF);
        // 数据仍在磁盘（恢复的物理前提）
        let c = |n: usize| 32 * 512 + (n - 2) * 4096;
        assert_eq!(&img[c(6)..c(6) + 64], &[9u8; 64]);
    }

    #[test]
    fn subdir_and_nested_file() {
        let img = ExfatImageBuilder::new()
            .add_subdir("/", "DCIM")
            .add_file("/DCIM", "IMG.JPG", &[3u8; 100])
            .build();
        let r = root(&img);
        let set = &r[SET_OFF..SET_OFF + 3 * 32];
        assert_eq!(set[0], 0x85);
        assert_eq!(u16le(set, 4) & 0x10, 0x10, "Directory 属性位");
        assert_eq!(u32le(set, 32 + 20), 6, "DCIM 占簇 6");
        assert_eq!(u64le(set, 32 + 24), 4096);
        // DCIM 簇 6 内是 IMG.JPG 的项集
        let sub = &img[32 * 512 + 4 * 4096..32 * 512 + 5 * 4096];
        assert_eq!(sub[0], 0x85);
        assert_eq!(u32le(sub, 32 + 20), 7);
    }

    #[test]
    fn root_grows_across_clusters_when_full() {
        // 根 1 簇 = 128 槽；每文件 3 槽 + 3 特项 → 45 个文件（138 槽）必然跨第二簇
        let mut b = ExfatImageBuilder::new();
        for i in 0..45u32 {
            b.add_file("/", &format!("F{i:04}.TXT"), b"x");
        }
        let img = b.build();
        let fat = 24 * 512;
        let second = u32le(&img, fat + 5 * 4);
        assert_ne!(second, 0xFFFF_FFFF, "根目录必须已跨簇（FAT 链）");
        assert!((6..=253).contains(&second), "第二根簇 {second}");
        // 文件占 6..50，生长簇应为 51
        assert_eq!(second, 51);
        // 第二根簇首槽是一个文件项集（0x85）
        let off = 32 * 512 + (second as usize - 2) * 4096;
        assert_eq!(img[off], 0x85, "第二根簇应从文件项集开始");
        // 第一根簇的 FAT 链：5→51→EOC
        assert_eq!(u32le(&img, fat + 51 * 4), 0xFFFF_FFFF);
    }

    #[test]
    #[should_panic(expected = "cluster")]
    fn panics_when_clusters_exhausted() {
        let big = vec![0u8; 300 * 4096];
        let _ = ExfatImageBuilder::new()
            .add_file("/", "BIG.BIN", &big)
            .build();
    }

    #[test]
    #[should_panic(expected = "name")]
    fn panics_on_overlong_name() {
        let name = "名".repeat(256);
        let _ = ExfatImageBuilder::new().add_file("/", &name, b"x").build();
    }

    #[test]
    #[should_panic(expected = "dir")]
    fn panics_on_missing_parent_dir() {
        let _ = ExfatImageBuilder::new()
            .add_file("/NOPE", "A.TXT", b"x")
            .build();
    }
}
