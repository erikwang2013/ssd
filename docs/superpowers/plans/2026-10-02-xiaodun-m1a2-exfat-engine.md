# 小盾 M1a2：exFAT 引擎实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 `xd-fs-exfat`（exFAT 只读解析 + 删除文件恢复：引导扇区/FAT32 表/分配位图/目录项集/扫描/读取）+ `xd-fixtures` exFAT 合成镜像 builder，达 M1a 对等品质量级。

**Architecture:** 与 `xd-fs-fat` 同构：`BlockDevice` 只读 trait → 分模块（boot / fattab / bitmap / dirent / scan）→ 保守降级 + 二档质量分级。关键差异（**必须内化**）：① 分配权威是**位图**不是 FAT（删除后 FAT 是 stale，内核从不写）；② 删除项 `0x85→0x05` 落在规范"unused entry"区间 `0x01..0x7F`——按"跳过 <0x80"实现会**静默丢失全部删除文件**；③ 删除**只清 bit7 且不重算 SetChecksum**——把类型位 `|=0x80` 还原后重算比对，是项集自洽的最强判据；④ `NoFatChain=1` 文件删除后仍连续是**规范保证**（不是猜测）；⑤ 三个 checksum 变体（Boot 跳 106/107/112 / EntrySet 跳 2/3 / Table 不跳）宽度与跳过规则均不同，各配 KAT。

**Tech Stack:** Rust（workspace 新 crate `xd-fs-exfat`；`xd-fixtures` 加 `exfat.rs`）。测试用 `ImageFileDevice` + `tempfile` 真实文件通路，与 M1a 同规。

**规格源**：`/tmp/xd-m1a2-brief.md`（Microsoft exFAT 规范 + Linux `fs/exfat`），**含三处已实证勘误**（以本计划为准）：
1. **0x82 Up-case 项字段偏移**：简报称 FirstCluster@28/DataLength@32（其结构与 32 字节槽宽矛盾）——**真实 mkfs.exfat 1.2.8 产物实证：FirstCluster@20、DataLength@24，与 0x81 同构**；
2. **簇宽**：简报称 spc_shift=3 为"2KB 簇"——实为 **8 扇区 = 4KB**（mkfs 真实产物同为 spc_shift=3/4KB，且 bitmap=2、upcase=3..4、root=5，本计划几何与之对齐）；
3. **0xC1 名字码元偏移**：简报称"码元在 1..30、[31] 保留"——实为 **偏移 1 保留、15 个 UTF-16LE 码元在偏移 2..32**。证据链：exfatprogs 1.2.0 源码 `include/exfat_ondisk.h`（`struct { __u8 flags; __le16 unicode_0_14[15]; } name;`）+ 按此布局手工构造的三种名字（ASCII/16 码元双 0xC1/中文）经 `fsck.exfat -n` **全部 clean**（exfatprogs 是生产级 Windows 互操作参考实现）。

**实证锚点（2026-10-02，mkfs.exfat 真实 8MB 镜像，`/tmp/xd-probe-exfat.img` 留档）**：BootChecksum 跳过 106/107/112 → stored==calc（912DFBC6）；Up-case 表 5836B、TableChecksum==0xE619D30D（sha256 8344f27a…，已作为资产入库）；fsck.exfat 对"按本计划配方构造的项集"判 clean（含 SetChecksum/NameHash/NameLength，KAT 值 `name_hash("TEST.TXT")==0x3368`、`name_hash("AB")==0x2029`）。**T1 完成后应本地跑 `fsck.exfat -n` 校验 fixture（预期 clean；CI 不依赖该工具）。**

**先例参照**：`docs/superpowers/plans/2026-10-02-xiaodun-m1a-fat-engine.md`（M1a；其"移交注"含必须对等继承的语义：根不可读→Err、is_dir 忠实 attr、保守降级、read_file 诚实短前缀、live 只信链）。

---

## 文件结构

```
crates/
├── xd-fixtures/
│   ├── src/exfat.rs            # ExfatImageBuilder + checksum/upcase 辅助 + 测试（T1）
│   ├── src/exfat_upcase.bin    # 规范推荐 Up-case 表资产（5836B，已入库，勿编辑）
│   └── src/lib.rs              # +mod exfat; pub use exfat::ExfatImageBuilder;
├── xd-fs-exfat/
│   ├── Cargo.toml              # 依赖 xd-device；dev-deps: xd-fixtures/tempfile
│   └── src/
│       ├── lib.rs              # ExfatError + 模块导出（T2）
│       ├── boot.rs             # 引导区解析（T2）
│       ├── fattab.rs           # 32 位 FAT 表 + chain()（T3）
│       ├── bitmap.rs           # 分配位图（T3）
│       ├── dirent.rs           # 目录项集解析（T4）
│       └── scan.rs             # 扫描 + 分级 + read_file（T5/T6）
├── xd-fs-exfat/tests/roundtrip.rs          # e2e（T7）
└── xd-fixtures/examples/gen_exfat_image.rs # 镜像生成示例（T7）
```

**几何常量（builder 与测试共享，对齐真实 mkfs 产物）**：bps_shift=9（512B 扇区）、spc_shift=3（**8 扇区 = 4KB 簇**）、volume_length=2048 扇区（1MiB，规范最小值）、fat_offset=24、fat_length=2、number_of_fats=1、cluster_heap_offset=32、cluster_count=252（= floor((2048-32)/8) 精确值）、**bitmap=簇 2、up-case=簇 3..4（FAT 链 3→4，5836B 占 2 簇）、root=簇 5**、文件簇从 6 起、volume_serial=0x1234_5678、revision=0x0100。

---

### Task 1: xd-fixtures —— exFAT 合成镜像 builder

**Files:**
- Create: `crates/xd-fixtures/src/exfat.rs`（资产 `src/exfat_upcase.bin` 已于 `015af1b` 入库）
- Modify: `crates/xd-fixtures/src/lib.rs`（+`mod exfat; pub use exfat::{ExfatImageBuilder, boot_checksum, UPCASE_TABLE};`——T2/T3 测试依赖后两项）

- [ ] **Step 1: 写失败的测试（`exfat.rs` 末尾 `#[cfg(test)] mod tests`）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn u16le(s: &[u8], o: usize) -> u16 { u16::from_le_bytes([s[o], s[o + 1]]) }
    fn u32le(s: &[u8], o: usize) -> u32 { u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]]) }
    fn u64le(s: &[u8], o: usize) -> u64 {
        let mut b = [0u8; 8]; b.copy_from_slice(&s[o..o + 8]); u64::from_le_bytes(b)
    }
    /// 根目录字节偏移（簇 5，4KB 簇：HEAP=32 扇区×512 + (5-2)×4096）
    const ROOT_OFF: usize = 32 * 512 + 3 * 4096;
    fn root(img: &[u8]) -> &[u8] { &img[ROOT_OFF..ROOT_OFF + 4096] }
    /// 文件项集固定从根槽 3 起（槽 0=0x83、槽 1=0x81、槽 2=0x82）
    const SET_OFF: usize = 3 * 32;

    #[test]
    fn build_is_deterministic() {
        let a = ExfatImageBuilder::new().add_file("/", "A.TXT", b"hello").build();
        let b = ExfatImageBuilder::new().add_file("/", "A.TXT", b"hello").build();
        assert_eq!(a, b);
        assert_eq!(a.len(), 2048 * 512); // 1 MiB
    }

    #[test]
    fn boot_region_valid() {
        let img = ExfatImageBuilder::new().build();
        assert_eq!(&img[0..3], &[0xEB, 0x76, 0x90]);
        assert_eq!(&img[3..11], b"EXFAT   ");
        assert_eq!(u16le(&img, 510), 0xAA55);
        for s in 1..=8usize { // 扩展引导扇区尾部签名
            let end = (s + 1) * 512 - 4;
            assert_eq!(u32le(&img, end), 0xAA55_0000, "ext boot sector {s}");
        }
        // 主 checksum 扇区 = 11 扇区范围 BootChecksum 的小端重复
        let expect = boot_checksum(&img[..512 * 11]);
        for k in 0..128usize { assert_eq!(u32le(&img, 512 * 11 + k * 4), expect); }
        // 备份区（12..23）同构且自洽
        let backup = &img[512 * 12..512 * 24];
        assert_eq!(&backup[3..11], b"EXFAT   ");
        assert_eq!(u32le(backup, 512 * 11), boot_checksum(&backup[..512 * 11]));
    }

    #[test]
    fn boot_checksum_skips_volume_flags_and_percent_in_use() {
        let mut img = ExfatImageBuilder::new().build();
        let before = boot_checksum(&img[..512 * 11]);
        img[106] ^= 0xFF; img[107] ^= 0xFF; img[112] ^= 0xFF; // 跳过区
        assert_eq!(boot_checksum(&img[..512 * 11]), before, "跳字节后必须不变");
        img[100] ^= 0xFF; // VolumeSerial 在范围内
        assert_ne!(boot_checksum(&img[..512 * 11]), before);
    }

    #[test]
    fn checksum_fold_kat() {
        // 手算：循环右移 1 位累加
        assert_eq!(boot_checksum(&[0x01]), 1);
        assert_eq!(boot_checksum(&[0x80, 0x01]), 0x41);
        // 回绕看的是**进位前**的 sum → 需要两字节才触发：[0x03,0x03] → 0x8000_0004
        assert_eq!(boot_checksum(&[0x03, 0x03]), 0x8000_0004);
        assert_eq!(entry_set_checksum(&[0x01]), 1);
        assert_eq!(entry_set_checksum(&[0x80, 0x01]), 0x41);
        assert_eq!(entry_set_checksum(&[0x03, 0x03]), 0x8004);
    }

    #[test]
    fn geometry_fields_parse_back() {
        let img = ExfatImageBuilder::new().build();
        assert_eq!(img[108], 9);   // BytesPerSectorShift
        assert_eq!(img[109], 3);   // SectorsPerClusterShift（8 扇区 = 4KB 簇）
        assert_eq!(img[110], 1);   // NumberOfFats
        assert_eq!(u32le(&img, 80), 24);   // FatOffset
        assert_eq!(u32le(&img, 84), 2);    // FatLength
        assert_eq!(u32le(&img, 88), 32);   // ClusterHeapOffset
        assert_eq!(u32le(&img, 92), 252);  // ClusterCount
        assert_eq!(u32le(&img, 96), 5);    // RootDirCluster
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
        for c in 0x0000u16..=0x0060 { assert_eq!(decoded[c as usize], c); }
        for c in 0x61u16..=0x7A { assert_eq!(decoded[c as usize], c - 0x20, "a-z → A-Z"); }
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
        assert_eq!(u32le(r, 64 + 20), 3);                    // FirstCluster@20
        assert_eq!(u64le(r, 64 + 24), 5836);                 // DataLength@24
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
            assert_eq!(bm[((c - 2) / 8) as usize] & bit, bit, "cluster {c} 应已分配");
        }
    }

    #[test]
    fn contiguous_file_keeps_fat_untouched() {
        let img = ExfatImageBuilder::new().add_file("/", "A.TXT", b"hello").build();
        let r = root(&img);
        let set = &r[SET_OFF..SET_OFF + 3 * 32];
        assert_eq!(set[0], 0x85);
        assert_eq!(set[1], 2); // SecondaryCount = 0xC0 + 1×0xC1
        assert_eq!(u16le(set, 4) & 0x20, 0x20); // Archive
        assert_eq!(set[32], 0xC0);
        assert_eq!(set[32 + 1], 0x03, "AllocationPossible|NoFatChain");
        assert_eq!(set[32 + 3], 5); // NameLength
        assert_eq!(u64le(set, 32 + 8), 5);   // ValidDataLength
        assert_eq!(u32le(set, 32 + 20), 6);  // FirstCluster（文件簇从 6 起）
        assert_eq!(u64le(set, 32 + 24), 5);  // DataLength
        assert_eq!(set[64], 0xC1);
        let name: Vec<u16> = (0..5).map(|i| u16le(set, 64 + 2 + i * 2)).collect();
        assert_eq!(String::from_utf16(&name).unwrap(), "A.TXT");
        // SetChecksum 与 NameHash 自洽
        assert_eq!(entry_set_checksum(set), u16le(set, 2));
        let upcased: Vec<u8> = "A.TXT".encode_utf16()
            .flat_map(|c| upcase_ascii(c).to_le_bytes()).collect();
        assert_eq!(name_hash(&upcased), u16le(set, 32 + 4));
        // NameHash KAT（由 fsck.exfat 实证过的参考值）
        assert_eq!(name_hash(&"TEST.TXT".encode_utf16().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>()), 0x3368);
        assert_eq!(name_hash(&"AB".encode_utf16().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>()), 0x2029);
        // 名字码元布局：偏移 1 保留、码元从偏移 2 起（勘误 #3）
        assert_eq!(set[64 + 1], 0, "偏移 1 应为保留字节");
        // NoFatChain=1 → FAT 未被写（entry(6) 仍为 0）；位图已置位
        assert_eq!(u32le(&img, 24 * 512 + 6 * 4), 0);
        let bm = &img[32 * 512..32 * 512 + 32];
        assert_eq!(bm[(6 - 2) / 8] & (1 << ((6 - 2) % 8)), 1 << ((6 - 2) % 8));
        // 数据落盘
        let data_off = 32 * 512 + (6 - 2) * 4096;
        assert_eq!(&img[data_off..data_off + 5], b"hello");
    }

    #[test]
    fn chained_allocation_writes_fat() {
        let img = ExfatImageBuilder::new().add_file_chained("/", "B.BIN", &[7u8; 5000]).build();
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
        assert_eq!(u32le(set, 32 + 20), 6, "FirstCluster 保留（恢复的黄金信息）");
        assert_eq!(u64le(set, 32 + 24), 9000);
        // SetChecksum 未重算：还原类型位后重算 == 存储值
        let mut restored = set.to_vec();
        restored[0] |= 0x80; restored[32] |= 0x80; restored[64] |= 0x80;
        assert_eq!(entry_set_checksum(&restored), u16le(set, 2), "还原校验必须通过");
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
        for i in 0..45u32 { b.add_file("/", &format!("F{i:04}.TXT"), b"x"); }
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
        let _ = ExfatImageBuilder::new().add_file("/", "BIG.BIN", &big).build();
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
        let _ = ExfatImageBuilder::new().add_file("/NOPE", "A.TXT", b"x").build();
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xd-fixtures`
Expected: 编译失败（`ExfatImageBuilder`/`boot_checksum`/`entry_set_checksum`/`table_checksum`/`name_hash`/`upcase_ascii`/`decode_upcase`/`UPCASE_TABLE` 未定义）。

- [ ] **Step 3: 实现 `crates/xd-fixtures/src/exfat.rs`**

```rust
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
        if skip.contains(&i) { continue; }
        sum = (if sum & 1 != 0 { 0x8000_0000 } else { 0 })
            .wrapping_add(sum >> 1)
            .wrapping_add(*b as u32);
    }
    sum
}

/// 16 位变体（EntrySet/NameHash 用）。
pub fn fold16(bytes: &[u8], skip: &[usize]) -> u16 {
    let mut sum: u16 = 0;
    for (i, b) in bytes.iter().enumerate() {
        if skip.contains(&i) { continue; }
        sum = (if sum & 1 != 0 { 0x8000 } else { 0 })
            .wrapping_add(sum >> 1)
            .wrapping_add(*b as u16);
    }
    sum
}

pub fn boot_checksum(region11: &[u8]) -> u32 { fold32(region11, &[106, 107, 112]) }
pub fn entry_set_checksum(set: &[u8]) -> u16 { fold16(set, &[2, 3]) }
pub fn table_checksum(table: &[u8]) -> u32 { fold32(table, &[]) }
pub fn name_hash(upcased_utf16le: &[u8]) -> u16 { fold16(upcased_utf16le, &[]) }

/// 前 128 码元强制映射（§7.2.5 Table 24）：a-z → A-Z，其余恒等。
pub fn upcase_ascii(c: u16) -> u16 {
    if (0x61..=0x7A).contains(&c) { c - 0x20 } else { c }
}

/// 解码压缩 up-case 表（§7.2.5：uni==index 恒等；0xFFFF → 下一 u16 为恒等个数）。
pub fn decode_upcase(compressed: &[u8]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    let mut skip = false;
    let mut i = 0usize;
    while i + 1 < compressed.len() {
        let uni = u16::from_le_bytes([compressed[i], compressed[i + 1]]);
        i += 2;
        if skip {
            for _ in 0..uni { out.push(out.len() as u16); }
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
        while self.allocated.contains(&self.cursor) { self.cursor += 1; }
        assert!(self.cursor <= CLUSTER_COUNT + 1, "cluster 耗尽（需要更多簇）");
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
        assert!((1..=255).contains(&units), "name 长度非法（{units} 个 UTF-16 码元）：{name}");
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

    fn push_file(&mut self, dir: &str, name: &str, data: &[u8], vdl: u64, contiguous: bool, _pad: Option<()>) -> &mut Self {
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
        let path = if parent == "/" { format!("/{name}") } else { format!("{parent}/{name}") };
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
            img[b] = 0xEB; img[b + 1] = 0x76; img[b + 2] = 0x90;
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
            img[b + 110] = 1;    // NumberOfFats
            img[b + 111] = 0x80; // DriveSelect
            img[b + 112] = 0xFF; // PercentInUse = 未知
            img[b + 510] = 0x55; img[b + 511] = 0xAA;
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
        let mut paths: Vec<&String> = self.dirs.keys().collect();
        paths.sort(); // 确定性（HashMap 迭代序不定）
        for p in &paths { dir_bufs.insert((*p).clone(), Vec::new()); }

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
            bm[24..32].copy_from_slice(&((CLUSTER_COUNT as usize).div_ceil(8) as u64).to_le_bytes());
            r.extend_from_slice(&bm);
            // 槽 2：0x82 Up-case（同偏移规则；TableChecksum@4）
            let mut uc = [0u8; 32];
            uc[0] = 0x82;
            uc[4..8].copy_from_slice(&table_checksum(UPCASE_TABLE).to_le_bytes());
            uc[20..24].copy_from_slice(&UPCASE_CLUSTERS[0].to_le_bytes());
            uc[24..32].copy_from_slice(&(UPCASE_TABLE.len() as u64).to_le_bytes());
            r.extend_from_slice(&uc);
        }

        // 子目录项集（写入父目录；确定性顺序）
        for p in &paths {
            if *p == "/" { continue; }
            let name = p.rsplit('/').next().unwrap();
            let clusters = &self.dirs[*p];
            let parent = match p.rfind('/') {
                Some(0) => "/".to_string(),
                Some(i) => p[..i].to_string(),
                None => unreachable!(),
            };
            let bytes = build_entry_set(name, 0x10, clusters[0], (clusters.len() * CBS) as u64, true, (clusters.len() * CBS) as u64);
            dir_bufs.get_mut(&parent).unwrap().extend_from_slice(&bytes);
        }
        // 文件项集（插入序）
        for f in &self.files {
            let first = f.clusters.first().copied().unwrap_or(0);
            let bytes = build_entry_set(
                &f.name, 0x20, first, f.data.len() as u64, f.contiguous && !f.clusters.is_empty(), f.vdl,
            );
            let mut bytes = bytes;
            if f.deleted {
                for i in (0..bytes.len()).step_by(32) { bytes[i] &= 0x7F; }
            }
            dir_bufs.get_mut(&f.parent).unwrap().extend_from_slice(&bytes);
        }
        // 子目录容量（1 簇）fail-fast
        for p in &paths {
            if *p != "/" {
                assert!(
                    dir_bufs[*p].len() <= CBS,
                    "子目录 {p} 槽位超出 1 簇（{}B）——夹具不支持子目录扩容",
                    dir_bufs[*p].len()
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
        img[fat + 4..fat + 8].copy_from_slice(&EOC.to_le_bytes());        // entry[1]
        let mut write_chain = |clusters: &[u32]| {
            for w in clusters.windows(2) {
                let e = fat + w[0] as usize * 4;
                img[e..e + 4].copy_from_slice(&w[1].to_le_bytes());
            }
            let e = fat + *clusters.last().unwrap() as usize * 4;
            img[e..e + 4].copy_from_slice(&EOC.to_le_bytes());
        };
        write_chain(&UPCASE_CLUSTERS);            // 3→4→EOC
        write_chain(&self.dirs["/"]);             // 5→…→EOC（含扩容链）
        for p in &paths {
            if *p != "/" { write_chain(&self.dirs[*p]); }
        }
        for f in &self.files {
            if !f.contiguous && !f.clusters.is_empty() { write_chain(&f.clusters); }
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
                if start >= end { break; }
                let off = Self::cluster_byte(*c);
                img[off..off + (end - start)].copy_from_slice(&f.data[start..end]);
            }
        }

        // ---- 目录槽落盘 ----
        for p in &paths {
            let buf = &dir_bufs[*p];
            for (i, chunk) in buf.chunks(CBS).enumerate() {
                let off = Self::cluster_byte(self.dirs[*p][i]);
                img[off..off + chunk.len()].copy_from_slice(chunk);
            }
        }

        img
    }
}

impl Default for ExfatImageBuilder {
    fn default() -> Self { Self::new() }
}

/// 组装一个 0x85/0xC0/0xC1×N 项集（含 SetChecksum 与 NameHash）。
fn build_entry_set(name: &str, attr: u16, first_cluster: u32, dl: u64, no_fat_chain: bool, vdl: u64) -> Vec<u8> {
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
    stream[1] = if no_fat_chain && first_cluster != 0 { 0x03 } else { 0x01 };
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
    let upcased: Vec<u8> = units.iter().flat_map(|c| upcase_ascii(*c).to_le_bytes()).collect();
    set[32 + 4..32 + 6].copy_from_slice(&name_hash(&upcased).to_le_bytes());
    // SetChecksum（在"未删除"形态下计算；删除只清 bit7 不重算）
    let sum = entry_set_checksum(&set);
    set[2..4].copy_from_slice(&sum.to_le_bytes());
    set
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p xd-fixtures`
Expected: **22 passed（15 `#[test]` + 7 `#[should_panic]`）**——基础 16 之上含修复轮新增 6：`upcase_hashes_accented_names_via_real_table`、`empty_file_is_valid`、`panics_when_explicit_clusters_too_few`、`panics_on_allocation_conflict`、`panics_on_vdl_exceeding_data`、`panics_on_subdir_slot_overflow`（见修订轮）；既有 FAT builder 12 测试不回归。

- [ ] **Step 5: Commit**

```bash
git add crates/xd-fixtures
git commit -m "feat(fixtures): exFAT 合成镜像 builder（几何/checksum/项集/位图/删除语义）"
```

**修订轮（执行侧，2026-10-02）**：实施提交 `1e74b91`（分支 m1a2-exfat）。计划字面代码有 5 处被执行侧修正，
经 spec 审查逐一判定**接受**（均有独立验证）：

- **① 编译性**：fold 字面量类型标注；`paths: Vec<&String>`（借用冲突）→ `Vec<String>`。语义零变化（字符级 diff 穷举）。
- **② 计划测试硬伤**：`boot_checksum(&[0x03])`/`entry_set_checksum(&[0x03])` 期望值是 `[0x03,0x03]` 的折叠值
  （单字节恒为 3——回绕分支看进位前的 sum）→ 输入改两字节；`c(8)+904` → `+808`（9000−8192）。本文件已就地修正。
- **③ ★语义性（必需）**：新增 **`push_dir_unit`**——目录项集**簇对齐**放置，剩余槽以 **0x01**（unused 标记）补齐。
  原计划的流式拼接会让第 42 个文件的项集跨根目录簇边界：**违反规范"项集不得跨簇"，且该计划的 T4 解析器
  自己会拒绝它、`root_grows` 测试也与之矛盾**（流式布局下第二根簇首槽为 0xC1 而非 0x85）。独立解析器验证：
  补齐版 45/45 还原、流式版 44/45（恰好丢 F0041.TXT）。
  **napkin 记录**：`fsck.exfat` **不强制**"项集不跨簇"（流式镜像 fsck 仍判 clean）——该规则的 oracle 是本计划
  的解析器规则（`i / cb != (end-1) / cb → 拒绝`），不是 fsck。0x01 补齐的合法性已由 fsck 实证（45 文件全数枚举）。
- **④ 卫生**：`UPCASE_TABLE_CHECKSUM` 直写 0x82 + `debug_assert_eq!(table_checksum(UPCASE_TABLE), …)`；
  `decode_upcase` 仅测试使用 → `#[cfg(test)]`。
- **⑤ rustfmt** 展开。

**下游常量（T4-T7 依赖）**：根目录多簇时项集簇对齐 + 0x01 填充；root_grows 场景文件占簇 6..50、第二根簇 51、
首集为 `F0041.TXT`。

**修复轮（qual-t1，2026-10-02）**：质量审查 With fixes（3 Important 均计划文本问题，实现忠实照抄），
修复提交 **`cf4e7e7`**（"fix(fixtures): NameHash 走规范表上转型、显式簇数守卫与补齐/断言补测（qual-t1）"）：

- **I1**：NameHash 上转型改为**规范表驱动**（`OnceLock` + `decode_upcase(UPCASE_TABLE)`，decode_un-gate 进生产路径）——
  é→É、α→Α 等非 ASCII 映射不可漏（实证 `Café.TXT` 旧值 0x7C06 ≠ 表值 **0x6C06**，fsck 判 name hash wrong）；
  新测试钉 0x6C06。ASCII 结果不变 → 下游零影响。
- **I2**：`add_file_in_clusters` 加"簇数容得下数据"断言（**计划 T6 配方自己踩中**：`&[7]` 装 4500B 静默丢尾、
  fsck 判损）→ 断言 + 计划 T6 配方改 `&[7, 8]`（qual 验证断言结果不变）。
- **I3(a)**：root_grows 补 0x01 补齐断言（槽 126/127）——防回归成 0x00 导致解析器半途终止；
  **I3(b)**：T5 新增 `scans_multi_cluster_root_completely`（45 文件多簇根 → 45 条全出）。
- **M1-M4**：push_dir_unit debug_assert（落地为 `is_multiple_of` 形式，rust 1.97 clippy 门禁所迫）；
  4 个 should_panic + 空文件测试；删 `_pad` 死参；删 `upcase_ascii`（其测试自洽校验改用 `upcase_table()`）。
- **M5（拆分，不阻塞）**：exfat.rs 916 行 → M1b 欠账（缝：纯 checksum/upcase 函数约 90 行 → exfat_checksum.rs）。

计数：xd-fixtures **34**（22 exfat + 12 FAT）、workspace **110**。

---

### Task 2: xd-fs-exfat —— 引导区解析（boot.rs）

**Files:**
- Create: `crates/xd-fs-exfat/Cargo.toml`、`crates/xd-fs-exfat/src/lib.rs`、`crates/xd-fs-exfat/src/boot.rs`
- Modify: 根 `Cargo.toml`（workspace members + `xd-fs-exfat`）

- [ ] **Step 1: 测试模块（boot.rs 末尾）**

```rust
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
        let cases: &[(usize, Vec<u8>, &str)] = &[
            (108, vec![8], "bps_shift=8"),
            (109, vec![17], "spc_shift=17"),
            (110, vec![3], "number_of_fats=3"),
            (80, 23u32.to_le_bytes().to_vec(), "fat_offset=23"),
            (92, 0u32.to_le_bytes().to_vec(), "cluster_count=0"),
            (96, 300u32.to_le_bytes().to_vec(), "root=300"),
        ];
        for (off, bytes, what) in cases {
            let mut img = xd_fixtures::ExfatImageBuilder::new().build();
            patch_boot_both(&mut img, &[(*off, bytes.as_slice())]);
            let (_f, dev) = dev_for(&img);
            assert!(parse(&dev).is_err(), "{what} 应被拒绝");
        }
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
        img[200] ^= 0xFF;              // 主区
        img[12 * 512 + 200] ^= 0xFF;   // 备区
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
    }

    #[test]
    fn active_fat_second_when_flagged() {
        let mut img = xd_fixtures::ExfatImageBuilder::new().build();
        patch_boot_both(&mut img, &[(110, &2u8.to_le_bytes()), (106, &1u16.to_le_bytes())]); // texFAT + ActiveFat=1
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
```

- [ ] **Step 2: 运行确认失败**（`cargo test -p xd-fs-exfat` → 编译失败）

- [ ] **Step 3: 实现**

`crates/xd-fs-exfat/Cargo.toml`：
```toml
[package]
name = "xd-fs-exfat"
version = "0.1.0"
edition = "2021"

[dependencies]
xd-device = { path = "../xd-device" }

[dev-dependencies]
xd-fixtures = { path = "../xd-fixtures" }
tempfile = "3"
```

`crates/xd-fs-exfat/src/lib.rs`：
```rust
//! xd-fs-exfat：exFAT 只读解析与删除文件恢复（只读铁律：无任何写设备路径）。
pub mod bitmap;
pub mod boot;
pub mod dirent;
pub mod fattab;
pub mod scan;

/// 与 `xd_fs_fat::FatError` 同构（non_exhaustive）。
#[derive(Debug)]
#[non_exhaustive]
pub enum ExfatError {
    /// 引导区/几何/结构非法
    InvalidBoot(String),
    /// 设备读取失败
    Io(String),
}

impl std::fmt::Display for ExfatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExfatError::InvalidBoot(m) => write!(f, "invalid exfat boot: {m}"),
            ExfatError::Io(m) => write!(f, "exfat io: {m}"),
        }
    }
}
impl std::error::Error for ExfatError {}
```
（T2 先只 `pub mod boot;`，其余模块随各任务追加；`lib.rs` 的 mod 行按任务逐步加，编译保持绿。）

`crates/xd-fs-exfat/src/boot.rs`：
```rust
//! exFAT 引导区解析：EXFAT 签名分派、几何交叉校验、BootChecksum（跳 106/107/112）、
//! Main 失败回退 Backup（backup_used=true，scan 据此降级）。
//! 实证：跳过规则与真实 mkfs.exfat 产物 stored==calc 验证一致（见 M1a2 计划头部）。

use crate::ExfatError;
use xd_device::BlockDevice;

const EXFAT_SIG: &[u8; 8] = b"EXFAT   ";

#[derive(Debug, Clone)]
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
    pub fn sector_bytes(&self) -> u64 { 1u64 << self.bytes_per_sector_shift }
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
        if self.number_of_fats == 2 && self.volume_flags & 0x1 == 1 { 1 } else { 0 }
    }
    pub fn active_fat_offset(&self) -> u32 {
        self.fat_offset + self.active_fat_index() as u32 * self.fat_length
    }
}

fn u16le(s: &[u8], o: usize) -> u16 { u16::from_le_bytes([s[o], s[o + 1]]) }
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
        if i == 106 || i == 107 || i == 112 { continue; }
        sum = (if sum & 1 != 0 { 0x8000_0000 } else { 0 })
            .wrapping_add(sum >> 1)
            .wrapping_add(*b as u32);
    }
    sum
}

/// 校验一个引导区域的 checksum 扇区（区域起点 sector_offset，取所在 11 扇区范围）。
fn region_valid(dev: &dyn BlockDevice, sector_offset: u64, bps: u64) -> Result<bool, ExfatError> {
    let mut region = vec![0u8; (bps * 11) as usize];
    let n = read_at(dev, sector_offset * bps, &mut region)?;
    if n < region.len() { return Ok(false); } // 读不满 → 视为无效（不是 Err：可能只是 Backup 在设备外）
    let mut cks = [0u8; 4];
    if read_at(dev, (sector_offset + 11) * bps, &mut cks)? < 4 { return Ok(false); }
    Ok(boot_checksum(&region) == u32::from_le_bytes(cks))
}

fn read_at(dev: &dyn BlockDevice, off: u64, buf: &mut [u8]) -> Result<usize, ExfatError> {
    dev.read_at(off, buf).map_err(|e| ExfatError::Io(e.to_string()))
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
        return Err(bad("bytes_per_sector_shift 越界"));
    }
    if sectors_per_cluster_shift > 25 - bytes_per_sector_shift {
        return Err(bad("sectors_per_cluster_shift 越界"));
    }
    if number_of_fats != 1 && number_of_fats != 2 {
        return Err(bad("number_of_fats 非法"));
    }
    if fat_offset < 24 || fat_length < 1 {
        return Err(bad("fat_offset/fat_length 非法"));
    }
    let sector_bytes = 1u64 << bytes_per_sector_shift;
    let heap_min = fat_offset as u64 + fat_length as u64 * number_of_fats as u64;
    if cluster_heap_offset as u64 < heap_min {
        return Err(bad("cluster_heap_offset 小于 FAT 区末端"));
    }
    if volume_length < cluster_heap_offset as u64 {
        return Err(bad("volume_length 小于 cluster_heap_offset"));
    }
    if cluster_count < 1 || cluster_count > 0xFFFF_FFF6 {
        return Err(bad("cluster_count 越界"));
    }
    if !(2..=cluster_count as u64 + 1).contains(&(root_cluster as u64)) {
        return Err(bad("root_cluster 越界"));
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
    let bps = 1u64 << head[108];
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
    if read_at(dev, 12 * bps, &mut backup)? < bps as usize
        || !region_valid(dev, 12, bps)?
    {
        return Err(ExfatError::InvalidBoot("Main 与 Backup 引导区校验均失败".into()));
    }
    let mut boot = geometry(&backup)?;
    boot.backup_used = true;
    Ok(boot)
}
```

- [ ] **Step 4: 运行 `cargo test -p xd-fs-exfat`** → 预期 **9 passed**；workspace 其余不回归。（T2 无测试计数变化于 T1 修复轮。）

- [ ] **Step 5: Commit** `feat(fs-exfat): 引导区解析（几何校验/checksum/Backup 回退）`

---

### Task 3: xd-fs-exfat —— 32 位 FAT 与分配位图（fattab.rs + bitmap.rs）

**Files:**
- Create: `crates/xd-fs-exfat/src/fattab.rs`、`crates/xd-fs-exfat/src/bitmap.rs`
- Modify: `crates/xd-fs-exfat/src/lib.rs`（+两行 mod）

- [ ] **Step 1: 测试。先把 `boot.rs` 测试模块中的 `dev_for` 与 `patch_boot_both` 提升为
  `#[cfg(test)] pub(crate) mod testutil`（boot.rs 内），fattab/bitmap 测试模块以
  `use crate::boot::testutil::{dev_for, patch_boot_both};` 引用（单一来源，避免三份副本漂移）**

fattab 测试：
```rust
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
    }

    #[test]
    fn chain_breaks_on_zero_not_treats_as_cluster_zero() {
        // B.BIN 链式 6→7→EOC；把 FAT[7] 清零 → 链应为 [6,7]（0 视为断裂），不是 [6,7,0,...]
        let mut img = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "B.BIN", &[7u8; 5000]).build();
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
            .add_file_chained("/", "B.BIN", &[7u8; 100]).build();
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
            bytes_per_sector_shift: 9, sectors_per_cluster_shift: 3, number_of_fats: 1,
            fat_offset: 24, fat_length: 2, cluster_heap_offset: 32, cluster_count: 252,
            root_cluster: 5, volume_length: 2048, volume_flags: 0, backup_used: false,
        };
        let fat = Fat32::new(&dev, &boot);
        assert!(matches!(fat.next_raw(6), Err(ExfatError::InvalidBoot(_))));
    }

    #[test]
    fn active_fat_selection_reads_second_fat() {
        // 主 FAT entry(6)=7（B.BIN 两簇 6→7）；第二 FAT entry(6)=EOC；
        // ActiveFat=1 → 若正确读第二 FAT：chain(6)==[6]；若错读第一 FAT：[6,7]
        let mut img = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "B.BIN", &[7u8; 5000]).build();
        // 第二 FAT 区（fat_offset+fat_length = 26 扇区起）
        let second = (24 + 2) * 512;
        img[second + 6 * 4..second + 6 * 4 + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        patch_boot_both(&mut img, &[(110, &2u8.to_le_bytes()), (106, &1u16.to_le_bytes())]);
        let (_f, dev) = dev_for(&img);
        let boot = boot::parse(&dev).unwrap();
        assert_eq!(boot.active_fat_index(), 1);
        let fat = Fat32::new(&dev, &boot);
        assert_eq!(fat.chain(6).unwrap(), vec![6]);
    }
```

bitmap 测试：
```rust
    #[test]
    fn loads_and_queries_fixture_bitmap() {
        // A.TXT 占簇 6 → 位已置；位图项在根目录
        let image = xd_fixtures::ExfatImageBuilder::new().add_file("/", "A.TXT", b"hello").build();
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
        assert!(Bitmap::load(&dev, &boot, &fat, 0, 32).is_err());   // first_cluster < 2
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
```

- [ ] **Step 2: 运行确认失败**

- [ ] **Step 3: 实现**

`fattab.rs`：
```rust
//! exFAT 32 位 FAT：EOC 是精确值 0xFFFFFFFF（非区间！）、坏簇 0xFFFFFFF7、
//! 0 视为链断裂；ActiveFat 由 VolumeFlags bit0 选择（texFAT）。

use crate::ExfatError;
use crate::boot::ExfatBoot;
use xd_device::BlockDevice;

pub const EOC: u32 = 0xFFFF_FFFF;
pub const BAD_CLUSTER: u32 = 0xFFFF_FFF7;

pub fn is_eoc(v: u32) -> bool { v == EOC }

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
            return Err(ExfatError::InvalidBoot(format!("fat entry {c} beyond device")));
        }
        Ok(u32::from_le_bytes(b))
    }

    /// 顺链收集。起点须 2..=count+1；EOC/0/坏簇/越界值即停；环 → 有界长链（len == count+2）。
    pub fn chain(&self, start: u32) -> Result<Vec<u32>, ExfatError> {
        let max_cluster = self.boot.cluster_count as u64 + 1;
        if !(2..=max_cluster).contains(&(start as u64)) {
            return Err(ExfatError::InvalidBoot(format!("chain start {start} out of range")));
        }
        let mut out = vec![start];
        let mut cur = start;
        while (out.len() as u64) <= max_cluster {
            let v = self.next_raw(cur)?;
            if v == 0 || is_eoc(v) || v == BAD_CLUSTER || (v as u64) > max_cluster {
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
    if let Ok(chain) = fat.chain(first_cluster) {
        if chain.len() as u64 >= need {
            clusters = chain[..need as usize].to_vec();
        }
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
        if n == 0 { break; }
        out.extend_from_slice(&buf[..n]);
        if out.len() as u64 >= data_length || n < buf.len() {
            break;
        }
    }
    if (out.len() as u64) < data_length {
        return Err(ExfatError::InvalidBoot("allocation 读不满（设备截断？）".into()));
    }
    out.truncate(data_length as usize);
    Ok(out)
}
```

`bitmap.rs`：
```rust
//! 分配位图（§7.1）：**exFAT 的分配权威**（删除后 FAT 是 stale）。
//! 位下标 = cluster - 2；字节内 LSB 优先；位 = 1 已分配/坏簇、0 空闲。
//! 位下标 ≥ cluster_count 为保留位——不得解读（is_free 直接拒绝越界簇号）。
//! DataLength ≥ ceil(cluster_count/8)（小于 = 错误）；大于合法（多出为保留位）。

use crate::ExfatError;
use crate::boot::ExfatBoot;
use crate::fattab::{Fat32, read_allocation};

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
            return Err(ExfatError::InvalidBoot("bitmap DataLength 小于 ceil(count/8)".into()));
        }
        let bytes = read_allocation(dev, boot, fat, first_cluster, data_length)?;
        Ok(Bitmap { bytes, cluster_count: boot.cluster_count })
    }

    /// 簇 `c` 是否空闲。`c` 须满足 2..=cluster_count+1；保留位/越界 → Err（不得解读）。
    pub fn is_free(&self, c: u32) -> Result<bool, ExfatError> {
        if !(2..=self.cluster_count as u64 + 1).contains(&(c as u64)) {
            return Err(ExfatError::InvalidBoot(format!("cluster {c} 越界（保留位不得解读）")));
        }
        let idx = (c - 2) as usize;
        let byte = self
            .bytes
            .get(idx / 8)
            .ok_or_else(|| ExfatError::InvalidBoot("bitmap 数据不足（不应到达）".into()))?;
        Ok(byte & (1 << (idx % 8)) == 0)
    }
}
```

- [ ] **Step 4: 运行** → fattab 6 + bitmap 7 = **13 passed**（累计 crate **22**）

- [ ] **Step 5: Commit** `feat(fs-exfat): 32 位 FAT 链与分配位图（ActiveFat/保留位纪律/连续回退）`

---

### Task 4: xd-fs-exfat —— 目录项集解析（dirent.rs）

**Files:**
- Create: `crates/xd-fs-exfat/src/dirent.rs`
- Modify: `crates/xd-fs-exfat/src/lib.rs`（+`pub mod dirent;`）

**核心语义（照 §3，含勘误 #3）**：
- `0x00` = 目录结束；`0x01..0x7F` = unused/无效槽——**删除项（0x05/0x40/0x41）也落此区间**，绝不能"见到 <0x80 就跳过"；识别靠：`t & 0x7F == 0x05` + 结构自洽 + **还原校验**（`|=0x80` 后重算 SetChecksum == 磁盘值）。
- 名字码元在 0xC1 **偏移 2 起**（偏移 1 保留）；NameLength@0xC0 偏移 3；NameHash@偏移 4（ASCII 名逐单位校验，非 ASCII 跳过——不作门槛的字段记录）。
- 项集不得跨簇边界（传入 `cluster_bytes` 判定）；SecondaryCount 合法区间 1..=18；0xC0 必须是第一个次项。
- 删除项门槛：还原校验失败 → **丢弃**（不做"尽力拼接"）。

- [ ] **Step 1: 测试（dirent.rs 末尾）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use xd_fixtures::ExfatImageBuilder;

    const ROOT_OFF: usize = 32 * 512 + 3 * 4096;
    const CB: usize = 4096;
    const SET_OFF: usize = 3 * 32; // 根槽 3 起

    fn root_of(img: &[u8]) -> &[u8] { &img[ROOT_OFF..ROOT_OFF + CB] }

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
        assert!(!d.entries.iter().any(|e| e.deleted), "被覆写的删除项必须丢弃");
        assert!(d.entries.iter().any(|e| e.name == "A.TXT"), "其他项不受影响");
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
        root[SET_OFF + 3 * 32 + 1] = 0xFF; // A.TXT 的 SecondaryCount 改爆
        let _ = parse_directory_bytes(&root, CB); // 不 panic
        root[SET_OFF + 3 * 32 + 1] = 0x00; // =0 也不合法
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
        assert!(dcim.attr_dir, "is_dir 必须忠实属性位——不得伪装成 0 字节文件");
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
        let img = ExfatImageBuilder::new().add_file("/", "A.TXT", b"x").build();
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
        assert!(parse_directory_bytes(&bad, CB).entries.is_empty(), "跨簇项集必须拒绝");
    }

    #[test]
    fn inconsistent_vdl_dl_rejected() {
        // vdl > dl → 非法
        let img = ExfatImageBuilder::new().add_file("/", "A.TXT", b"hello").build();
        let mut root = root_of(&img).to_vec();
        root[SET_OFF + 32 + 8..SET_OFF + 32 + 16].copy_from_slice(&99u64.to_le_bytes());
        let d = parse_directory_bytes(&root, CB);
        assert!(!d.entries.iter().any(|e| e.name == "A.TXT"));
    }

    #[test]
    fn dl_zero_requires_first_zero() {
        // DataLength=0 但 FirstCluster≠0 → 非法（规范：FirstCluster==0 ↔ DL==0）
        let img = ExfatImageBuilder::new().add_file("/", "A.TXT", b"hello").build();
        let mut root = root_of(&img).to_vec();
        root[SET_OFF + 32 + 24..SET_OFF + 32 + 32].copy_from_slice(&0u64.to_le_bytes());
        let d = parse_directory_bytes(&root, CB);
        assert!(!d.entries.iter().any(|e| e.name == "A.TXT"));
    }
}
```

- [ ] **Step 2: 运行确认失败** → **Step 3: 实现**

```rust
//! exFAT 目录项集解析。删除识别三件套：`t & 0x7F == 0x05` + 结构自洽 + 还原校验
//! （类型位 |=0x80 后重算 SetChecksum == 磁盘值——删除只清 bit7 不重算，此校验为最强自洽判据）。
//! 0x01..0x7F 皆可为"unused 槽"——绝不允许"见 <0x80 即跳过"（会静默丢失全部删除文件）。

use crate::ExfatError;

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

fn u16le(s: &[u8], o: usize) -> u16 { u16::from_le_bytes([s[o], s[o + 1]]) }
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
        if i == 2 || i == 3 { continue; }
        sum = (if sum & 1 != 0 { 0x8000 } else { 0 })
            .wrapping_add(sum >> 1)
            .wrapping_add(*b as u16);
    }
    sum
}

fn name_hash16(upcased_le: &[u8]) -> u16 { entry_set_checksum16(upcased_le) }

fn upcase_ascii(c: u16) -> u16 {
    if (0x61..=0x7A).contains(&c) { c - 0x20 } else { c }
}

/// 解析目录字节（`cluster_bytes` 用于拒绝跨簇项集）。`data` 为该目录所有簇按链序拼接。
pub fn parse_directory_bytes(data: &[u8], cluster_bytes: usize) -> Directory {
    let mut out = Directory::default();
    let mut i = 0usize;
    while i + 32 <= data.len() {
        let t = data[i];
        if t == 0x00 { break; } // 目录结束
        if t & 0x80 == 0 {
            // 未被 in-use 位标记：仅 0x05 型（删除的 File 项）才尝试组装
            if t & 0x7F == 0x05 {
                if let Some((e, slots)) = try_assemble_set(data, i, cluster_bytes, true) {
                    out.entries.push(e);
                    i += slots * 32;
                    continue;
                }
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
                out.specials.bitmaps.push((data[i + 1] & 0x01 != 0, u32le(data, i + 20), u64le(data, i + 24)));
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
fn try_assemble_set(data: &[u8], i: usize, cb: usize, deleted: bool) -> Option<(ParsedEntry, usize)> {
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
        let base = (1 + k) * 32;
        if restored[base] != 0xC1 {
            return None;
        }
        for u in 0..15 {
            if units.len() == name_len { break; }
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
        let up: Vec<u8> = units.iter().flat_map(|u| upcase_ascii(*u).to_le_bytes()).collect();
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
```

- [ ] **Step 4: 运行** → **13 passed**（累计 crate 35）
- [ ] **Step 5: Commit** `feat(fs-exfat): 目录项集解析（还原校验/多名字项/特殊项/跨簇拒绝）`

---

### Task 5: xd-fs-exfat —— 扫描与质量分级（scan.rs）

**Files:**
- Create: `crates/xd-fs-exfat/src/scan.rs`
- Modify: `crates/xd-fs-exfat/src/lib.rs`（+`pub mod scan;`）

**分级规则（brief §4.2，位图为准）**：`Complete` ⇔ 结构合法（已由 T4 门槛）∧ 簇全部在堆内 ∧ **位图可读且全空闲** ∧ (`contiguous` ∨ 链长足够)；位图不可读 → **永不给 Complete**；`backup_used` → 删除项封顶 `MaybeDamaged`。

- [ ] **Step 1: 测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot::testutil::{dev_for, patch_boot_both};

    const FAT_B: usize = 24 * 512;
    const ROOT_B: usize = 32 * 512 + 3 * 4096;
    const SET: usize = ROOT_B + 96;

    #[test]
    fn scans_live_and_deleted_with_full_names() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "A.TXT", b"hello")
            .add_file_chained("/", "G.BIN", &[9u8; 9000])
            .delete("/", "G.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let a = entries.iter().find(|e| e.name == "A.TXT").unwrap();
        assert!(!a.deleted && a.quality == RecoverQuality::Complete);
        let g = entries.iter().find(|e| e.name == "G.BIN").unwrap();
        assert!(g.deleted && g.size_bytes == 9000, "exFAT 删除名一字不差");
    }

    #[test]
    fn deleted_contiguous_is_complete() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::Complete, "NoFatChain 删除后仍连续（规范保证）+ 位图全空");
        assert!(e.contiguous);
    }

    #[test]
    fn deleted_chained_stale_fat_bitmap_clear_is_complete() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "G.BIN", &[9u8; 9000])
            .delete("/", "G.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::Complete, "位图全空（权威判据），stale FAT 不影响");
    }

    #[test]
    fn deleted_with_reused_cluster_is_maybe_damaged() {
        // 删除后被新文件复用簇（位图置位）→ 只能 MaybeDamaged
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "OLD.BIN", &[7u8; 9000])
            .delete("/", "OLD.BIN")
            .add_file_in_clusters("/", "NEW.BIN", &[5u8; 4500], &[6, 7], false)
            .build();
        let (_f, dev) = dev_for(&image);
        let old = scan(&dev).unwrap().into_iter().find(|e| e.name == "OLD.BIN").unwrap();
        assert_eq!(old.quality, RecoverQuality::MaybeDamaged);
    }

    #[test]
    fn unreadable_bitmap_never_complete() {
        // 把 0x81 的 FirstCluster 改成越界 → 位图不可读 → 删除项封顶 MaybeDamaged，scan 仍 Ok
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        let mut patched = image.clone();
        patched[ROOT_B + 32 + 20..ROOT_B + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::MaybeDamaged);
    }

    #[test]
    fn pick_bitmap_prefers_active_and_falls_back() {
        // 纯函数级：texFAT 双位图选择
        let both = vec![(false, 2, 32u64), (true, 40, 32u64)];
        assert_eq!(pick_bitmap(&both, 1), Some((40, 32)));
        assert_eq!(pick_bitmap(&both, 0), Some((2, 32)));
        let only_first = vec![(false, 2, 32u64)];
        assert_eq!(pick_bitmap(&only_first, 1), Some((2, 32)), "缺失活动位图时回退任一可用");
        assert_eq!(pick_bitmap(&[], 0), None);
    }

    #[test]
    fn texfat_fallback_still_scans() {
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        patch_boot_both(&mut image, &[(110, &2u8.to_le_bytes()), (106, &1u16.to_le_bytes())]);
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::Complete, "仅 id-0 位图存在 → 回退仍可用");
    }

    #[test]
    fn backup_geometry_caps_deleted_quality() {
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "PHOTO.JPG", &[1u8; 9000])
            .delete("/", "PHOTO.JPG")
            .build();
        image[200] ^= 0xFF; // 主区校验失败（备区仍有效）
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(e.quality, RecoverQuality::MaybeDamaged, "卷级降级：几何未验证");
    }

    #[test]
    fn root_unreadable_is_err() {
        let image = xd_fixtures::ExfatImageBuilder::new().add_file("/", "A.TXT", b"x").build();
        let truncated = image[..20_000].to_vec(); // 根区（28672 起）在设备外
        let (_f, dev) = dev_for(&truncated);
        assert!(matches!(scan(&dev), Err(ExfatError::InvalidBoot(m)) if m.contains("根目录")));
    }

    #[test]
    fn unreadable_subdir_marks_entry_damaged_and_continues() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_subdir("/", "DCIM")
            .add_file("/DCIM", "IMG.JPG", &[3u8; 100])
            .add_file("/", "ROOT.TXT", b"root")
            .build();
        let mut patched = image.clone();
        // DCIM 项集首簇（根槽 3 的 0xC0 偏移 20）改越界
        patched[SET + 32 + 20..SET + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let dir = entries.iter().find(|e| e.name == "DCIM").unwrap();
        assert_eq!(dir.quality, RecoverQuality::MaybeDamaged);
        assert!(entries.iter().any(|e| e.name == "ROOT.TXT"), "不得中止全盘");
        assert!(!entries.iter().any(|e| e.name == "IMG.JPG"));
    }

    #[test]
    fn deleted_dir_listed_not_recursed() {
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_subdir("/", "DCIM")
            .add_file("/DCIM", "IMG.JPG", &[3u8; 100])
            .build();
        let mut patched = image.clone();
        patched[SET] &= 0x7F;
        patched[SET + 32] &= 0x7F;
        patched[SET + 64] &= 0x7F;
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let dcim = entries.iter().find(|e| e.name == "DCIM").unwrap();
        assert!(dcim.deleted && dcim.is_dir, "is_dir 忠实属性");
        assert!(!entries.iter().any(|e| e.name == "IMG.JPG"), "不递归已删目录");
    }

    #[test]
    fn scans_multi_cluster_root_completely() {
        // 45 文件把根撑到 2 簇（含 0x01 补齐）：扫描必须读满整链、45 条全出——
        // 防"补齐回归成 0x00 导致解析器半途终止、静默丢后半根目录"（qual-t1 I3）
        let mut b = xd_fixtures::ExfatImageBuilder::new();
        for i in 0..45u32 {
            b.add_file("/", &format!("F{i:04}.TXT"), b"x");
        }
        let image = b.build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let files: Vec<&str> = entries.iter().filter(|e| !e.is_dir).map(|e| e.name.as_str()).collect();
        assert_eq!(files.len(), 45, "多簇根不得丢条目：{files:?}");
        assert!(files.contains(&"F0041.TXT"), "第二根簇的条目必须在列");
        assert!(files.contains(&"F0044.TXT"));
    }
}
```

- [ ] **Step 2: 运行确认失败** → **Step 3: 实现**

```rust
//! 快扫：根（恒 FAT 链）→ 递归存活子目录；分级以**位图**为分配权威。
//! 对等继承 M1a 语义：根不可读 → Err（区别于空盘）；is_dir 忠实；保守降级；MAX 兜底。

use crate::ExfatError;
use crate::bitmap::Bitmap;
use crate::boot::{self, ExfatBoot};
use crate::dirent::{self, ParsedEntry};
use crate::fattab::Fat32;
use xd_device::BlockDevice;

const MAX_ENTRIES: usize = 200_000;
const MAX_DEPTH: u32 = 32;
const MAX_DIR_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverQuality { Complete, MaybeDamaged }

#[derive(Debug, Clone)]
pub struct ExfatEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64, // = ValidDataLength（交付长度）
    pub data_length: u64,
    pub first_cluster: u32,
    pub deleted: bool,
    pub is_dir: bool,
    pub contiguous: bool,
    pub quality: RecoverQuality,
    pub ext: String,
}

/// texFAT 双位图选择：优先活动位图（bitmaps[k].0 == active），缺失则回退任一。
pub(crate) fn pick_bitmap(bitmaps: &[(bool, u32, u64)], active: u8) -> Option<(u32, u64)> {
    let want_second = active == 1;
    bitmaps
        .iter()
        .find(|(second, _, _)| *second == want_second)
        .or_else(|| bitmaps.first())
        .map(|(_, f, l)| (*f, *l))
}

pub fn scan(dev: &dyn BlockDevice) -> Result<Vec<ExfatEntry>, ExfatError> {
    let boot = boot::parse(dev)?;
    let fat = Fat32::new(dev, &boot);
    let mut out = Vec::new();
    let root_data = read_root_dir(dev, &boot, &fat)?;
    let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
    let bitmap = load_bitmap(dev, &boot, &fat);
    scan_parsed(dev, &boot, &fat, bitmap.as_ref(), &root.entries, "/", 0, &mut out)?;
    Ok(out)
}

/// 定位并加载分配位图（scan 与 read_file 共用）。任何失败 → None（分级层即降级，绝不回退 FAT）。
fn load_bitmap(dev: &dyn BlockDevice, boot: &ExfatBoot, fat: &Fat32) -> Option<Bitmap> {
    let root_data = read_root_dir(dev, boot, fat).ok()?;
    let root = dirent::parse_directory_bytes(&root_data, boot.cluster_bytes() as usize);
    let (first, len) = pick_bitmap(&root.specials.bitmaps, boot.active_fat_index())?;
    Bitmap::load(dev, boot, fat, first, len).ok()
}

/// 读根目录全部字节（根恒 FAT 链）。链失败或首读 0 字节 → Err（与空盘 Ok 区分，M1a I2 对等）。
fn read_root_dir(dev: &dyn BlockDevice, boot: &ExfatBoot, fat: &Fat32) -> Result<Vec<u8>, ExfatError> {
    let chain = fat
        .chain(boot.root_cluster)
        .map_err(|_| ExfatError::InvalidBoot("根目录不可读".into()))?;
    let cb = boot.cluster_bytes();
    let mut data = Vec::new();
    let mut buf = vec![0u8; cb as usize];
    let mut got_any = false;
    for c in chain {
        if data.len() as u64 > MAX_DIR_BYTES {
            break;
        }
        let n = match dev.read_at(boot.cluster_to_byte(c), &mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 { break; }
        got_any = true;
        data.extend_from_slice(&buf[..n]);
        if n < buf.len() { break; }
    }
    if !got_any {
        return Err(ExfatError::InvalidBoot("根目录不可读".into()));
    }
    Ok(data)
}

#[allow(clippy::too_many_arguments)]
fn scan_parsed(
    dev: &dyn BlockDevice,
    boot: &ExfatBoot,
    fat: &Fat32,
    bitmap: Option<&Bitmap>,
    entries: &[ParsedEntry],
    path: &str,
    depth: u32,
    out: &mut Vec<ExfatEntry>,
) -> Result<(), ExfatError> {
    if depth > MAX_DEPTH || out.len() > MAX_ENTRIES {
        return Ok(());
    }
    for e in entries {
        if out.len() >= MAX_ENTRIES {
            return Ok(());
        }
        let ext = e.name.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase()).unwrap_or_default();
        let quality = if e.is_dir {
            RecoverQuality::Complete
        } else if e.deleted {
            grade_deleted(boot, fat, bitmap, e)
        } else {
            RecoverQuality::Complete
        };
        out.push(ExfatEntry {
            name: e.name.clone(),
            path: path.to_string(),
            size_bytes: e.valid_data_length,
            data_length: e.data_length,
            first_cluster: e.first_cluster,
            deleted: e.deleted,
            is_dir: e.is_dir,
            contiguous: e.contiguous,
            quality,
            ext,
        });
        let pushed = out.len() - 1;
        if e.is_dir && !e.deleted && e.first_cluster >= 2 {
            let child_path = if path == "/" { format!("/{}", e.name) } else { format!("{path}/{}", e.name) };
            let child = read_subdir_bytes(dev, boot, fat, e);
            match child {
                Ok(data) => {
                    let d = dirent::parse_directory_bytes(&data, boot.cluster_bytes() as usize);
                    scan_parsed(dev, boot, fat, bitmap, &d.entries, &child_path, depth + 1, out)?;
                }
                Err(_) => {
                    out[pushed].quality = RecoverQuality::MaybeDamaged; // 项本身可读、内容不可枚举
                }
            }
        }
    }
    Ok(())
}

/// 子目录字节：contiguous → 连续读（规范保证）；否则先链、不足退连续；读不满即降级 Err。
fn read_subdir_bytes(
    dev: &dyn BlockDevice,
    boot: &ExfatBoot,
    fat: &Fat32,
    e: &ParsedEntry,
) -> Result<Vec<u8>, ExfatError> {
    if e.data_length == 0 || e.data_length > MAX_DIR_BYTES {
        return Err(ExfatError::InvalidBoot("目录 DataLength 非法".into()));
    }
    let cb = boot.cluster_bytes();
    let need = e.data_length.div_ceil(cb);
    let mut clusters: Vec<u32> = Vec::new();
    if !e.contiguous {
        if let Ok(chain) = fat.chain(e.first_cluster) {
            if chain.len() as u64 >= need {
                clusters = chain[..need as usize].to_vec();
            }
        }
    }
    if clusters.is_empty() {
        let max_cluster = boot.cluster_count as u64 + 1;
        let mut c = e.first_cluster as u64;
        while (clusters.len() as u64) < need && c <= max_cluster {
            clusters.push(c as u32);
            c += 1;
        }
        if (clusters.len() as u64) < need {
            return Err(ExfatError::InvalidBoot("目录簇越界".into()));
        }
    }
    let mut data = Vec::with_capacity(e.data_length as usize);
    let mut buf = vec![0u8; cb as usize];
    for c in clusters {
        let n = match dev.read_at(boot.cluster_to_byte(c), &mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 { break; }
        data.extend_from_slice(&buf[..n]);
        if n < buf.len() { break; }
    }
    if (data.len() as u64) < e.data_length {
        return Err(ExfatError::InvalidBoot("目录读不满".into()));
    }
    data.truncate(e.data_length as usize);
    Ok(data)
}

/// 删除项分级：簇定位（contiguous 规范保证 / 链优先）→ 界内 → 位图全空 → Complete。
fn grade_deleted(boot: &ExfatBoot, fat: &Fat32, bitmap: Option<&Bitmap>, e: &ParsedEntry) -> RecoverQuality {
    if boot.backup_used {
        return RecoverQuality::MaybeDamaged; // 卷级几何未验证 → 封顶
    }
    let Some(bitmap) = bitmap else {
        return RecoverQuality::MaybeDamaged; // 位图不可读 → 永不给 Complete
    };
    if e.data_length == 0 {
        return RecoverQuality::Complete;
    }
    if e.first_cluster < 2 {
        return RecoverQuality::MaybeDamaged;
    }
    let cb = boot.cluster_bytes();
    let need = e.data_length.div_ceil(cb);
    let max_cluster = boot.cluster_count as u64 + 1;
    let mut clusters: Vec<u32> = Vec::new();
    if !e.contiguous {
        if let Ok(chain) = fat.chain(e.first_cluster) {
            if chain.len() as u64 >= need {
                clusters = chain[..need as usize].to_vec();
            }
        }
    }
    if clusters.is_empty() {
        let mut c = e.first_cluster as u64;
        while (clusters.len() as u64) < need && c <= max_cluster {
            clusters.push(c as u32);
            c += 1;
        }
    }
    if (clusters.len() as u64) < need {
        return RecoverQuality::MaybeDamaged; // 越界截断：表项不可信
    }
    for c in clusters {
        match bitmap.is_free(c) {
            Ok(true) => {}
            _ => return RecoverQuality::MaybeDamaged, // 已占用/不可判：宁可漏报
        }
    }
    RecoverQuality::Complete
}
```

- [ ] **Step 4: 运行** → **12 passed**（累计 crate 47）
- [ ] **Step 5: Commit** `feat(fs-exfat): 扫描与质量分级（位图权威/texFAT 选表/根失败即 Err）`

---

### Task 6: xd-fs-exfat —— 文件读取（read_file，追加到 scan.rs）

**策略（对齐 M1a 判例 + brief §4.2）**：
- `size = ValidDataLength`（交付长度；`[VDL,DL)` **绝不交付**——那是未初始化区）；`need` 由 `DataLength` 定拓扑。
- `contiguous`（NoFatChain=1）→ 连续簇（**规范保证**，非猜测），u64 累积 + 同界截断（防野生 first_cluster 溢出，M1a I4 对等）。
- live 链式 → **只信链**，链短/坏 → 诚实短前缀（绝不连续猜读，M1a qual-t7 I1 对等）。
- 删除链式 → 链可用且簇空闲（位图为准）→ 用 stale 链；否则连续；再按位图**截断到首个被占用簇之前**（保守前缀）。
- 读循环：Err/0 → break；短读只收前缀；`truncate(size)`；`with_capacity(size.min(簇数×cb))`。

- [ ] **Step 1: 测试（追加到 scan.rs 测试模块）**

```rust
    #[test]
    fn reads_contiguous_file_exact() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new().add_file("/", "A.BIN", &data).build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.name == "A.BIN").unwrap();
        assert_eq!(read_file(&dev, &e).unwrap(), data);
    }

    #[test]
    fn reads_chained_fragmented_in_order() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_in_clusters("/", "F.BIN", &data, &[7, 6, 8], false)
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.name == "F.BIN").unwrap();
        assert_eq!(read_file(&dev, &e).unwrap(), data, "必须按链序 7→6→8 拼接");
    }

    #[test]
    fn deleted_contiguous_reads_exact() {
        let data: Vec<u8> = (0..12000u32).map(|i| (i % 253) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file("/", "G.BIN", &data)
            .delete("/", "G.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(read_file(&dev, &e).unwrap(), data);
    }

    #[test]
    fn deleted_chained_uses_stale_chain_when_bitmap_free() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 241) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "G.BIN", &data)
            .delete("/", "G.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
        assert_eq!(read_file(&dev, &e).unwrap(), data, "位图全空 → stale 链可用（仍是原数据）");
    }

    #[test]
    fn deleted_chained_occupied_middle_cluster_truncates_prefix() {
        // OLD.BIN 链式 3 簇（6,7,8）删除后，簇 7/8 被 NEW.BIN 复用 → 只交付簇 6 的前缀
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 239) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "OLD.BIN", &data)
            .delete("/", "OLD.BIN")
            .add_file_in_clusters("/", "NEW.BIN", &[5u8; 4500], &[7, 8], false)
            .build();
        let (_f, dev) = dev_for(&image);
        let old = scan(&dev).unwrap().into_iter().find(|e| e.name == "OLD.BIN").unwrap();
        assert_eq!(old.quality, RecoverQuality::MaybeDamaged);
        let bytes = read_file(&dev, &old).unwrap();
        assert_eq!(bytes.len(), 4096, "只交付首个空闲簇");
        assert_eq!(bytes, data[..4096]);
    }

    #[test]
    fn vdl_lt_dl_delivers_vdl_only() {
        // 磁盘上有 9000 字节真实数据，但 VDL=5000 → 交付绝不越过 VDL
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 233) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_with_vdl("/", "V.BIN", &data, 5000)
            .build();
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.name == "V.BIN").unwrap();
        assert_eq!(e.size_bytes, 5000);
        assert_eq!(e.data_length, 9000);
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes.len(), 5000, "严禁交付 [VDL,DL) 未初始化区");
        assert_eq!(bytes, data[..5000]);
    }

    #[test]
    fn live_broken_chain_returns_short_prefix_not_guess() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let mut image = xd_fixtures::ExfatImageBuilder::new()
            .add_file_chained("/", "A.BIN", &data)
            .build();
        image[FAT_B + 6 * 4..FAT_B + 6 * 4 + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // FAT[6]=EOC
        let (_f, dev) = dev_for(&image);
        let e = scan(&dev).unwrap().into_iter().find(|e| e.name == "A.BIN").unwrap();
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes.len(), 4096, "live 只信链：链只剩首簇 → 诚实短前缀");
        assert_eq!(bytes, data[..4096]);
    }

    #[test]
    fn wild_first_cluster_bounded_not_panic() {
        // 野生 first_cluster + 大 DL → u64 累积 + 同界截断；不得 panic、不得伪造
        let image = xd_fixtures::ExfatImageBuilder::new().add_file("/", "X.TXT", b"x").build();
        let (_f, dev) = dev_for(&image);
        let e = ExfatEntry {
            name: "WILD.BIN".into(), path: "/".into(),
            size_bytes: 1 << 20, data_length: 1 << 20,
            first_cluster: 0xFFFF_FE00, deleted: true, is_dir: false,
            contiguous: true, quality: RecoverQuality::MaybeDamaged, ext: "bin".into(),
        };
        let bytes = read_file(&dev, &e).unwrap();
        assert!(bytes.is_empty(), "界截断：确定性为空");
    }

    #[test]
    fn truncated_device_returns_read_prefix() {
        let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::ExfatImageBuilder::new().add_file("/", "A.BIN", &data).build();
        // 截断到簇 6 全 + 簇 7 前 200 字节（文件簇 6,7,8 起于 heap+4*4096）
        let cut = 32 * 512 + 4 * 4096 + 4096 + 200;
        let truncated = image[..cut].to_vec();
        let (_f, dev) = dev_for(&truncated);
        // 根目录在簇 5（完整）→ 可扫出条目
        let e = scan(&dev).unwrap().into_iter().find(|e| e.name == "A.BIN").unwrap();
        let bytes = read_file(&dev, &e).unwrap();
        assert_eq!(bytes.len(), 4096 + 200);
        assert_eq!(bytes, data[..4096 + 200]);
    }
```

- [ ] **Step 2: 运行确认失败** → **Step 3: 实现（追加到 scan.rs）**

```rust
/// 读取文件内容（交付 min(VDL,DL) = size_bytes 字节；设备边界/坏读早停 → 诚实短前缀）。
/// 注意：返回值只有字节——"是否走了连续假设"由 `entry.deleted` 推断（M1d UI 文案据此）。
pub fn read_file(dev: &dyn BlockDevice, entry: &ExfatEntry) -> Result<Vec<u8>, ExfatError> {
    if entry.size_bytes == 0 || entry.first_cluster < 2 {
        return Ok(Vec::new());
    }
    let boot = boot::parse(dev)?;
    let fat = Fat32::new(dev, &boot);
    let cb = boot.cluster_bytes();
    let size = entry.size_bytes as usize; // 目录项来源（parser 保证 VDL ≤ DL）
    let need = entry.data_length.div_ceil(cb);
    let max_cluster = boot.cluster_count as u64 + 1;

    let contiguous_range = |first: u32| -> Vec<u32> {
        let mut v = Vec::new();
        let mut c = first as u64;
        while (v.len() as u64) < need && c <= max_cluster {
            v.push(c as u32);
            c += 1;
        }
        v
    };

    let bitmap = if entry.deleted { load_bitmap(dev, &boot, &fat) } else { None };
    let mut clusters: Vec<u32> = if entry.contiguous {
        contiguous_range(entry.first_cluster) // 规范保证连续
    } else if entry.deleted {
        // 链可用且（位图不可读 ∨ 链簇全空）→ 用 stale 链；否则连续
        let chain = fat.chain(entry.first_cluster).unwrap_or_default();
        let chain_ok = chain.len() as u64 >= need;
        let chain_free = chain_ok
            && match &bitmap {
                Some(b) => chain.iter().all(|c| matches!(b.is_free(*c), Ok(true))),
                None => true,
            };
        if chain_ok && chain_free {
            chain[..need as usize].to_vec()
        } else {
            contiguous_range(entry.first_cluster)
        }
    } else {
        // live 链式：只信链（绝不连续猜读——坏 FAT 猜读会交付他人数据，M1a qual-t7 I1）
        let chain = fat.chain(entry.first_cluster).unwrap_or_default();
        chain[..(need as usize).min(chain.len())].to_vec()
    };
    // 删除项：按位图截断到首个被占用簇之前（保守前缀）
    if entry.deleted {
        if let Some(b) = &bitmap {
            let mut cut = clusters.len();
            for (k, c) in clusters.iter().enumerate() {
                if !matches!(b.is_free(*c), Ok(true)) {
                    cut = k;
                    break;
                }
            }
            clusters.truncate(cut);
        }
    }

    let mut out = Vec::with_capacity(size.min(clusters.len() * cb as usize));
    let mut buf = vec![0u8; cb as usize];
    for c in clusters {
        let n = match dev.read_at(boot.cluster_to_byte(c), &mut buf) {
            Ok(n) => n,
            Err(_) => break, // 坏读早停：保留已读前缀（与 scan 同规则）
        };
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() >= size || n < buf.len() {
            break;
        }
    }
    out.truncate(size);
    Ok(out)
}
```

- [ ] **Step 4: 运行** → **9 passed**（累计 crate 56；workspace 相应 +9）
- [ ] **Step 5: Commit** `feat(fs-exfat): 文件读取（连续/链/stale 链 + 位图截断前缀、VDL 交付）`

---

### Task 7: 端到端 —— exFAT 镜像 → 扫描 → 字节级找回 + 生成示例

**Files:**
- Create: `crates/xd-fs-exfat/tests/roundtrip.rs`、`crates/xd-fixtures/examples/gen_exfat_image.rs`

- [ ] **Step 1: e2e 测试**

```rust
//! M1a2 出口标准：exFAT 合成镜像落盘 → 扫描 → 删除照片字节级 + 全名找回
//! （「相机卡删照片」的 exFAT 版；对比 FAT 版的红利：名字一字不差）。

use xd_device::image::ImageFileDevice;

#[test]
fn deleted_photo_fully_recovered_from_exfat_image() {
    let photo: Vec<u8> = (0..12000u32).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let image_bytes = xd_fixtures::ExfatImageBuilder::new()
        .add_subdir("/", "DCIM")
        .add_file("/DCIM", "旅行照片_0001.JPG", &photo)
        .add_file("/", "READ_ME.TXT", b"keep me")
        .delete("/DCIM", "旅行照片_0001.JPG")
        .build();

    let mut f = tempfile::NamedTempFile::new().unwrap();
    use std::io::Write;
    f.write_all(&image_bytes).unwrap();
    f.flush().unwrap();
    let dev = ImageFileDevice::open(f.path()).unwrap();

    let entries = xd_fs_exfat::scan::scan(&dev).unwrap();
    let photo_entry = entries
        .iter()
        .find(|e| e.deleted && !e.is_dir && e.ext == "jpg")
        .expect("deleted jpg not found");
    assert_eq!(photo_entry.name, "旅行照片_0001.JPG", "exFAT 红利：删除后名字一字不差");
    assert_eq!(photo_entry.size_bytes, 12000);
    assert_eq!(photo_entry.path, "/DCIM");
    assert_eq!(entries.iter().filter(|e| e.deleted).count(), 1);

    let recovered = xd_fs_exfat::scan::read_file(&dev, photo_entry).unwrap();
    assert_eq!(recovered, photo, "recovered bytes differ from original");

    // 只读铁律：扫描/读取不得改动镜像一个字节
    assert_eq!(std::fs::read(f.path()).unwrap(), image_bytes, "扫描/读取不得改动镜像");

    assert!(entries.iter().any(|e| e.name == "READ_ME.TXT" && !e.deleted));
}

#[test]
fn deleted_chained_photo_recovered_byte_exact() {
    let photo: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
    let image_bytes = xd_fixtures::ExfatImageBuilder::new()
        .add_file_chained("/", "BURST.BIN", &photo)
        .delete("/", "BURST.BIN")
        .build();

    let mut f = tempfile::NamedTempFile::new().unwrap();
    use std::io::Write;
    f.write_all(&image_bytes).unwrap();
    f.flush().unwrap();
    let dev = ImageFileDevice::open(f.path()).unwrap();

    let e = xd_fs_exfat::scan::scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.deleted)
        .expect("deleted file not found");
    assert_eq!(xd_fs_exfat::scan::read_file(&dev, &e).unwrap(), photo);
}
```

- [ ] **Step 2: 运行** → crate **58 passed**（56 + 2）

- [ ] **Step 3: 生成示例**

```rust
// 生成一张含"已删除照片"的 exFAT 镜像（1 MiB）：
//   cargo run -p xd-fixtures --example gen_exfat_image -- /tmp/xd-exfat.img
fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/xd-exfat.img".into());
    let photo: Vec<u8> = (0..65_536u32).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_subdir("/", "DCIM")
        .add_file("/DCIM", "IMG_0001.JPG", &photo)
        .delete("/DCIM", "IMG_0001.JPG")
        .build();
    std::fs::write(&path, &image).unwrap();
    println!("wrote {} ({} bytes)", path, image.len());
}
```

Run: `cargo run -p xd-fixtures --example gen_exfat_image -- /tmp/xd-exfat.img`
Expected: `wrote /tmp/xd-exfat.img (1048576 bytes)`

- [ ] **Step 4: 本地 oracle（非 CI 依赖）**：`fsck.exfat -n /tmp/xd-exfat.img` → 预期 `clean`（exfatprogs 为参考实现；CI 环境若无该工具则跳过，不作为门禁）

- [ ] **Step 5: Commit** `test(fs-exfat): 端到端字节级 + 全名找回 + 镜像生成示例`

---

### Task 8: M1a2 出口验收

- [ ] `cargo test --workspace --locked` 全绿（预期 +57 crate 测试）；clippy `-D warnings` 零告警；fmt 干净
- [ ] e2e 两案通过（删除 JPG 全名 + 字节级；链式删除逐字节）
- [ ] `cargo run -p xd-fixtures --example gen_exfat_image` 产物 = 1,048,576 字节；`fsck.exfat -n` 判 clean（本地验证）
- [ ] `bash scripts/apply-copyright.sh` 幂等（新 .rs 文件获版权头；`exfat_upcase.bin` 为资产不加头）
- [ ] 计划同步：执行中的偏离/修复轮按 M1a 惯例记录于本节
- [ ] `provenance.sha256` 重生成——待发布时执行

## 后续切片（各自独立计划）

- **M1b**：契约 v1（`scan.start`/`scan.progress`/`scan.results`）+ daemon 并发 + 状态机 + xd-core 编排；
  **并入 scan.rs 拆分欠账**（M1a 移交注 11：`read_file` → `read.rs`）与 `RecoverQuality` 文档补全（注 12）
- **M1c**：`xd-carving`（JPEG/PNG 签名雕刻 v1；碎片化删除的答案——见 M1a 移交注 16 的恢复率边界）
- **M1d**：UI 三页 + 恢复导出（文案：删除项=连续假设恢复；exFAT 全名红利；VDL 未初始化区标注）
- **M1e**：设备枚举/提权/打包（调研简报 `/tmp/xd-m1e-brief.md` 已就绪：sysfs/uaccess/pkexec/deb/losetup e2e）

---

© 2026 erik · https://erik.xyz · erik@erik.xyz
