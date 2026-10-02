// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! exFAT 目录项集解析。删除识别三件套：`t & 0x7F == 0x05` + 结构自洽 + 还原校验
//! （类型位 |=0x80 后重算 SetChecksum == 磁盘值——删除只清 bit7 不重算，此校验为最强自洽判据）。
//! 0x01..0x7F 皆可为"unused 槽"——绝不允许"见 <0x80 即跳过"（会静默丢失全部删除文件）。

/// 根目录特殊项：bitmap 可多处（texFAT First/Second），upcase/卷标各至多一处。
#[derive(Debug, Default, Clone)]
pub struct Specials {
    /// (SecondBitmap?, FirstCluster, DataLength)
    pub bitmaps: Vec<(bool, u32, u64)>,
    pub upcase: Option<(u32, u64)>,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ParsedEntry {
    pub name: String,
    pub attr_dir: bool,
    pub first_cluster: u32,
    pub valid_data_length: u64,
    pub data_length: u64,
    pub contiguous: bool,
    pub deleted: bool,
    pub checksum_ok: bool,
    pub name_verified: bool,
}

#[derive(Debug, Default)]
pub struct Directory {
    pub entries: Vec<ParsedEntry>,
    pub specials: Specials,
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

/// 16 位循环右移 1 位累加，跳过集内绝对下标 2/3（SetChecksum 字段自身）。
fn entry_set_checksum16(set: &[u8]) -> u16 {
    let mut sum: u16 = 0;
    for (i, b) in set.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        sum = (if sum & 1 != 0 { 0x8000u16 } else { 0u16 })
            .wrapping_add(sum >> 1)
            .wrapping_add(*b as u16);
    }
    sum
}

/// NameHash 与项集校验和算法同族，但**不跳过任何字节**（§7.2.5；跳过 2/3 是 SetChecksum 专属）。
fn name_hash16(upcased_le: &[u8]) -> u16 {
    let mut sum: u16 = 0;
    for b in upcased_le {
        sum = (if sum & 1 != 0 { 0x8000u16 } else { 0u16 })
            .wrapping_add(sum >> 1)
            .wrapping_add(*b as u16);
    }
    sum
}

fn upcase_ascii(c: u16) -> u16 {
    if (0x61..=0x7A).contains(&c) {
        c - 0x20
    } else {
        c
    }
}

/// 解析目录字节（`cluster_bytes` 用于拒绝跨簇项集）。`data` 为该目录所有簇按链序拼接。
pub fn parse_directory_bytes(data: &[u8], cluster_bytes: usize) -> Directory {
    let mut out = Directory::default();
    let mut i = 0usize;
    while i + 32 <= data.len() {
        let t = data[i];
        if t == 0x00 {
            break;
        } // 目录结束
        if t & 0x80 == 0 {
            // 未被 in-use 位标记：仅 0x05 型（删除的 File 项）才尝试组装
            if t & 0x7F == 0x05
                && let Some((e, slots)) = try_assemble_set(data, i, cluster_bytes, true)
            {
                out.entries.push(e);
                i += slots * 32;
                continue;
            }
            i += 32;
            continue;
        }
        match t {
            0x85 => {
                if let Some((e, slots)) = try_assemble_set(data, i, cluster_bytes, false) {
                    out.entries.push(e);
                    i += slots * 32;
                    continue;
                }
                i += 32;
            }
            0x81 => {
                out.specials.bitmaps.push((
                    data[i + 1] & 0x01 != 0,
                    u32le(data, i + 20),
                    u64le(data, i + 24),
                ));
                i += 32;
            }
            0x82 => {
                out.specials.upcase = Some((u32le(data, i + 20), u64le(data, i + 24)));
                i += 32;
            }
            0x83 => {
                let n = (data[i + 1] as usize).min(11);
                let units: Vec<u16> = (0..n).map(|k| u16le(data, i + 2 + k * 2)).collect();
                out.specials.label = Some(String::from_utf16_lossy(&units));
                i += 32;
            }
            _ => i += 32, // 未知项（0x20 等 benign/其他）忽略
        }
    }
    out
}

/// 尝试组装一个项集。`deleted` 时启用还原校验门槛；返回 (条目, 槽数)。
fn try_assemble_set(
    data: &[u8],
    i: usize,
    cb: usize,
    deleted: bool,
) -> Option<(ParsedEntry, usize)> {
    let secondary = data[i + 1] as usize;
    if !(1..=18).contains(&secondary) {
        return None;
    }
    let slots = 1 + secondary;
    let end = i.checked_add(slots * 32)?;
    if end > data.len() {
        return None;
    }
    if i / cb != (end - 1) / cb {
        return None; // 项集不得跨簇边界
    }
    let raw = &data[i..end];
    // 还原类型位
    let mut restored = raw.to_vec();
    for k in 0..slots {
        restored[k * 32] |= 0x80;
    }
    if restored[0] != 0x85 || restored[32] != 0xC0 {
        return None;
    }
    // 第二槽之后：名字项 × n，随后只允许 vendor 扩展（0xE0/0xE1）
    let stream = &raw[32..64];
    let name_len = stream[3] as usize;
    if name_len == 0 {
        return None;
    }
    let name_entries = name_len.div_ceil(15);
    if slots < 2 + name_entries {
        return None;
    }
    for k in 2 + name_entries..slots {
        if !matches!(restored[k * 32], 0xE0 | 0xE1) {
            return None;
        }
    }
    let mut units: Vec<u16> = Vec::with_capacity(name_len);
    for k in 0..name_entries {
        let base = (2 + k) * 32; // 槽 0=File、槽 1=Stream，名字项自槽 2 起
        if restored[base] != 0xC1 {
            return None;
        }
        for u in 0..15 {
            if units.len() == name_len {
                break;
            }
            units.push(u16le(raw, base + 2 + u * 2)); // 勘误 #3：码元自偏移 2 起
        }
    }
    // 还原校验（删除侧硬门槛；live 记录不设门槛）
    let stored = u16le(raw, 2);
    let computed = entry_set_checksum16(&restored);
    let checksum_ok = computed == stored;
    if deleted && !checksum_ok {
        return None;
    }
    // 字段
    let attr = u16le(raw, 4);
    let attr_dir = attr & 0x10 != 0;
    let vdl = u64le(stream, 8);
    let first_cluster = u32le(stream, 20);
    let data_length = u64le(stream, 24);
    if vdl > data_length {
        return None;
    }
    if data_length == 0 && first_cluster != 0 {
        return None;
    }
    if data_length > 0 && first_cluster < 2 {
        return None;
    }
    let contiguous = stream[1] & 0x02 != 0 && first_cluster != 0;
    // NameHash：ASCII 名逐单位校验（上转型后）；非 ASCII 跳过
    let hash_field = u16le(stream, 4);
    let all_ascii = units.iter().all(|u| *u < 0x80);
    let name_verified = if all_ascii {
        let up: Vec<u8> = units
            .iter()
            .flat_map(|u| upcase_ascii(*u).to_le_bytes())
            .collect();
        name_hash16(&up) == hash_field
    } else {
        false
    };
    Some((
        ParsedEntry {
            name: String::from_utf16_lossy(&units),
            attr_dir,
            first_cluster,
            valid_data_length: vdl,
            data_length,
            contiguous,
            deleted,
            checksum_ok,
            name_verified,
        },
        slots,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xd_fixtures::ExfatImageBuilder;

    const ROOT_OFF: usize = 32 * 512 + 3 * 4096;
    const CB: usize = 4096;
    const SET_OFF: usize = 3 * 32; // 根槽 3 起

    fn root_of(img: &[u8]) -> &[u8] {
        &img[ROOT_OFF..ROOT_OFF + CB]
    }

    /// 经 builder 造一个含"活文件 + 链式删除文件"的镜像，返回根目录字节
    fn fixture_root() -> Vec<u8> {
        let img = ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"hello")
            .add_file_chained("/", "G.BIN", &[9u8; 9000])
            .delete("/", "G.BIN")
            .build();
        root_of(&img).to_vec()
    }

    #[test]
    fn parses_live_set() {
        let d = parse_directory_bytes(&fixture_root(), CB);
        let a = d.entries.iter().find(|e| e.name == "A.TXT").unwrap();
        assert!(!a.deleted && !a.attr_dir && a.checksum_ok);
        assert_eq!(a.first_cluster, 6);
        assert_eq!((a.valid_data_length, a.data_length), (5, 5));
        assert!(a.contiguous);
        assert!(a.name_verified);
    }

    #[test]
    fn parses_deleted_set_with_full_name() {
        // exFAT 红利：删除后名字一字不差（对比 FAT 的 0xE5 首字符丢失）
        let d = parse_directory_bytes(&fixture_root(), CB);
        let g = d.entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(g.name, "G.BIN");
        assert!(g.checksum_ok, "还原校验必须通过");
        assert!(!g.contiguous);
        assert_eq!(g.data_length, 9000);
    }

    #[test]
    fn deleted_set_with_overwritten_byte_is_dropped() {
        let mut root = fixture_root();
        // 覆写删除项名字的某一字节（模拟槽位复用）→ 还原校验必失败 → 丢弃
        let name_byte = SET_OFF + 3 * 32 + 2 + 3 * 2; // 第二套件（0xC1）名字第 2 个码元低字节
        root[name_byte] ^= 0xFF;
        let d = parse_directory_bytes(&root, CB);
        assert!(
            !d.entries.iter().any(|e| e.deleted),
            "被覆写的删除项必须丢弃"
        );
        assert!(
            d.entries.iter().any(|e| e.name == "A.TXT"),
            "其他项不受影响"
        );
    }

    #[test]
    fn stops_at_end_marker() {
        let mut root = fixture_root();
        // 把"删除项集"整体后移一槽，原槽置 0x00 → 解析必须就此停下，不得把 0x00 之后当删除项
        let set: Vec<u8> = root[SET_OFF + 3 * 32..SET_OFF + 6 * 32].to_vec();
        root[SET_OFF + 4 * 32..SET_OFF + 7 * 32].copy_from_slice(&set);
        root[SET_OFF + 3 * 32] = 0x00;
        let d = parse_directory_bytes(&root, CB);
        assert!(!d.entries.iter().any(|e| e.deleted));
    }

    #[test]
    fn long_name_multi_entries() {
        let name = "ABCDEFGHIJKLMNOPQ"; // 17 码元 → 2 个 0xC1
        let img = ExfatImageBuilder::new().add_file("/", name, b"x").build();
        let d = parse_directory_bytes(root_of(&img), CB);
        assert_eq!(d.entries[0].name, name);
    }

    #[test]
    fn unicode_surrogate_pair_name() {
        let name = "📷照片.JPG"; // 代理对 + 中文（非 ASCII → name_verified 跳过但名字必须还原）
        let img = ExfatImageBuilder::new().add_file("/", name, b"x").build();
        let d = parse_directory_bytes(root_of(&img), CB);
        assert_eq!(d.entries[0].name, name);
    }

    #[test]
    fn bad_secondary_count_rejected_no_panic() {
        let mut root = fixture_root();
        root[SET_OFF + 1] = 0xFF; // A.TXT 的 SecondaryCount 改爆（A.TXT 集起于 SET_OFF）
        let _ = parse_directory_bytes(&root, CB); // 不 panic
        root[SET_OFF + 1] = 0x00; // =0 也不合法
        let d = parse_directory_bytes(&root, CB);
        assert!(!d.entries.iter().any(|e| e.name == "A.TXT"));
    }

    #[test]
    fn deleted_lookalike_garbage_rejected() {
        let mut root = fixture_root();
        // 删除项的第二槽改成 0x00（丢掉 0xC0）→ 还原后第二槽非 0xC0 → 丢弃
        root[SET_OFF + 3 * 32 + 32] = 0x00;
        let d = parse_directory_bytes(&root, CB);
        assert!(!d.entries.iter().any(|e| e.deleted));
    }

    #[test]
    fn deleted_dir_keeps_is_dir() {
        let img = ExfatImageBuilder::new().add_subdir("/", "DCIM").build();
        let mut root = root_of(&img).to_vec();
        // 模拟删除目录：只清 3 个槽的 bit7（这正是 exFAT 的删除语义，不重算 checksum）
        root[SET_OFF] &= 0x7F;
        root[SET_OFF + 32] &= 0x7F;
        root[SET_OFF + 64] &= 0x7F;
        let d = parse_directory_bytes(&root, CB);
        let dcim = d.entries.iter().find(|e| e.name == "DCIM").unwrap();
        assert!(dcim.deleted);
        assert!(
            dcim.attr_dir,
            "is_dir 必须忠实属性位——不得伪装成 0 字节文件"
        );
    }

    #[test]
    fn specials_extracted_from_root() {
        let img = ExfatImageBuilder::new().build();
        let d = parse_directory_bytes(root_of(&img), CB);
        assert_eq!(d.specials.bitmaps, vec![(false, 2, 32)]); // (SecondBitmap?, first, dl)
        assert_eq!(d.specials.upcase, Some((3, 5836)));
        assert_eq!(d.specials.label.as_deref(), Some("XIAODUN"));
    }

    #[test]
    fn set_crossing_cluster_boundary_rejected() {
        let img = ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"x")
            .build();
        let root = root_of(&img);
        let set: Vec<u8> = root[SET_OFF..SET_OFF + 3 * 32].to_vec();
        // 对照组：偏移 0 处放同一项集 → 可解析
        let mut ok = vec![0u8; 2 * CB];
        ok[..96].copy_from_slice(&set);
        assert_eq!(parse_directory_bytes(&ok, CB).entries.len(), 1);
        // 实验组：项集起始于簇边界前 80 字节（96 字节集必跨界）→ 必须拒绝
        let mut bad = vec![0u8; 2 * CB];
        let at = CB - 80;
        bad[at..at + 96].copy_from_slice(&set);
        assert!(
            parse_directory_bytes(&bad, CB).entries.is_empty(),
            "跨簇项集必须拒绝"
        );
    }

    #[test]
    fn inconsistent_vdl_dl_rejected() {
        // vdl > dl → 非法
        let img = ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"hello")
            .build();
        let mut root = root_of(&img).to_vec();
        root[SET_OFF + 32 + 8..SET_OFF + 32 + 16].copy_from_slice(&99u64.to_le_bytes());
        let d = parse_directory_bytes(&root, CB);
        assert!(!d.entries.iter().any(|e| e.name == "A.TXT"));
    }

    #[test]
    fn dl_zero_requires_first_zero() {
        // DataLength=0 但 FirstCluster≠0 → 非法（规范：FirstCluster==0 ↔ DL==0）
        let img = ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"hello")
            .build();
        let mut root = root_of(&img).to_vec();
        root[SET_OFF + 32 + 24..SET_OFF + 32 + 32].copy_from_slice(&0u64.to_le_bytes());
        let d = parse_directory_bytes(&root, CB);
        assert!(!d.entries.iter().any(|e| e.name == "A.TXT"));
    }
}
