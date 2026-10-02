// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 目录项解析：32 字节槽 → Sfn / Lfn / End / Free；名称组装（含删除项重建）。
//! 说明：parse_directory_bytes 跳过 "." / ".."（真实 FAT 目录均含；不是可恢复文件，也不得触发递归）。

pub const ATTR_LFN: u8 = 0x0F;
pub const ATTR_DIRECTORY: u8 = 0x10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sfn {
    pub name83: [u8; 11],
    pub attr: u8,
    pub nt_res: u8,
    pub first_cluster: u32,
    pub size: u32,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LfnSlot {
    /// 原始 seq 字节：存活项含顺序与 0x40 末标；删除项恒为 0xE5（序信息已丢）。
    pub seq_raw: u8,
    pub chars: Vec<u16>,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    Sfn(Sfn),
    Lfn(LfnSlot),
    End, // 0x00：目录结束（其后均为空闲区）
}

/// 分类一个 32 字节槽。
pub fn parse_slot(raw: &[u8; 32]) -> Slot {
    if raw[0] == 0x00 {
        return Slot::End;
    }
    let deleted = raw[0] == 0xE5;
    if raw[11] == ATTR_LFN {
        let mut chars = Vec::with_capacity(13);
        // 0x0000/0xFFFF 终止符按段生效（规范槽的后续段以 0xFFFF 填充）
        for &(a, b) in &[(1usize, 10usize), (14, 25), (28, 31)] {
            let mut i = a;
            while i < b + 1 && i < 31 {
                let u = u16::from_le_bytes([raw[i], raw[i + 1]]);
                if u == 0x0000 || u == 0xFFFF {
                    break;
                }
                chars.push(u);
                i += 2;
            }
        }
        return Slot::Lfn(LfnSlot {
            seq_raw: raw[0],
            chars,
            deleted,
        });
    }
    let mut name83 = [0u8; 11];
    name83.copy_from_slice(&raw[..11]);
    Slot::Sfn(Sfn {
        name83,
        attr: raw[11],
        nt_res: raw[12],
        first_cluster: ((u16::from_le_bytes([raw[20], raw[21]]) as u32) << 16)
            | u16::from_le_bytes([raw[26], raw[27]]) as u32,
        size: u32::from_le_bytes([raw[28], raw[29], raw[30], raw[31]]),
        deleted,
    })
}

/// 组装 8.3 名。`lcase` 为 NTRes 字节（raw[12]，bit3=基名小写, bit4=扩展名小写）；删除项首字符不可知 → '?'。
/// 字节按 Latin-1 逐字节映射为字符（SFN 是 OEM 码页原始字节，不做 UTF-8 猜测——0xE5 保留为 U+00E5）。
pub fn assemble_sfn_name(name83: &[u8; 11], lcase: u8) -> String {
    let mut base: Vec<u8> = name83[..8]
        .iter()
        .copied()
        .take_while(|&c| c != b' ')
        .collect();
    let mut ext: Vec<u8> = name83[8..]
        .iter()
        .copied()
        .take_while(|&c| c != b' ')
        .collect();
    if let Some(f) = base.first_mut() {
        if *f == 0x05 {
            *f = 0xE5; // 0x05 → 真实的 0xE5 首字节（非删除）
        } else if *f == 0xE5 {
            *f = b'?'; // 删除项首字符丢失
        }
    }
    let apply = |bytes: &mut [u8], lower: bool| {
        if lower {
            for b in bytes.iter_mut() {
                *b = b.to_ascii_lowercase();
            }
        }
    };
    apply(&mut base, lcase & 0x08 != 0);
    apply(&mut ext, lcase & 0x10 != 0);
    let mut out: String = base.iter().map(|&b| b as char).collect();
    if !ext.is_empty() {
        out.push('.');
        out.extend(ext.iter().map(|&b| b as char));
    }
    out
}

/// 组装 LFN。存活：按 seq 升序（0x40 标志在最后一段）。删除：物理序为尾→头，逆序拼接。
/// 不做 seq 连续性/0x40 末标/checksum 校验——畸形 run 按低 5 位排序尽力拼接（错名而非 panic）。
pub fn assemble_lfn(slots: &[LfnSlot]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if slots.iter().all(|s| s.deleted) {
        for s in slots.iter().rev() {
            parts.push(String::from_utf16_lossy(&s.chars));
        }
    } else {
        let mut ordered: Vec<&LfnSlot> = slots.iter().filter(|s| !s.deleted).collect();
        ordered.sort_by_key(|s| s.seq_raw & 0x1F);
        for s in ordered {
            parts.push(String::from_utf16_lossy(&s.chars));
        }
    }
    parts.concat()
}

/// 解析一整块目录数据为合并后的条目序列。
/// 规则：紧邻 SFN 之前、连续的 LFN 槽组归属该 SFN；End 槽终止解析；"." / ".." 跳过。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEntry {
    pub name: String,
    pub attr: u8,
    pub first_cluster: u32,
    pub size: u32,
    pub deleted: bool,
    pub is_dir: bool,
    pub has_lfn: bool,
}

pub fn parse_directory_bytes(data: &[u8]) -> Vec<ParsedEntry> {
    let mut out = Vec::new();
    let mut lfn_run: Vec<LfnSlot> = Vec::new();
    for chunk in data.chunks_exact(32) {
        let raw: &[u8; 32] = chunk.try_into().expect("chunks_exact(32)");
        // 跳过 "." / ".."（真实 FAT 目录均含；不是可恢复文件，也不得触发递归）
        if &raw[..11] == b".          " || &raw[..11] == b"..         " {
            continue;
        }
        match parse_slot(raw) {
            Slot::End => break,
            Slot::Lfn(l) => lfn_run.push(l),
            Slot::Sfn(s) => {
                // 删除态一致才配对：存活文件不得冠删除残留的 LFN 名
                let pair = !lfn_run.is_empty() && lfn_run.iter().all(|l| l.deleted) == s.deleted;
                let (name, has_lfn) = if !pair {
                    (assemble_sfn_name(&s.name83, s.nt_res), false)
                } else {
                    let joined = assemble_lfn(&lfn_run);
                    let fallback = assemble_sfn_name(&s.name83, s.nt_res);
                    if joined.trim().is_empty() {
                        (fallback, false) // 回退时 has_lfn 如实为 false
                    } else {
                        (joined, true)
                    }
                };
                lfn_run.clear();
                out.push(ParsedEntry {
                    name,
                    attr: s.attr,
                    first_cluster: s.first_cluster,
                    size: s.size,
                    deleted: s.deleted,
                    is_dir: s.attr & ATTR_DIRECTORY != 0,
                    has_lfn,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_plain_sfn() {
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(b"HELLO   TXT");
        raw[11] = 0x20;
        let e = parse_slot(&raw);
        assert_eq!(
            e,
            Slot::Sfn(Sfn {
                name83: *b"HELLO   TXT",
                attr: 0x20,
                nt_res: 0,
                first_cluster: 0,
                size: 0,
                deleted: false
            })
        );
    }

    #[test]
    fn decodes_deleted_sfn_first_byte_05_quirk() {
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(b"\x05EVIL   TXT"); // 真实名以 0xE5 开头时磁盘上写 0x05（"EVIL"+3 空格补齐 8.3）
        raw[11] = 0x20;
        let e = parse_slot(&raw);
        let Slot::Sfn(s) = e else { panic!() };
        assert_eq!(assemble_sfn_name(&s.name83, 0), "\u{E5}EVIL.TXT");
    }

    #[test]
    fn deleted_sfn_is_marked_and_name_loses_first_char() {
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(b"\xE5HOTO   JPG");
        raw[11] = 0x20;
        let Slot::Sfn(s) = parse_slot(&raw) else {
            panic!()
        };
        assert!(s.deleted);
        assert_eq!(assemble_sfn_name(&s.name83, 0), "?HOTO.JPG"); // 首字符不可知
    }

    #[test]
    fn skips_dot_entries() {
        let mut dot = [0u8; 32];
        dot[..11].copy_from_slice(b".          ");
        dot[11] = 0x10;
        let mut dotdot = [0u8; 32];
        dotdot[..11].copy_from_slice(b"..         ");
        dotdot[11] = 0x10;
        let mut file = [0u8; 32];
        file[..11].copy_from_slice(b"A       TXT");
        file[11] = 0x20;
        let mut data = Vec::new();
        data.extend_from_slice(&dot);
        data.extend_from_slice(&dotdot);
        data.extend_from_slice(&file);
        let parsed = parse_directory_bytes(&data);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "A.TXT");
    }

    fn lfn_chars_to_slot(seq: u8, last: bool, text: &str) -> LfnSlot {
        LfnSlot {
            seq_raw: if last { seq | 0x40 } else { seq },
            chars: text.encode_utf16().collect(),
            deleted: false,
        }
    }

    #[test]
    fn lfn_assembly_alive_uses_seq_order() {
        // 物理序：先末段（seq=2|0x40）后首段（seq=1）；组装按 seq 升序还原
        let a = lfn_chars_to_slot(2, true, "ng_na");
        let b = lfn_chars_to_slot(1, false, "my_lO");
        assert_eq!(assemble_lfn(&[a, b]), "my_lOng_na");
    }

    #[test]
    fn lfn_assembly_deleted_orphan_reverses_physical_run() {
        // 删除后 seq 字节全丢（0xE5）：按物理逆序拼接（run 内物理顺序是尾→头）
        let mk = |text: &str| LfnSlot {
            seq_raw: 0xE5,
            chars: text.encode_utf16().collect(),
            deleted: true,
        };
        let slots = vec![mk("0.bin"), mk("my_ph")]; // 物理序：先尾段 "0.bin" 后头段 "my_ph"
        assert_eq!(assemble_lfn(&slots), "my_ph0.bin");
    }

    #[test]
    fn lowercase_flags_come_from_ntres() {
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(b"README  TXT");
        raw[11] = 0x20; // attr=archive（bit3/4 不参与小写）
        raw[12] = 0x18; // NTRes：基名+扩展名小写
        let Slot::Sfn(s) = parse_slot(&raw) else {
            panic!()
        };
        assert_eq!(s.nt_res, 0x18);
        assert_eq!(assemble_sfn_name(&s.name83, s.nt_res), "readme.txt");
        // 卷标（attr 0x08）不得被误小写
        let mut vol = [0u8; 32];
        vol[..11].copy_from_slice(b"MYDISK     ");
        vol[11] = 0x08;
        let Slot::Sfn(s) = parse_slot(&vol) else {
            panic!()
        };
        assert_eq!(assemble_sfn_name(&s.name83, s.nt_res), "MYDISK");
    }

    /// LFN 槽内第 k 个 UTF-16 单元的字节偏移（窗口 1←0..4、2←5..10、3←11..12）。
    fn lfn_char_offset(k: usize) -> usize {
        match k {
            0..=4 => 1 + k * 2,
            5..=10 => 14 + (k - 5) * 2,
            _ => 28 + (k - 11) * 2,
        }
    }

    #[test]
    fn lfn_slots_parse_from_raw_bytes() {
        // 手工构造一条完整 LFN 槽（13 个 UTF-16 单元）——覆盖 (1,10)/(14,25)/(28,31) 三窗口
        let mut raw = [0u8; 32];
        raw[0] = 0x41; // seq=1 | 0x40（唯一段）
        raw[11] = 0x0F;
        let text: Vec<u16> = "photo_2024.jp".encode_utf16().collect();
        assert_eq!(text.len(), 13);
        for (k, &u) in text.iter().enumerate() {
            let i = lfn_char_offset(k);
            raw[i..i + 2].copy_from_slice(&u.to_le_bytes());
        }
        let Slot::Lfn(l) = parse_slot(&raw) else {
            panic!()
        };
        assert_eq!(String::from_utf16_lossy(&l.chars), "photo_2024.jp");
        assert!(!l.deleted);
    }

    #[test]
    fn mixed_run_falls_back_to_sfn_name() {
        // 删除孤儿 LFN + 存活 SFN → 不配对，用 SFN 名
        let mut lfn = [0u8; 32];
        lfn[0] = 0xE5; // 删除
        lfn[11] = 0x0F; // attr 在窗口外，须保持不被字符写入覆盖
        for (k, u) in "to.jpg".encode_utf16().enumerate() {
            let i = lfn_char_offset(k);
            lfn[i..i + 2].copy_from_slice(&u.to_le_bytes());
        }
        let mut sfn = [0u8; 32];
        sfn[..11].copy_from_slice(b"B       TXT");
        sfn[11] = 0x20;
        let mut data = Vec::new();
        data.extend_from_slice(&lfn);
        data.extend_from_slice(&sfn);
        let parsed = parse_directory_bytes(&data);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "B.TXT");
        assert!(!parsed[0].deleted);
        assert!(!parsed[0].has_lfn);
    }

    #[test]
    fn lfn_run_through_parse_directory_bytes() {
        // 构造：存活 LFN 槽（seq=1|0x40, "photo.jpg"）+ 存活 SFN（PHOTO   JPG）→ 名字取 LFN
        let mut lfn = [0u8; 32];
        lfn[0] = 0x41;
        lfn[11] = 0x0F; // attr 在窗口外，须保持不被字符写入覆盖
        for (k, u) in "photo.jpg".encode_utf16().enumerate() {
            let i = lfn_char_offset(k);
            lfn[i..i + 2].copy_from_slice(&u.to_le_bytes());
        }
        let mut sfn = [0u8; 32];
        sfn[..11].copy_from_slice(b"PHOTO   JPG");
        sfn[11] = 0x20;
        let mut data = Vec::new();
        data.extend_from_slice(&lfn);
        data.extend_from_slice(&sfn);
        let parsed = parse_directory_bytes(&data);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "photo.jpg");
        assert!(parsed[0].has_lfn);

        // 删除孤儿：两个删除 LFN 槽（物理序尾→头）+ 删除 SFN → 逆序重建名字
        let mk_deleted = |text: &str| {
            let mut s = [0u8; 32];
            s[0] = 0xE5; // 删除态 seq 全丢
            s[11] = 0x0F;
            for (k, u) in text.encode_utf16().enumerate() {
                let i = lfn_char_offset(k);
                s[i..i + 2].copy_from_slice(&u.to_le_bytes());
            }
            s
        };
        let mut dsfn = [0u8; 32];
        dsfn[..11].copy_from_slice(b"GONE    JPG");
        dsfn[11] = 0x20;
        dsfn[0] = 0xE5;
        let mut data2 = Vec::new();
        data2.extend_from_slice(&mk_deleted("gone_0")); // 尾段在前
        data2.extend_from_slice(&mk_deleted("old_")); // 头段在后
        data2.extend_from_slice(&dsfn);
        let parsed2 = parse_directory_bytes(&data2);
        assert_eq!(parsed2.len(), 1);
        assert_eq!(parsed2[0].name, "old_gone_0");
        assert!(parsed2[0].deleted);
    }

    #[test]
    fn deleted_subdir_entry_keeps_is_dir() {
        // 已删目录不得伪装成文件（M1d 会当 0 字节文件"恢复"）：is_dir 忠实 attr
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(b"\xE5LDDIR     ");
        raw[11] = ATTR_DIRECTORY;
        raw[26..28].copy_from_slice(&5u16.to_le_bytes());
        let parsed = parse_directory_bytes(&raw);
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].deleted);
        assert!(parsed[0].is_dir, "已删目录的 is_dir 必须忠实 attr");
        assert_eq!(parsed[0].name, "?LDDIR");
    }
}
