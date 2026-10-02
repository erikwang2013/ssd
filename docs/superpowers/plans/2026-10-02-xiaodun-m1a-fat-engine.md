<!-- © 2026 erik · https://erik.xyz · erik@erik.xyz -->

# 小盾 M1a 实施计划：FAT/exFAT 引擎地基（xd-fs-fat + 合成镜像工具）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 `xd-fs-fat` 引擎（FAT12/16/32 快速扫描 + 删除文件找回 + 字节级读取）与 `xd-fixtures` 合成镜像构建器——「镜像里删掉的文件 → 扫出来 → 原样读回」在测试中字节级成立。

**Architecture:** `xd-fixtures` 纯内存构建合法 FAT 镜像（BPB/FAT 表/目录项严格按规范布局，删除=首字节 0xE5 + FAT 链释放、后续分配优先复用释放簇——模拟真实删除后覆盖）。`xd-fs-fat` 只依赖 `xd-device::BlockDevice`（任意偏移只读），零平台耦合：BPB 解析（结构判定 FAT32）→ FAT 表访问（FAT12 半字节寻址/16/32 掩码）→ 目录项解析（SFN 0x05 quirk、小写标志、LFN 组含被删孤儿）→ 递归扫描 + 质量分级 → 链读/连续回退读文件。引擎自持结果类型，M1b 的协议层再做映射（引擎零依赖 xd-core，避免循环依赖）。

**Tech Stack:** Rust（edition 2024）、仅 workspace 既有依赖（serde/serde_json/tempfile），无新增第三方 crate。

**边界（本计划明确不做，各自有后续计划）：** exFAT（独立格式解析，M1a2）；雕刻 carving（M1c）；契约 v1/daemon 并发/任务状态机（M1b）；UI 三页（M1d）；设备枚举/提权/打包（M1e）。**FAT32 判定采用"结构优先"**（`root_entry_count==0 && fat16_size==0 && root_cluster>=2` → FAT32；否则按簇数 4085/65525 分 12/16）——与 fatfs 等主流实现一致，使小型 FAT32 测试镜像可行，Microsoft 规范的纯簇数判定仅在病态镜像上不同。

**依赖关系：** T1 → T2 → {T3→T4→T5→T6→T7} → T8。T3 起与 T2 无耦合后可并行，但按单写者规范串行执行。

---

## File Structure（M1a 结束时）

```
Cargo.toml                          # + members: crates/xd-fixtures, crates/xd-fs-fat
crates/xd-fixtures/
  Cargo.toml                        # publish = false
  src/lib.rs                        # FatImageBuilder（BPB/FAT/目录/文件布局/删除/覆盖复用）
crates/xd-fs-fat/
  Cargo.toml                        # 依赖 xd-device；dev-dep xd-fixtures + tempfile
  src/lib.rs                        # pub mod bpb/fat/dirent/scan; 错误类型 FatError
  src/bpb.rs                        # 引导扇区解析 + 几何计算 + FAT 类型判定
  src/fat.rs                        # FAT 表访问：entry/chain/is_free（12/16/32）
  src/dirent.rs                     # 目录项解析：SFN/LFN/槽分类/名称组装
  src/scan.rs                       # scan()/read_file()/FatEntry/RecoverQuality
  tests/roundtrip.rs                # 端到端：镜像 → 扫描 → 读回字节
fixtures/gen_fat_image.rs           # 由 builder 输出镜像文件的示例（供后续 e2e/手工用）
```

---

### Task 1: xd-fixtures —— FAT16 合成镜像构建器

**Files:**
- Modify: `Cargo.toml`（members + 空行分隔）
- Create: `crates/xd-fixtures/Cargo.toml`、`crates/xd-fixtures/src/lib.rs`

- [ ] **Step 1: 挂入 workspace**

`Cargo.toml` members 追加两行（本任务只需 xd-fixtures，xd-fs-fat 在 Task 3 再加）：

```toml
members = [
    "crates/xd-core",
    "crates/xd-device",
    "crates/xd-daemon",
    "crates/xd-ffi",
    "crates/xd-fixtures",
]
```

`crates/xd-fixtures/Cargo.toml`：

```toml
[package]
name = "xd-fixtures"
version.workspace = true
edition.workspace = true
publish = false
```

- [ ] **Step 2: 写失败的测试（`crates/xd-fixtures/src/lib.rs` 末尾测试模块）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fat16_layout_bytes_are_sane() {
        let image = FatImageBuilder::fat16().add_file("/", "HELLO.TXT", b"hello world").build();
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
        let de = image.windows(32).position(|w| &w[..11] == b"HELLO   TXT").unwrap();
        assert_eq!(image[de + 11], 0x20);
    }

    #[test]
    fn deleted_file_has_0xe5_and_freed_fat() {
        let image = FatImageBuilder::fat16()
            .add_file("/", "A.BIN", &[1u8; 1000])
            .delete("/", "A.BIN")
            .build();
        let de = image.windows(32).position(|w| w[0] == 0xE5 && &w[8..11] == b"BIN").unwrap();
        assert_eq!(image[de], 0xE5);
        // FAT 里原文件两簇已被释放（entry(2)=0, entry(3)=0）
        let fat_start = 512usize; // reserved=1 → FAT 在第 2 个扇区
        assert_eq!(u16::from_le_bytes([image[fat_start + 4], image[fat_start + 5]]), 0);
        assert_eq!(u16::from_le_bytes([image[fat_start + 6], image[fat_start + 7]]), 0);
    }

    #[test]
    fn reuse_of_freed_clusters_overlaps() {
        let image = FatImageBuilder::fat16()
            .add_file("/", "OLD.BIN", &[7u8; 1024]) // 簇 2..3
            .delete("/", "OLD.BIN")
            .add_file("/", "NEW.BIN", &[9u8; 1024]) // 复用 2..3
            .build();
        let old = image.windows(32).position(|w| &w[1..5] == b"LD  ").unwrap();
        assert_eq!(image[old], 0xE5);
        // 第一个数据簇现在属于 NEW.BIN：NEW 目录项 first_cluster == 2
        let new = image.windows(32).position(|w| &w[..7] == b"NEW    ").unwrap();
        assert_eq!(u16::from_le_bytes([image[new + 26], image[new + 27]]), 2);
    }

    #[test]
    fn empty_file_builds_without_panic() {
        let image = FatImageBuilder::fat16().add_file("/", "EMPTY.TXT", b"").build();
        let de = image.windows(32).position(|w| w[0] == b'E' && &w[8..11] == b"TXT").unwrap();
        assert_eq!(u32::from_le_bytes([image[de + 28], image[de + 29], image[de + 30], image[de + 31]]), 0);
        assert_eq!(u16::from_le_bytes([image[de + 26], image[de + 27]]), 2); // 仍占 1 簇
    }

    #[test]
    fn deletion_timing_respected() {
        let img = FatImageBuilder::fat16()
            .add_file("/", "IMG.JPG", &[7u8; 500])   // 簇 2
            .add_file("/", "READ.TXT", b"keep me")   // 簇 3
            .delete("/", "IMG.JPG")                  // 删除发生在两个 add 之后
            .build();
        // 照片字节仍在簇 2（未被后续文件覆盖）；数据区首字节 = 50*512
        let data_start = 50usize * 512;
        assert_eq!(&img[data_start..data_start + 4], &[7u8; 4]);
        // READ.TXT 在簇 3
        let rd = img.windows(32).position(|w| &w[..8] == b"READ    ").unwrap();
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
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p xd-fixtures`
Expected: 编译失败（`FatImageBuilder` 未定义）。

- [ ] **Step 4: 实现 FAT16 构建器**

```rust
//! 合成 FAT 镜像构建器：测试与 e2e 的全部输入来源（脱开真实硬件）。
//! 参数固定（T1 只支持 FAT16；T2 扩展 FAT12/32）：bps=512、spc=1、reserved=1、
//! fats=1、root_entries=512、fat_size=17 扇区、total=4224 扇区（≈2.1 MiB）。
//! 4174 数据簇 ≥ 4085 → 按微软簇数规则也是真 FAT16（避免真实驱动判为 FAT12）。
//!
//! 契约（测试作者必读）：
//! 1. 写入历史按调用顺序模拟：delete() 的时刻记为 files.len()，该文件的簇在
//!    「下一次 add 时」才被释放 → 只有删除**之后**添加的文件才可能复用其簇。
//! 2. M1a 恢复按「连续簇假设」：碎裂的已删文件（其后有新文件绕过空洞占其邻簇）
//!    连续回退读可能读到他人字节——刻意的夹具边界，勿在 T6/T8 构造该形状。
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
pub const TOTAL_SECTORS: u32 = 4224;

#[derive(Clone)]
struct BuildFile {
    dir: String,          // "/" 或 "/SUB"（T1 仅 "/"）
    name: [u8; 11],       // 8.3 原始名（大写、空格填充）
    data: Vec<u8>,
    /// Some(时刻)：在该时刻（= delete() 调用时的 files.len()）释放其簇；None = 存活。
    /// 按真实时序释放，保证「先建后删」的文件各自可恢复。
    deleted_at: Option<usize>,
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
        // 布局（扇区）：
        //  0        reserved / 引导扇区
        //  1..18    FAT#1（17 扇区）
        //  18..50   根目录（512 项 × 32B = 32 扇区）
        //  50..    数据区（簇 N → 扇区 50 + (N-2)，共 4174 簇）
        const FAT_START: u32 = 1;
        const FAT_SIZE: u32 = 17;
        const ROOT_START: u32 = FAT_START + FAT_SIZE; // 18
        const ROOT_SECTORS: u32 = 32;
        const DATA_START: u32 = ROOT_START + ROOT_SECTORS; // 50
        const MAX_CLUSTER: u32 = 2 + (TOTAL_SECTORS - DATA_START); // spc=1

        let mut image = vec![0u8; (TOTAL_SECTORS * BPS) as usize];

        // FAT[0]=媒体描述符、FAT[1]=EOC（合规要求）
        let fat0 = (FAT_START * BPS) as usize;
        image[fat0..fat0 + 2].copy_from_slice(&0xFFF8u16.to_le_bytes());
        image[fat0 + 2..fat0 + 4].copy_from_slice(&0xFFFFu16.to_le_bytes());

        // ---- 分配簇（最低空闲优先；删除按真实时序释放：在分配文件 i 前，
        // 先释放所有 deleted_at == Some(i) 的文件簇 → 只有删除之后的文件才会复用）----
        let mut next_free: Vec<u32> = (2..MAX_CLUSTER).collect();
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
                // 存活文件写 FAT 链（末簇 EOC=0xFFFF）；删除文件不写链（已释放）
                for (j, &c) in take.iter().enumerate() {
                    let entry_off = (FAT_START * BPS + c * 2) as usize;
                    let value: u16 = if j + 1 == take.len() { 0xFFFF } else { take[j + 1] as u16 };
                    image[entry_off..entry_off + 2].copy_from_slice(&value.to_le_bytes());
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
                let start = (DATA_START * BPS + (c - 2) * BPS) as usize;
                image[start..start + (end - off)].copy_from_slice(&data[off..end]);
            }
        }

        // ---- 根目录项（32B 槽；删除项首字节 0xE5）----
        let mut slot = ROOT_START * BPS;
        for (clusters, data, idx) in &placed {
            let f = &self.files[*idx];
            let mut entry = [0u8; 32];
            entry[..11].copy_from_slice(&f.name);
            if f.deleted_at.is_some() {
                entry[0] = 0xE5;
            }
            entry[11] = 0x20; // ATTR_ARCHIVE
            entry[26..28].copy_from_slice(&(clusters[0] as u16).to_le_bytes());
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
        bs[28..32].copy_from_slice(&DATA_START.to_le_bytes()); // hidden sectors（夹具惯例值）
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
    debug_assert!(
        name.is_ascii() && base.len() <= 8 && ext.len() <= 3,
        "encode_sfn 仅支持 ASCII ≤8.3 名: {name}"
    );
    let mut out = [b' '; 11];
    for (i, c) in base.bytes().take(8).enumerate() {
        out[i] = c.to_ascii_uppercase();
    }
    for (i, c) in ext.bytes().take(3).enumerate() {
        out[8 + i] = c.to_ascii_uppercase();
    }
    out
}
```

（`placed` 的 `count` 与 `idx` 字段在 T1 未全用到是允许的；`let _ = count;` 防未用告警。）

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p xd-fixtures`
Expected: 6 passed。

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/xd-fixtures
git commit -m "feat(fixtures): FAT16 合成镜像构建器（分配/删除/簇复用）"
```

---

### Task 2: xd-fixtures —— FAT12 / FAT32 / 子目录支持

**Files:**
- Modify: `crates/xd-fixtures/src/lib.rs`

- [ ] **Step 1: 写失败的测试（追加到测试模块）**

```rust
    #[test]
    fn fat32_structural_fields() {
        let image = FatImageBuilder::fat32().add_file("/", "A.TXT", b"abc").build();
        assert_eq!(u16::from_le_bytes([image[17], image[18]]), 0); // root_entries = 0
        assert_eq!(u16::from_le_bytes([image[22], image[23]]), 0); // fat16_size = 0
        assert_eq!(u32::from_le_bytes([image[36], image[37], image[38], image[39]]), 64); // fat32_size
        assert_eq!(u32::from_le_bytes([image[44], image[45], image[46], image[47]]), 2); // root_cluster
        // 根簇 EOC 与根目录内容确实在簇 2（reserved=32 → FAT[2] 在 32*512+8）
        let fat = 32usize * 512;
        assert_eq!(
            u32::from_le_bytes([image[fat + 8], image[fat + 9], image[fat + 10], image[fat + 11]]) & 0x0FFF_FFFF,
            0x0FFF_FFFF
        );
        let de = 96 * 512; // root_start(=data_start)=96
        assert_eq!(&image[de..de + 11], b"A       TXT");
        assert_eq!(u16::from_le_bytes([image[de + 26], image[de + 27]]), 3);
        assert_eq!(&image[97 * 512..97 * 512 + 3], b"abc");
    }

    #[test]
    fn fat12_boot_sector_and_packing() {
        let image = FatImageBuilder::fat12().add_file("/", "A.BIN", &[5u8; 600]).build();
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
        // 子目录内容区逐槽字节级绑定（"." 自指、".." 指父=0、IN.TXT 指簇 3）
        let dir_cluster = u16::from_le_bytes([image[de + 26], image[de + 27]]) as usize;
        let base = (50 + (dir_cluster - 2)) * 512;
        assert_eq!(&image[base..base + 11], b".          ");
        assert_eq!(u16::from_le_bytes([image[base + 26], image[base + 27]]) as usize, dir_cluster);
        assert_eq!(&image[base + 32..base + 43], b"..         ");
        assert_eq!(u16::from_le_bytes([image[base + 58], image[base + 59]]), 0);
        assert_eq!(&image[base + 64..base + 75], b"IN      TXT");
        assert_eq!(u16::from_le_bytes([image[base + 90], image[base + 91]]), 3);
        assert_eq!(&image[(50 + (3 - 2)) * 512..][..5], b"inner");
    }

    #[test]
    #[should_panic(expected = "根目录单簇容量不足")]
    fn fat32_root_capacity_fails_fast() {
        let mut b = FatImageBuilder::fat32();
        for i in 0..17 {
            b.add_file("/", &format!("F{i:02}.TXT"), b"x");
        }
        b.build();
    }

    #[test]
    #[should_panic(expected = "成员超出单簇容量")]
    fn subdir_capacity_fails_fast() {
        let mut b = FatImageBuilder::fat16();
        b.add_subdir("/", "DIR");
        for i in 0..15 {
            b.add_file("/DIR", &format!("M{i:02}.TXT"), b"x");
        }
        b.build();
    }

    #[test]
    #[should_panic(expected = "簇池不足")]
    fn pool_exhaustion_fails_fast() {
        let data = vec![0u8; 1007 * 512]; // FAT12 池 1006 簇，差一簇
        FatImageBuilder::fat12().add_file("/", "BIG.BIN", &data).build();
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xd-fixtures`
Expected: 编译失败（`fat32`/`fat12`/`add_subdir` 未定义）。

- [ ] **Step 3: 实现三类型布局 + 子目录**

将 `FatImageBuilder` 重构为带布局参数的版本（替换 Task 1 的同名项，测试模块保留）：

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FatType { Fat12, Fat16, Fat32 }

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

pub struct FatImageBuilder {
    fat_type: FatType,
    files: Vec<BuildFile>,
    subdirs: Vec<String>, // 形如 "DIR"
}

impl FatImageBuilder {
    pub fn fat12() -> Self { Self { fat_type: FatType::Fat12, files: Vec::new(), subdirs: Vec::new() } }
    pub fn fat16() -> Self { Self { fat_type: FatType::Fat16, files: Vec::new(), subdirs: Vec::new() } }
    pub fn fat32() -> Self { Self { fat_type: FatType::Fat32, files: Vec::new(), subdirs: Vec::new() } }

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
        self.files.push(BuildFile { dir: dir.to_string(), name: encode_sfn(name), data: data.to_vec(), deleted: false, first_cluster: 0 });
        self
    }

    pub fn delete(&mut self, dir: &str, name: &str) -> &mut Self {
        let target = encode_sfn(name);
        let f = self.files.iter_mut().find(|f| !f.deleted && f.dir == dir && f.name == target).expect("delete: file not found");
        f.deleted = true;
        self
    }
```

`build()` 改为通用算法（替换原实现；注释保留布局推导）：

```rust
    pub fn build(&self) -> Vec<u8> {
        let (root_entries, fat_size, reserved, root_cluster, total_sectors) = self.fat_type.layout();
        let root_sectors = ((root_entries as u32) * 32).div_ceil(BPS);
        let fat_start = reserved;
        let root_start = fat_start + fat_size;                     // FAT12/16 固定根目录区起点
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
        let root_cluster_actual = if root_cluster >= 2 { next_free.remove(0) } else { 0 };
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

        // 分配文件簇（最低空闲优先；删除按真实时序释放：分配文件 i 前先释放
        // deleted_at == Some(i) 的文件簇 → 只有删除之后的文件才会复用）
        let mut placed: Vec<(Vec<u32>, Vec<u8>, usize)> = Vec::new();
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
                for (j, &c) in take.iter().enumerate() {
                    if self.fat_type == FatType::Fat12 {
                        let value: u32 = if j + 1 == take.len() { 0xFFF } else { take[j + 1] };
                        set_fat12(&mut image, fat_start, c, value);
                    } else {
                        let width = if self.fat_type == FatType::Fat32 { 4 } else { 2 };
                        let off = (fat_start * BPS + c * width) as usize;
                        let value: u32 = if j + 1 == take.len() {
                            if width == 4 { 0x0FFF_FFFF } else { 0xFFFF }
                        } else { take[j + 1] };
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

        // 写文件数据（按 FAT 链逐簇落盘：释放簇复用可能碎裂，线性写会污染相邻文件）
        for (clusters, data, _) in &placed {
            for (j, &c) in clusters.iter().enumerate() {
                let off = j * BPS as usize;
                if off >= data.len() {
                    break;
                }
                let end = (off + BPS as usize).min(data.len());
                let start = (data_start * BPS + (c - 2) * BPS) as usize;
                image[start..start + (end - off)].copy_from_slice(&data[off..end]);
            }
        }

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

        // 根目录槽
        let mut slot = (root_start * BPS) as usize;
        for (dir, cluster) in &dir_clusters {
            let mut e = [0u8; 32];
            e[..11].copy_from_slice(&encode_sfn(dir));
            e[11] = 0x10;
            e[26..28].copy_from_slice(&(*cluster as u16).to_le_bytes());
            image[slot..slot + 32].copy_from_slice(&e);
            slot += 32;
        }
        for (clusters, data, idx) in &placed {
            if self.files[*idx].dir != "/" { continue; }
            let mut e = [0u8; 32];
            e[..11].copy_from_slice(&self.files[*idx].name);
            if self.files[*idx].deleted_at.is_some() { e[0] = 0xE5; }
            e[11] = 0x20;
            e[26..28].copy_from_slice(&(clusters[0] as u16).to_le_bytes());
            e[28..32].copy_from_slice(&(data.len() as u32).to_le_bytes());
            image[slot..slot + 32].copy_from_slice(&e);
            slot += 32;
        }

        // 子目录内容区（每个一簇）："." ".." + 成员项
        for (idx, (dir, cluster)) in dir_clusters.iter().enumerate() {
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
            let parent_cluster = if root_cluster_actual >= 2 { root_cluster_actual } else { 0 };
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
                if self.files[*fi].dir != format!("/{dir}") { continue; }
                let mut e = [0u8; 32];
                e[..11].copy_from_slice(&self.files[*fi].name);
                if self.files[*fi].deleted_at.is_some() { e[0] = 0xE5; }
                e[11] = 0x20;
                e[26..28].copy_from_slice(&(clusters[0] as u16).to_le_bytes());
                e[28..32].copy_from_slice(&(data.len() as u32).to_le_bytes());
                image[off..off + 32].copy_from_slice(&e);
                off += 32;
            }
            let _ = idx;
        }

        // 引导扇区（按类型）
        let mut bs = [0u8; 512];
        bs[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
        bs[3..11].copy_from_slice(b"MSDOS5.0");
        bs[11..13].copy_from_slice(&(BPS as u16).to_le_bytes());
        bs[13] = 1;
        bs[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
        bs[16] = 1;
        bs[17..19].copy_from_slice(&root_entries.to_le_bytes());
        bs[19..21].copy_from_slice(&0u16.to_le_bytes());
        bs[21] = 0xF8;
        bs[22..24].copy_from_slice(&(if root_entries > 0 { fat_size as u16 } else { 0 }).to_le_bytes());
        bs[24..26].copy_from_slice(&63u16.to_le_bytes());
        bs[26..28].copy_from_slice(&255u16.to_le_bytes());
        bs[28..32].copy_from_slice(&(data_start as u32).to_le_bytes());
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
```

```rust
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
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p xd-fixtures`
Expected: 12 passed（T1 6 + T2 3 + 收尾加固 3，见修订轮）。

- [ ] **Step 5: Commit**

```bash
git add crates/xd-fixtures
git commit -m "feat(fixtures): FAT12/32 布局与子目录支持"
```

---

### Task 3: xd-fs-fat —— 引导扇区解析（BPB）

**Files:**
- Modify: `Cargo.toml`（members + `crates/xd-fs-fat`）
- Create: `crates/xd-fs-fat/Cargo.toml`、`crates/xd-fs-fat/src/lib.rs`、`crates/xd-fs-fat/src/bpb.rs`

- [ ] **Step 1: 创建 crate 骨架**

`Cargo.toml` members 追加 `"crates/xd-fs-fat",`。

`crates/xd-fs-fat/Cargo.toml`：

```toml
[package]
name = "xd-fs-fat"
version.workspace = true
edition.workspace = true

[dependencies]
xd-device = { path = "../xd-device" }

[dev-dependencies]
xd-fixtures = { path = "../xd-fixtures" }
tempfile = { workspace = true }
```

`crates/xd-fs-fat/src/lib.rs`：

```rust
//! FAT12/16/32 只读解析：快速扫描（删除文件找回）与文件读取。
//! 全部输入经 `xd_device::BlockDevice`（任意偏移只读），零平台耦合。

pub mod bpb;
pub mod fat;
pub mod dirent;
pub mod scan;

/// 引擎统一错误类型。
#[derive(Debug)]
#[non_exhaustive]
pub enum FatError {
    Device(xd_device::DeviceError),
    InvalidBpb(String),
}

impl std::fmt::Display for FatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FatError::Device(e) => write!(f, "device error: {e}"),
            FatError::InvalidBpb(m) => write!(f, "invalid fat bpb: {m}"),
        }
    }
}

impl std::error::Error for FatError {}

impl From<xd_device::DeviceError> for FatError {
    fn from(e: xd_device::DeviceError) -> Self {
        FatError::Device(e)
    }
}
```

（`fat`/`dirent`/`scan` 模块先在 Step 3 建空文件 `// T4/T5/T6 填充` 以便编译；或本步只声明 `pub mod bpb;`，后续任务逐步加行——**采用后者**，避免空文件。）

- [ ] **Step 2: 写失败的测试（`crates/xd-fs-fat/src/bpb.rs` 末尾）**

```rust
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
        let image = xd_fixtures::FatImageBuilder::fat16().add_file("/", "A.TXT", b"x").build();
        let (_f, dev) = device_with(&image);
        let bpb = parse(&dev).unwrap();
        assert_eq!(bpb.fat_type, FatType::Fat16);
        assert_eq!(bpb.bytes_per_sector, 512);
        assert_eq!(bpb.root_entry_count, 512);
        assert_eq!(bpb.root_cluster, 0);
        // data_start = 1 + 1*4 + 32 = 37 扇区
        assert_eq!(bpb.data_start_sector, 50);
        assert_eq!(bpb.total_sectors, 4224);
    }

    #[test]
    fn parses_fat32_structurally() {
        let image = xd_fixtures::FatImageBuilder::fat32().add_file("/", "A.TXT", b"x").build();
        let (_f, dev) = device_with(&image);
        let bpb = parse(&dev).unwrap();
        assert_eq!(bpb.fat_type, FatType::Fat32);
        assert_eq!(bpb.root_entry_count, 0);
        assert_eq!(bpb.root_cluster, 2);
        // reserved=32, fats=1, fat_size=64 → data_start = 32 + 64 = 96
        assert_eq!(bpb.data_start_sector, 96);
    }

    #[test]
    fn parses_fat12_by_cluster_count() {
        let image = xd_fixtures::FatImageBuilder::fat12().add_file("/", "A.TXT", b"x").build();
        let (_f, dev) = device_with(&image);
        let bpb = parse(&dev).unwrap();
        assert_eq!(bpb.fat_type, FatType::Fat12);
        assert_eq!(bpb.fat_start_sector, 1);
        assert_eq!(bpb.root_start_sector, 4);
        assert_eq!(bpb.data_start_sector, 18);
        assert_eq!(bpb.data_cluster_count(), 1006);
    }

    #[test]
    fn rejects_bad_magic() {
        let image = xd_fixtures::FatImageBuilder::fat16().build();
        let mut bad = image.clone();
        bad[510] = 0;
        let (_f, dev) = device_with(&bad);
        assert!(matches!(
            parse(&dev),
            Err(FatError::InvalidBpb(m)) if m == "missing 0x55AA boot signature"
        ));
    }

    #[test]
    fn rejects_bad_bytes_per_sector() {
        let image = xd_fixtures::FatImageBuilder::fat16().build();
        let mut bad = image.clone();
        bad[11] = 0;
        bad[12] = 1; // 256，非法
        let (_f, dev) = device_with(&bad);
        assert!(matches!(
            parse(&dev),
            Err(FatError::InvalidBpb(m)) if m == "bad bytes per sector: 256"
        ));
    }

    #[test]
    fn rejects_bad_sectors_per_cluster() {
        let image = xd_fixtures::FatImageBuilder::fat16().build();
        let mut bad = image.clone();
        bad[13] = 3;
        let (_f, dev) = device_with(&bad);
        assert!(matches!(
            parse(&dev),
            Err(FatError::InvalidBpb(m)) if m == "bad sectors per cluster: 3"
        ));
    }

    #[test]
    fn rejects_zero_fat_count() {
        let image = xd_fixtures::FatImageBuilder::fat16().build();
        let mut bad = image.clone();
        bad[16] = 0;
        let (_f, dev) = device_with(&bad);
        assert!(matches!(
            parse(&dev),
            Err(FatError::InvalidBpb(m)) if m == "zero fat count"
        ));
    }

    #[test]
    fn rejects_fat32_without_root_cluster() {
        let image = xd_fixtures::FatImageBuilder::fat32().build();
        let mut bad = image.clone();
        bad[44..48].copy_from_slice(&1u32.to_le_bytes());
        let (_f, dev) = device_with(&bad);
        assert!(matches!(
            parse(&dev),
            Err(FatError::InvalidBpb(m)) if m == "fat32 without root cluster"
        ));
    }

    #[test]
    fn rejects_tiny_image() {
        let (_f, dev) = device_with(&[0u8; 100]);
        assert!(matches!(
            parse(&dev),
            Err(FatError::InvalidBpb(m)) if m == "image smaller than 512 bytes"
        ));
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
```

- [ ] **Step 3: 实现 `crates/xd-fs-fat/src/bpb.rs`**

```rust
//! 引导扇区（BPB）解析与几何计算。
//! FAT32 判定：结构优先（root_entry_count == 0 && fat16_size == 0 && root_cluster >= 2），
//! 否则按数据区簇数 4085/65525 区分 FAT12/16——与 fatfs 等主流实现一致。

use crate::FatError;
use xd_device::BlockDevice;

// M1a 刻意接受、M2 处理真实损坏盘时重估：reserved_sectors==0；12/16 的 root_entry_count==0；
// num_fats>2；total16/total32 同时非零时取 t16；total_sectors 不比对设备实际长度。
pub const SECTOR0_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
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

    /// # Panics
    /// 同 [`Self::cluster_to_sector`] 的前置条件（经其继承）。
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
        return Err(FatError::InvalidBpb(format!("bad bytes per sector: {bytes_per_sector}")));
    }
    let sectors_per_cluster = sector0[13];
    if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() || sectors_per_cluster > 128 {
        return Err(FatError::InvalidBpb(format!("bad sectors per cluster: {sectors_per_cluster}")));
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
    let root_start_sector = if fat_type == FatType::Fat32 { 0 } else { fat_start_sector + fat_size_sectors * num_fats as u32 };
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
        if clusters < 4085 { FatType::Fat12 } else { FatType::Fat16 }
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
```

`lib.rs` 更新为 `pub mod bpb;`（加 `pub mod fat;` 等在各任务中逐步加）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p xd-fs-fat`
Expected: 13 passed（校验分支全覆 + FAT12 几何 + 溢出/偏移覆盖）。

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/xd-fs-fat
git commit -m "feat(fs-fat): BPB 解析与几何计算（结构优先判型）"
```

---

### Task 4: xd-fs-fat —— FAT 表访问（12/16/32）

**Files:**
- Modify: `crates/xd-fs-fat/src/lib.rs`（+`pub mod fat;`）
- Create: `crates/xd-fs-fat/src/fat.rs`

- [ ] **Step 1: 写失败的测试（`fat.rs` 末尾）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpb;
    use xd_device::image::ImageFileDevice;

    fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(image).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        (f, dev)
    }

    #[test]
    fn follows_fat16_chain_and_reports_eoc() {
        let image = xd_fixtures::FatImageBuilder::fat16().add_file("/", "A.BIN", &[0u8; 1200]).build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        assert_eq!(fat.entry(2).unwrap(), 3);
        assert!(fat.is_eoc_reachable(4).unwrap()); // 1200B → 3 簇：2→3→4(EOC)
        assert_eq!(fat.chain(2).unwrap(), vec![2, 3, 4]);
    }

    #[test]
    fn follows_fat32_chain_with_mask() {
        let image = xd_fixtures::FatImageBuilder::fat32().add_file("/", "A.BIN", &[0u8; 1200]).build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        // 根目录占簇 2，文件从簇 3 起：1200B → 3 簇 3→4→5(EOC)
        assert_eq!(fat.chain(3).unwrap(), vec![3, 4, 5]);
        assert!(fat.is_eoc_reachable(5).unwrap());
    }

    #[test]
    fn follows_fat12_nibble_packing() {
        let image = xd_fixtures::FatImageBuilder::fat12().add_file("/", "A.BIN", &[0u8; 600]).build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        assert_eq!(fat.chain(2).unwrap(), vec![2, 3]); // 600B → 2 簇
    }

    #[test]
    fn wild_cluster_on_fat12_does_not_overflow() {
        // 0xAAAA_AAAB 的 12 位偏移加法曾经 u32 溢出（debug panic）；u64 后为超设备偏移 → Err
        let image = xd_fixtures::FatImageBuilder::fat12().build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        let err = fat.entry(0xAAAA_AAAB).unwrap_err();
        assert!(matches!(err, FatError::InvalidBpb(m) if m.contains("beyond device")));
    }

    #[test]
    fn chain_detects_cycle_within_legal_bound() {
        let image = xd_fixtures::FatImageBuilder::fat16().add_file("/", "A.BIN", &[0u8; 1024]).build();
        let mut patched = image.clone();
        // FAT16 entry(2) @ 516、entry(3) @ 518：造 2→3→2 环
        patched[516..518].copy_from_slice(&3u16.to_le_bytes());
        patched[518..520].copy_from_slice(&2u16.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        let chain = fat.chain(2).unwrap();
        assert!(chain.len() as u32 > bpb.data_cluster_count(), "环应表现为超长链（> count）");
        assert!(chain.len() as u32 <= bpb.data_cluster_count() + 2, "且有界");
    }

    #[test]
    fn chain_rejects_out_of_range_start() {
        let image = xd_fixtures::FatImageBuilder::fat16().build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        for start in [0u32, 1, u32::MAX] {
            assert!(matches!(fat.chain(start), Err(FatError::InvalidBpb(_))));
        }
    }

    #[test]
    fn is_free_reports_freed_clusters() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &[0u8; 1024])
            .delete("/", "GONE.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        assert!(fat.is_free(2).unwrap());
        assert!(fat.is_free(3).unwrap());
    }

    #[test]
    fn broken_chain_for_deleted_file_yields_short_chain() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &[0u8; 1024])
            .delete("/", "GONE.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let bpb = bpb::parse(&dev).unwrap();
        let fat = Fat::new(&dev, &bpb);
        assert_eq!(fat.chain(2).unwrap(), vec![2]); // 首跳即断（0=free）
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xd-fs-fat`
Expected: 编译失败（`fat` 模块未定义）。

- [ ] **Step 3: 实现 `crates/xd-fs-fat/src/fat.rs`**

```rust
//! FAT 表访问：按需读取单个表项（不整表载入——32GB 卡的 FAT 可达 128MB）。
//! ponytail: 每次查询一次 read_at；如需扫描吞吐再引入扇区缓存/预读。

use crate::bpb::{Bpb, FatType};
use crate::FatError;
use xd_device::BlockDevice;

pub struct Fat<'d> {
    dev: &'d dyn BlockDevice,
    bpb: &'d Bpb,
}

impl<'d> Fat<'d> {
    pub fn new(dev: &'d dyn BlockDevice, bpb: &'d Bpb) -> Self {
        Self { dev, bpb }
    }

    /// 读取 cluster 的表项原值（已按类型掩码）。短读（EOF）返回错误而非静默 0。
    pub fn entry(&self, cluster: u32) -> Result<u32, FatError> {
        match self.bpb.fat_type {
            FatType::Fat32 => {
                let mut b = [0u8; 4];
                self.read_entry(self.bpb.fat_entry_byte(cluster), &mut b, cluster)?;
                Ok(u32::from_le_bytes(b) & 0x0FFF_FFFF)
            }
            FatType::Fat16 => {
                let mut b = [0u8; 2];
                self.read_entry(self.bpb.fat_entry_byte(cluster), &mut b, cluster)?;
                Ok(u16::from_le_bytes(b) as u32)
            }
            FatType::Fat12 => {
                let off = self.bpb.fat_start_sector as u64 * self.bpb.bytes_per_sector as u64
                    + cluster as u64 + cluster as u64 / 2;
                let mut b = [0u8; 2];
                self.read_entry(off, &mut b, cluster)?;
                let pair = u16::from_le_bytes(b) as u32;
                Ok(if cluster.is_multiple_of(2) { pair & 0x0FFF } else { pair >> 4 })
            }
        }
    }

    /// 读满 buf，否则报"表项越界"（EOF 短读不得静默成 0=空闲）。
    fn read_entry(&self, off: u64, buf: &mut [u8], cluster: u32) -> Result<(), FatError> {
        let n = self.dev.read_at(off, buf)?;
        if n < buf.len() {
            return Err(FatError::InvalidBpb(format!("fat entry {cluster} beyond device")));
        }
        Ok(())
    }

    pub fn is_free(&self, cluster: u32) -> Result<bool, FatError> {
        Ok(self.entry(cluster)? == 0)
    }

    fn is_eoc(&self, value: u32) -> bool {
        match self.bpb.fat_type {
            FatType::Fat12 => value >= 0x0FF8,
            FatType::Fat16 => value >= 0xFFF8,
            FatType::Fat32 => value >= 0x0FFF_FFF8,
        }
    }

    /// `entry` 指向链尾（EOC 或保留值 1）——两者都视为不可继续。
    pub fn is_eoc_reachable(&self, cluster: u32) -> Result<bool, FatError> {
        let v = self.entry(cluster)?;
        Ok(self.is_eoc(v) || v == 1) // 1 = 保留值（不可继续）
    }

    /// 从 start 顺链读取簇号序列（含 start）。
    /// 合法簇号上界为 `data_cluster_count() + 1`；链长超过 `data_cluster_count()` 必含环
    /// （鸽笼：合法簇数量有限）——T7 可用 `chain.len() > bpb.data_cluster_count()` 判环。
    pub fn chain(&self, start: u32) -> Result<Vec<u32>, FatError> {
        let max_cluster = self.bpb.data_cluster_count() + 1;
        if !(2..=max_cluster).contains(&start) {
            return Err(FatError::InvalidBpb(format!("chain start {start} out of range")));
        }
        let mut out = vec![start];
        let mut cur = start;
        while out.len() as u32 <= max_cluster {
            let v = self.entry(cur)?;
            if v == 0 || v == 1 || self.is_eoc(v) {
                break;
            }
            if v > max_cluster {
                break; // 顺带接住坏簇标记（0xFF7/0xFFF7/0x0FFF_FFF7 均 > max_cluster）
            }
            out.push(v);
            cur = v;
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p xd-fs-fat`
Expected: 21 passed（13 + 8）。

- [ ] **Step 5: Commit**

```bash
git add crates/xd-fs-fat
git commit -m "feat(fs-fat): FAT 表访问（12 半字节/16/32 掩码、链与 EOC）"
```

---

### Task 5: xd-fs-fat —— 目录项解析（SFN / LFN / 槽分类）

**Files:**
- Modify: `crates/xd-fs-fat/src/lib.rs`（+`pub mod dirent;`）
- Create: `crates/xd-fs-fat/src/dirent.rs`

- [ ] **Step 1: 写失败的测试（`dirent.rs` 末尾）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_plain_sfn() {
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(b"HELLO   TXT");
        raw[11] = 0x20;
        let e = parse_slot(&raw);
        assert_eq!(e, Slot::Sfn(Sfn { name83: *b"HELLO   TXT", attr: 0x20, first_cluster: 0, size: 0, deleted: false }));
    }

    #[test]
    fn decodes_deleted_sfn_first_byte_05_quirk() {
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(b"\x05EVIL   TXT"); // 真实名以 0xE5 开头时磁盘上写 0x05（11 字节，勿少空格）
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
        let Slot::Sfn(s) = parse_slot(&raw) else { panic!() };
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
        let mk = |text: &str| LfnSlot { seq_raw: 0xE5, chars: text.encode_utf16().collect(), deleted: true };
        let slots = vec![mk("0.bin"), mk("my_ph")]; // 物理序：先尾段 "0.bin" 后头段 "my_ph"
        assert_eq!(assemble_lfn(&slots), "my_ph0.bin");
    }

    /// LFN 槽第 k 个 UTF-16 单元在 32B 槽内的字节偏移（三窗口：1←0..4、14←5..10、28←11..12）。
    fn lfn_char_offset(k: usize) -> usize {
        match k {
            0..=4 => 1 + k * 2,
            5..=10 => 14 + (k - 5) * 2,
            _ => 28 + (k - 11) * 2,
        }
    }

    #[test]
    fn lowercase_flags_come_from_ntres() {
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(b"README  TXT");
        raw[11] = 0x20; // attr=archive（bit3/4 不参与小写）
        raw[12] = 0x18; // NTRes：基名+扩展名小写
        let Slot::Sfn(s) = parse_slot(&raw) else { panic!() };
        assert_eq!(s.nt_res, 0x18);
        assert_eq!(assemble_sfn_name(&s.name83, s.nt_res), "readme.txt");
        let mut vol = [0u8; 32];
        vol[..11].copy_from_slice(b"MYDISK     ");
        vol[11] = 0x08; // 卷标
        let Slot::Sfn(s) = parse_slot(&vol) else { panic!() };
        assert_eq!(assemble_sfn_name(&s.name83, s.nt_res), "MYDISK");
    }

    #[test]
    fn lfn_slots_parse_from_raw_bytes() {
        let mut raw = [0u8; 32];
        raw[0] = 0x41; // seq=1 | 0x40（唯一段）
        raw[11] = 0x0F;
        let text: Vec<u16> = "photo_2024.jp".encode_utf16().collect();
        assert_eq!(text.len(), 13);
        for (k, &u) in text.iter().enumerate() {
            let i = lfn_char_offset(k);
            raw[i..i + 2].copy_from_slice(&u.to_le_bytes());
        }
        let Slot::Lfn(l) = parse_slot(&raw) else { panic!() };
        assert_eq!(String::from_utf16_lossy(&l.chars), "photo_2024.jp");
        assert!(!l.deleted);
    }

    #[test]
    fn mixed_run_falls_back_to_sfn_name() {
        let mut lfn = [0u8; 32];
        lfn[0] = 0xE5; // 删除态孤儿
        lfn[11] = 0x0F;
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
        // 存活 LFN（seq=1|0x40, "photo.jpg"）+ 存活 SFN → 名字取 LFN
        let mut lfn = [0u8; 32];
        lfn[0] = 0x41;
        lfn[11] = 0x0F;
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

        // 删除孤儿：两删除 LFN 槽（物理尾→头）+ 删除 SFN → 逆序重建
        let mk_deleted = |text: &str| {
            let mut s = [0u8; 32];
            s[0] = 0xE5;
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
        data2.extend_from_slice(&mk_deleted("gone_0"));
        data2.extend_from_slice(&mk_deleted("old_"));
        data2.extend_from_slice(&dsfn);
        let parsed2 = parse_directory_bytes(&data2);
        assert_eq!(parsed2.len(), 1);
        assert_eq!(parsed2[0].name, "old_gone_0");
        assert!(parsed2[0].deleted);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xd-fs-fat`
Expected: 编译失败。

- [ ] **Step 3: 实现 `crates/xd-fs-fat/src/dirent.rs`**

```rust
//! 目录项解析：32 字节槽 → Sfn / Lfn / End / Free；名称组装（含删除项重建）。

pub const ATTR_LFN: u8 = 0x0F;
pub const ATTR_DIRECTORY: u8 = 0x10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sfn {
    pub name83: [u8; 11],
    pub attr: u8,
    pub nt_res: u8, // raw[12]：NTRes，bit3=基名小写 / bit4=扩展名小写（attr 的 bit3/4 是卷标/目录属性，勿混用）
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
        for &(a, b) in &[(1usize, 10usize), (14, 25), (28, 31)] {
            let mut i = a;
            while i < b + 1 && i + 1 <= 31 {
                let u = u16::from_le_bytes([raw[i], raw[i + 1]]);
                if u == 0x0000 || u == 0xFFFF {
                    break;
                }
                chars.push(u);
                i += 2;
            }
        }
        return Slot::Lfn(LfnSlot { seq_raw: raw[0], chars, deleted });
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
/// 非 ASCII 字节按 Latin-1（`b as char`）呈现——保留磁盘原始 OEM 字节，0xE5 quirk 无损；
/// 不依赖 UTF-8（0xE5 单字节经 from_utf8_lossy 会变 U+FFFD）。
pub fn assemble_sfn_name(name83: &[u8; 11], lcase: u8) -> String {
    let mut base: Vec<u8> = name83[..8].iter().copied().take_while(|&c| c != b' ').collect();
    let ext: Vec<u8> = name83[8..].iter().copied().take_while(|&c| c != b' ').collect();
    let first_deleted = base.first() == Some(&0xE5);
    if let Some(f) = base.first_mut() {
        if *f == 0x05 {
            *f = 0xE5; // 0x05 → 真实的 0xE5 首字节（非删除）
        } else if *f == 0xE5 {
            *f = b'?'; // 删除项首字符丢失
        }
    }
    let _ = first_deleted;
    let apply = |bytes: &mut [u8], lower: bool| {
        if lower {
            for b in bytes.iter_mut() {
                *b = b.to_ascii_lowercase();
            }
        }
    };
    apply(&mut base, lcase & 0x08 != 0);
    let mut ext = ext;
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

/// 解析一整块目录数据（bps 对齐扇区的连续字节）为合并后的条目序列。
/// 规则：紧邻 SFN 之前、连续的 LFN 槽组归属该 SFN；End 槽终止解析。
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
            Slot::Free => {}
            Slot::Sfn(s) => {
                // 连续性校验：LFN run 与 SFN 同为存活或同为删除才配对（简化：仅要求非空）
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
                    is_dir: s.attr & ATTR_DIRECTORY != 0 && !s.deleted,
                    has_lfn,
                });
            }
        }
    }
    out
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p xd-fs-fat`
Expected: 31 passed（21 + 10）。

- [ ] **Step 5: Commit**

```bash
git add crates/xd-fs-fat
git commit -m "feat(fs-fat): 目录项解析（SFN/LFN、0x05 与 0xE5 quirk、删除孤儿重建）"
```

---

### Task 6: xd-fs-fat —— 扫描（递归 + 质量分级）

**Files:**
- Modify: `crates/xd-fs-fat/src/lib.rs`（+`pub mod scan;`）
- Create: `crates/xd-fs-fat/src/scan.rs`

- [ ] **Step 1: 写失败的测试（`scan.rs` 末尾）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use xd_device::image::ImageFileDevice;

    fn dev_for(image: &[u8]) -> (tempfile::NamedTempFile, ImageFileDevice) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(image).unwrap();
        f.flush().unwrap();
        let dev = ImageFileDevice::open(f.path()).unwrap();
        (f, dev)
    }

    #[test]
    fn finds_live_and_deleted_files() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "LIVE.TXT", b"alive")
            .add_file("/", "GONE.JPG", &[9u8; 700])
            .delete("/", "GONE.JPG")
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let live = entries.iter().find(|e| e.name == "LIVE.TXT").unwrap();
        assert!(!live.deleted);
        assert_eq!(live.quality, RecoverQuality::Complete);
        let gone = entries.iter().find(|e| e.deleted).unwrap();
        assert!(gone.deleted);
        assert_eq!(gone.size_bytes, 700);
        assert_eq!(gone.quality, RecoverQuality::Complete); // 全簇空闲
    }

    #[test]
    fn deleted_name_loses_first_char_or_uses_lfn() {
        // SFN 删除名首字符丢失 → "?ONE.JPG"
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.JPG", &[1u8; 300])
            .delete("/", "GONE.JPG")
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let d = entries.iter().find(|e| e.deleted).unwrap();
        assert!(d.name.starts_with('?'), "expected '?ONE.JPG', got {}", d.name);
    }

    #[test]
    fn overwritten_clusters_grade_maybe_damaged() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "OLD.BIN", &[7u8; 1024])
            .delete("/", "OLD.BIN")
            .add_file("/", "NEW.BIN", &[9u8; 1024]) // 复用簇
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let old = entries.iter().find(|e| e.deleted).unwrap();
        assert_eq!(old.quality, RecoverQuality::MaybeDamaged);
    }

    #[test]
    fn recurses_subdirectories() {
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_subdir("/", "PHOTOS")
            .add_file("/PHOTOS", "IMG.JPG", &[3u8; 100])
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let img = entries.iter().find(|e| e.name == "IMG.JPG").unwrap();
        assert_eq!(img.path, "/PHOTOS");
        // 目录项本身也在结果里
        assert!(entries.iter().any(|e| e.name == "PHOTOS" && e.is_dir));
    }

    #[test]
    fn scans_fat32_and_fat12_images() {
        for builder in [xd_fixtures::FatImageBuilder::fat32(), xd_fixtures::FatImageBuilder::fat12()] {
            let image = builder.add_file("/", "K.TXT", b"ok").build();
            let (_f, dev) = dev_for(&image);
            let entries = scan(&dev).unwrap();
            assert!(entries.iter().any(|e| e.name == "K.TXT"), "scan failed for {:?}", entries);
        }
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xd-fs-fat`
Expected: 编译失败（`scan` 未定义）。

- [ ] **Step 3: 实现 `crates/xd-fs-fat/src/scan.rs`**

```rust
//! 快速扫描：目录遍历（根 + 子目录）+ 删除文件找回 + 质量分级 + 文件读取。

use crate::bpb::{self, Bpb, FatType};
use crate::dirent::{self, ParsedEntry};
use crate::fat::Fat;
use crate::FatError;
use xd_device::BlockDevice;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverQuality {
    /// 数据簇全部空闲，且长度按连续假设可得
    Complete,
    /// 有簇已被重新分配（覆盖风险），或簇信息缺失
    MaybeDamaged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FatEntry {
    pub name: String,
    pub path: String, // "/" 或 "/DIR"
    pub size_bytes: u64,
    pub first_cluster: u32,
    pub deleted: bool,
    pub is_dir: bool,
    pub quality: RecoverQuality,
    pub ext: String, // 小写，无扩展名 = ""
}

const MAX_ENTRIES: usize = 200_000;
const MAX_DEPTH: u32 = 32;

pub fn scan(dev: &dyn BlockDevice) -> Result<Vec<FatEntry>, FatError> {
    let bpb = bpb::parse(dev)?;
    let fat = Fat::new(dev, &bpb);
    let mut out = Vec::new();
    if bpb.fat_type == FatType::Fat32 {
        scan_cluster_dir(dev, &bpb, &fat, bpb.root_cluster, "/", 0, &mut out)?;
    } else {
        scan_fixed_root(dev, &bpb, &fat, &mut out)?;
    }
    Ok(out)
}

/// FAT12/16 固定根目录。
fn scan_fixed_root(dev: &dyn BlockDevice, bpb: &Bpb, fat: &Fat, out: &mut Vec<FatEntry>) -> Result<(), FatError> {
    let root_bytes = ((bpb.root_entry_count as u32) * 32) as usize;
    let mut buf = vec![0u8; root_bytes];
    let start = bpb.root_start_sector as u64 * bpb.bytes_per_sector as u64;
    let n = dev.read_at(start, &mut buf)?;
    let parsed = dirent::parse_directory_bytes(&buf[..n]); // 短读 → 只解析已读部分（不得零填充当 End）
    append_parsed(dev, bpb, fat, parsed, "/", 0, out)
}

/// 簇链目录（FAT32 根与所有子目录）。
fn scan_cluster_dir(
    dev: &dyn BlockDevice,
    bpb: &Bpb,
    fat: &Fat,
    start_cluster: u32,
    path: &str,
    depth: u32,
    out: &mut Vec<FatEntry>,
) -> Result<(), FatError> {
    if depth > MAX_DEPTH || out.len() > MAX_ENTRIES {
        return Ok(());
    }
    let mut data = Vec::new();
    let mut buf = vec![0u8; bpb.cluster_bytes() as usize];
    let chain = match fat.chain(start_cluster) {
        Ok(c) => c,
        Err(_) => return Ok(()), // 坏目录链：跳过该目录，其余继续（保守降级，绝不中止全盘）
    };
    for c in chain {
        let n = match dev.read_at(bpb.cluster_to_byte(c), &mut buf) {
            Ok(n) => n,
            Err(_) => break, // 读失败：解析已收集部分
        };
        if n < buf.len() {
            break;
        }
        data.extend_from_slice(&buf);
        if data.len() > 64 * 1024 * 1024 {
            break; // 防御：目录不可能这么大
        }
    }
    let parsed = dirent::parse_directory_bytes(&data);
    append_parsed(dev, bpb, fat, parsed, path, depth, out)
}

fn append_parsed(
    dev: &dyn BlockDevice,
    bpb: &Bpb,
    fat: &Fat,
    parsed: Vec<ParsedEntry>,
    path: &str,
    depth: u32,
    out: &mut Vec<FatEntry>,
) -> Result<(), FatError> {
    for e in parsed {
        let ext = e.name.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase()).unwrap_or_default();
        let quality = if e.is_dir {
            RecoverQuality::Complete
        } else if e.deleted {
            grade_deleted(fat, bpb, e.first_cluster, e.size)?
        } else {
            RecoverQuality::Complete
        };
        out.push(FatEntry {
            name: e.name.clone(),
            path: path.to_string(),
            size_bytes: e.size as u64,
            first_cluster: e.first_cluster,
            deleted: e.deleted,
            is_dir: e.is_dir,
            quality,
            ext,
        });
        // 只递归存活目录（已删除目录的簇可能被再分配，M1a 不深入）
        if e.is_dir && !e.deleted && e.first_cluster >= 2 {
            let child_path = if path == "/" { format!("/{}", e.name) } else { format!("{path}/{}", e.name) };
            scan_cluster_dir(dev, bpb, fat, e.first_cluster, &child_path, depth + 1, out)?;
        }
    }
    Ok(())
}

/// 删除文件质量分级：按连续簇假设检查每个簇是否空闲。
fn grade_deleted(fat: &Fat, bpb: &Bpb, first_cluster: u32, size: u32) -> Result<RecoverQuality, FatError> {
    if size == 0 {
        return Ok(RecoverQuality::Complete);
    }
    if first_cluster < 2 {
        return Ok(RecoverQuality::MaybeDamaged); // 无簇信息（如删除后 first_cluster 被清零）
    }
    let need = (size as u32).div_ceil(bpb.cluster_bytes());
    let max_cluster = bpb.data_cluster_count() + 1;
    for i in 0..need {
        let c = first_cluster + i;
        if c > max_cluster {
            return Ok(RecoverQuality::MaybeDamaged); // 越界：表项不可信
        }
        match fat.is_free(c) {
            Ok(true) => {}                                // 确证空闲
            _ => return Ok(RecoverQuality::MaybeDamaged), // Err 与 Ok(false) 同路降级：宁可漏报不可错报
        }
    }
    Ok(RecoverQuality::Complete)
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p xd-fs-fat`
Expected: 36 passed（31 + 5）。

- [ ] **Step 5: Commit**

```bash
git add crates/xd-fs-fat
git commit -m "feat(fs-fat): 扫描（递归、删除找回、质量分级）"
```

---

### Task 7: xd-fs-fat —— 文件读取（链读 + 连续回退）

**Files:**
- Modify: `crates/xd-fs-fat/src/scan.rs`

- [ ] **Step 1: 写失败的测试（追加到 `scan.rs` 测试模块）**

```rust
    #[test]
    fn reads_live_file_exactly() {
        let data: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16().add_file("/", "DATA.BIN", &data).build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.name == "DATA.BIN").unwrap();
        let bytes = read_file(&dev, e).unwrap();
        assert_eq!(bytes, data);
    }

    #[test]
    fn reads_deleted_file_via_contiguous_fallback() {
        let data: Vec<u8> = (0..1200u32).map(|i| (i % 253) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &data)
            .delete("/", "GONE.BIN")
            .build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.deleted).unwrap();
        let bytes = read_file(&dev, e).unwrap();
        assert_eq!(bytes, data); // 删除后 FAT 链已清 → 连续回退精确还原
    }

    #[test]
    fn deleted_entry_ignores_reused_chain_reads_original() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let image = xd_fixtures::FatImageBuilder::fat16()
            .add_file("/", "GONE.BIN", &data)
            .delete("/", "GONE.BIN")
            .build();
        let mut patched = image.clone();
        // 模拟非连续复用：entry(2)=7、entry(7)=8、entry(8)=EOC（旧文件数据仍在簇 2,3）
        patched[516..518].copy_from_slice(&7u16.to_le_bytes());
        patched[526..528].copy_from_slice(&8u16.to_le_bytes());
        patched[528..530].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let (_f, dev) = dev_for(&patched);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.deleted).unwrap();
        let bytes = read_file(&dev, e).unwrap();
        assert_eq!(bytes, data); // 判据修正：删除项走连续回退而非他人链
    }

    #[test]
    fn reads_exact_size_not_full_cluster() {
        let data = b"short".to_vec(); // 5 字节 < 1 簇
        let image = xd_fixtures::FatImageBuilder::fat32().add_file("/", "S.TXT", &data).build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        let e = entries.iter().find(|e| e.name == "S.TXT").unwrap();
        assert_eq!(read_file(&dev, e).unwrap(), data);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xd-fs-fat`
Expected: 编译失败（`read_file` 未定义）。

- [ ] **Step 3: 实现（追加到 `scan.rs`）**

```rust
/// 读取文件内容（恰好 size 字节）。
/// 策略：先顺 FAT 链读；链长不足（删除后 FAT 已清）→ 按连续簇回退。
pub fn read_file(dev: &dyn BlockDevice, entry: &FatEntry) -> Result<Vec<u8>, FatError> {
    if entry.size_bytes == 0 || entry.first_cluster < 2 {
        return Ok(Vec::new());
    }
    let bpb = bpb::parse(dev)?;
    let fat = Fat::new(dev, &bpb);
    let size = entry.size_bytes as usize;
    let need = (size as u32).div_ceil(bpb.cluster_bytes()) as usize;
    let chain = fat.chain(entry.first_cluster).unwrap_or_default(); // 坏链 → 空 → 走连续回退
    let looped = chain.len() as u32 > bpb.data_cluster_count(); // 合法簇仅 count 个：> count ⟺ 必含环
    let clusters: Vec<u32> = if !entry.deleted && !looped && chain.len() >= need {
        chain[..need].to_vec()
    } else {
        // 删除项（M1a 语义下其链必属他人——删除即清 FAT）或环：按连续假设读
        (0..need as u32).map(|i| entry.first_cluster + i).collect()
    };
    let mut out = Vec::with_capacity(size);
    let mut buf = vec![0u8; bpb.cluster_bytes() as usize];
    for c in clusters {
        let n = dev.read_at(bpb.cluster_to_byte(c), &mut buf)?;
        if n < buf.len() {
            break;
        }
        out.extend_from_slice(&buf);
        if out.len() >= size {
            break;
        }
    }
    out.truncate(size);
    Ok(out)
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p xd-fs-fat`
Expected: 40 passed（36 + 4）。

- [ ] **Step 5: Commit**

```bash
git add crates/xd-fs-fat
git commit -m "feat(fs-fat): 文件读取（链读 + 连续回退，精确 size）"
```

---

### Task 8: 端到端 —— 镜像文件 → 扫描 → 字节级找回

**Files:**
- Create: `crates/xd-fs-fat/tests/roundtrip.rs`、`fixtures/gen_fat_image.rs`

- [ ] **Step 1: 写端到端测试 `crates/xd-fs-fat/tests/roundtrip.rs`**

```rust
//! M1a 出口标准：合成镜像落盘 → BlockDevice 打开 → 扫描 → 读回删除文件
//! 的字节与原始数据完全一致（「U 盘删照片」的镜像版）。

use xd_device::image::ImageFileDevice;

#[test]
fn deleted_photo_recovered_byte_exact_from_image_file() {
    // 造一张"相机卡"：500 字节的"照片"（内容确定），删掉它
    let photo: Vec<u8> = (0..500u32).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let image_bytes = xd_fixtures::FatImageBuilder::fat16()
        .add_subdir("/", "DCIM")
        .add_file("/DCIM", "IMG_0001.JPG", &photo)
        .add_file("/", "READ_ME.TXT", b"keep me")
        .delete("/DCIM", "IMG_0001.JPG")
        .build();

    // 落盘为镜像文件，走真实 BlockDevice 通路
    let mut f = tempfile::NamedTempFile::new().unwrap();
    use std::io::Write;
    f.write_all(&image_bytes).unwrap();
    f.flush().unwrap();
    let dev = ImageFileDevice::open(f.path()).unwrap();

    // 扫描
    let entries = xd_fs_fat::scan::scan(&dev).unwrap();
    let photo_entry = entries
        .iter()
        .find(|e| e.deleted && e.ext == "jpg")
        .expect("deleted jpg not found");
    assert_eq!(photo_entry.size_bytes, 500);

    // 字节级找回
    let recovered = xd_fs_fat::scan::read_file(&dev, photo_entry).unwrap();
    assert_eq!(recovered, photo, "recovered bytes differ from original");

    // 存活文件不受影响
    assert!(entries.iter().any(|e| e.name == "READ_ME.TXT" && !e.deleted));
}
```

- [ ] **Step 2: 运行确认通过（全 crate 测试）**

Run: `cargo test -p xd-fs-fat`
Expected: 41 passed（40 + 1，含新 e2e）。

（若 `xd_fs_fat::scan::scan` 路径过深，可在 `lib.rs` re-export：`pub use scan::{read_file, scan, FatEntry, RecoverQuality};`——**本步允许这一行改动**。）

- [ ] **Step 3: 写镜像生成示例 `fixtures/gen_fat_image.rs`（供后续 M1b e2e 与手工调试）**

```rust
// 生成一张含"已删除照片"的 FAT16 镜像到文件：
//   cargo run -p xd-fixtures --example gen_fat_image -- /tmp/xd-fat.img
fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/xd-fat.img".into());
    let photo: Vec<u8> = (0..65_536u32).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_subdir("/", "DCIM")
        .add_file("/DCIM", "IMG_0001.JPG", &photo)
        .delete("/DCIM", "IMG_0001.JPG")
        .build();
    std::fs::write(&path, &image).unwrap();
    println!("wrote {} ({} bytes)", path, image.len());
}
```

放在 `crates/xd-fixtures/examples/gen_fat_image.rs`（Cargo example 机制），运行验证：

Run: `cargo run -p xd-fixtures --example gen_fat_image -- /tmp/xd-fat.img`
Expected: `wrote /tmp/xd-fat.img (2162688 bytes)`（FAT16 = 4224×512 = 2,162,688）

- [ ] **Step 4: Commit**

```bash
git add crates/xd-fs-fat/tests crates/xd-fixtures/examples
git commit -m "test(fs-fat): 端到端字节级找回 + 镜像生成示例"
```

---

## M1a 出口验收

- [ ] `cargo test --workspace --locked` 全绿；`cargo clippy --workspace --all-targets --locked -- -D warnings` 零告警
- [ ] `cargo fmt --all --check` 干净
- [ ] 端到端测试 `deleted_photo_recovered_byte_exact_from_image_file` 通过（字节级）
- [ ] `cargo run -p xd-fixtures --example gen_fat_image` 能产出可被 `xd-daemon` 后续切片直接挂载的镜像
- [ ] `bash scripts/apply-copyright.sh` 对新文件补版权头（幂等）；`provenance.sha256` 重生成（若本计划作为发布前工作）

## 后续切片（各自独立计划）

- **M1a2**：exFAT 解析（目录项集 0x85/0xC0/0xC1、簇位图、删除位）
- **M1b**：契约 v1（`scan.start`/`scan.progress`/`scan.results` 事件流）+ daemon 并发（writer 串行化 + 扫描任务线程）+ 任务状态机 + xd-core 编排（把 `xd-fs-fat` 的结果映射为协议消息）
- **M1c**：`xd-carving`（JPEG/PNG 签名雕刻 v1）
- **M1d**：UI 三页（扫描控制/结果浏览/预览）+ 恢复导出（异设备校验 + 报告）
- **M1e**：物理设备枚举（Linux `/dev/sdX` 先行）+ 提权（Linux polkit）+ Linux 打包；Windows/macOS 平台层单独排期

---

© 2026 erik · https://erik.xyz · erik@erik.xyz
