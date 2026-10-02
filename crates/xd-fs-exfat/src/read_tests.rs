// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 测试体集中于此属刻意（源文件线宽控制的另一半）；本文件不受 500 行约束。
use super::*;
use crate::boot::testutil::dev_for;
use crate::scan::{RecoverQuality, scan};
use xd_fixtures::refix_deleted_checksum;

const ROOT_B: usize = 32 * 512 + 3 * 4096;
const SET: usize = ROOT_B + 96;
const FAT_B: usize = 24 * 512;

#[test]
fn reads_contiguous_file_exact() {
    let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "A.BIN", &data)
        .build();
    let (_f, dev) = dev_for(&image);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "A.BIN")
        .unwrap();
    assert_eq!(read_file(&dev, &e).unwrap(), data);
}

#[test]
fn reads_chained_fragmented_in_order() {
    let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "F.BIN", &data, &[7, 6, 8], false)
        .build();
    let (_f, dev) = dev_for(&image);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "F.BIN")
        .unwrap();
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
fn deleted_contiguous_middle_occupied_truncates_prefix() {
    // qual-t4 G2（minor 补强：占 3 簇的**中段**）：逐簇门控必须 stop 不能 skip——跳过被占簇
    // 续读会交付 8192 = 簇 6+8 错位数据；分级同步降级（连续车道=位图把关，链式车道=stale 链把关）
    let data: Vec<u8> = (0..12288u32).map(|i| (i % 239) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "G.BIN", &data)
        .delete("/", "G.BIN")
        .add_file_in_clusters("/", "NEW.BIN", &[5u8; 100], &[7], true)
        .build();
    let (_f, dev) = dev_for(&image);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "G.BIN")
        .unwrap();
    assert!(e.deleted);
    assert_eq!(
        e.quality,
        RecoverQuality::MaybeDamaged,
        "中段被占 → 位图门控降级"
    );
    let bytes = read_file(&dev, &e).unwrap();
    assert_eq!(bytes.len(), 4096, "中段被占 → 逐簇门控截断");
    assert_eq!(bytes, data[..4096]);
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
    assert_eq!(
        read_file(&dev, &e).unwrap(),
        data,
        "位图全空 → stale 链可用（仍是原数据）"
    );
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
    let old = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "OLD.BIN")
        .unwrap();
    assert_eq!(old.quality, RecoverQuality::MaybeDamaged);
    let bytes = read_file(&dev, &old).unwrap();
    assert_eq!(bytes.len(), 4096, "只交付首个空闲簇");
    assert_eq!(bytes, data[..4096]);
}

#[test]
fn deleted_chain_tail_occupied_prefix_free_delivers_full() {
    // I2 的读半壁（对 T5 的 deleted_chain_grades_on_need_prefix_only）：stale 链 [6,9,7,10]，
    // 链尾 7/10 已是 NEW.BIN 的链；need=2 前缀 [6,9] 全空闲 → 必须按 stale 链交付 8192。
    // 查整链（旧稿）会回退连续 [6,7] 再被位图截断 → 只交付 4096（qual-t5 探针）。
    let data: Vec<u8> = (0..9000u32).map(|i| (i % 229) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "OLD.BIN", &data, &[6, 9, 7], false)
        .delete("/", "OLD.BIN")
        .add_file_in_clusters("/", "NEW.BIN", &[5u8; 4600], &[7, 10], false)
        .build();
    let mut patched = image.clone();
    let stream = SET + 32;
    // VDL 与 DL 同补成 8192：vdl > dl 会被 T4 直接丢弃（夹具 vdl=9000）
    patched[stream + 8..stream + 16].copy_from_slice(&8192u64.to_le_bytes());
    patched[stream + 24..stream + 32].copy_from_slice(&8192u64.to_le_bytes());
    refix_deleted_checksum(&mut patched, SET, 3);
    let (_f, dev) = dev_for(&patched);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "OLD.BIN")
        .unwrap();
    assert!(e.deleted);
    assert_eq!(
        e.quality,
        RecoverQuality::Complete,
        "T5 判据：前缀全空 → Complete"
    );
    let bytes = read_file(&dev, &e).unwrap();
    assert_eq!(bytes.len(), 8192, "链尾被占不得回退连续（会只交付 4096）");
    assert_eq!(bytes, data[..8192], "按链序 6→9 拼接");
}

#[test]
fn deleted_wiped_stale_chain_delivers_honest_prefix() {
    // (a) 探针 B：碎片化删除项（链序 [6,9,7] ≠ 物理序），删除后 stale 链被清（FAT[6]=0，
    // 如部分工具删除时清链）→ 只沿链走到链断：仅簇 6 可交付（4096B）。旧式"链证伪退连续"
    // 会读入连续 [6,7,8] 三簇并按 size 截断交付 9000B 错位数据（12288 仅为原始读取量）。
    let data: Vec<u8> = (0..9000u32).map(|i| (i % 223) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "OLD.BIN", &data, &[6, 9, 7], false)
        .delete("/", "OLD.BIN")
        .build();
    let mut patched = image.clone();
    patched[FAT_B + 6 * 4..FAT_B + 6 * 4 + 4].copy_from_slice(&0u32.to_le_bytes()); // 清链首跳
    let (_f, dev) = dev_for(&patched);
    let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
    assert_eq!(e.quality, RecoverQuality::MaybeDamaged, "链证不足 → 封顶");
    let bytes = read_file(&dev, &e).unwrap();
    assert_eq!(
        bytes,
        data[..4096],
        "只交付链上确证的第一个簇，绝不连续猜读"
    );
}

#[test]
fn deleted_occupied_chain_cluster_stops_even_if_contiguous_free() {
    // (a) 探针 C：链 [6,9,7]，簇 9 被 NEW.BIN 复用（位图置位；FAT[9] 保留旧值 7——NEW.BIN
    // 连续不写 FAT）→ 链从 6 走到 9 即止、且簇 9 被占用 → 只交付簇 6（4096B）；
    // 连续区间 [6,7,8] 全空闲也不得猜读。
    let data: Vec<u8> = (0..9000u32).map(|i| (i % 211) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "OLD.BIN", &data, &[6, 9, 7], false)
        .delete("/", "OLD.BIN")
        .add_file_in_clusters("/", "NEW.BIN", &[5u8; 4000], &[9], true)
        .build();
    let (_f, dev) = dev_for(&image);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "OLD.BIN")
        .unwrap();
    assert_eq!(
        e.quality,
        RecoverQuality::MaybeDamaged,
        "链 [6,9,7] 中被占簇 9 截断（NEW.BIN 连续不写 FAT，FAT[9] 保留旧值）"
    );
    let bytes = read_file(&dev, &e).unwrap();
    assert_eq!(bytes, data[..4096], "被占用簇即止；连续区间空闲≠可猜");
}

#[test]
fn deleted_loop_chain_beyond_reachable_delivers_nothing() {
    // 同 scan 侧构造：无界卫时交付 3×同一簇的重复字节（伪造序）——界卫必须空交付
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "OLD.BIN", &[7u8; 4000], &[253], false)
        .delete("/", "OLD.BIN")
        .build();
    let mut patched = image.clone();
    let stream = SET + 32;
    patched[stream + 8..stream + 16].copy_from_slice(&12288u64.to_le_bytes());
    patched[stream + 24..stream + 32].copy_from_slice(&12288u64.to_le_bytes());
    patched[24 * 512 + 253 * 4..24 * 512 + 253 * 4 + 4].copy_from_slice(&253u32.to_le_bytes());
    refix_deleted_checksum(&mut patched, SET, 3);
    let (_f, dev) = dev_for(&patched);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "OLD.BIN")
        .unwrap();
    assert!(
        read_file(&dev, &e).unwrap().is_empty(),
        "物理不可能的链 → 确定性空（不得重复交付同一簇）"
    );
}

#[test]
fn deleted_nonrevisit_chain_beyond_reachable_delivers_nothing() {
    // qual-t4 G1：链 253→6→7 无回访但越过可达（253 是末簇，reachable=1 < need=3）
    // ——只有可达界卫能拦（回访检测不触发）→ 空交付
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "OLD.BIN", &[7u8; 4000], &[253], false)
        .delete("/", "OLD.BIN")
        .build();
    let mut patched = image.clone();
    let stream = SET + 32;
    patched[stream + 8..stream + 16].copy_from_slice(&12288u64.to_le_bytes()); // VDL
    patched[stream + 24..stream + 32].copy_from_slice(&12288u64.to_le_bytes()); // DL：need=3
    patched[24 * 512 + 253 * 4..24 * 512 + 253 * 4 + 4].copy_from_slice(&6u32.to_le_bytes()); // FAT[253]=6
    patched[24 * 512 + 6 * 4..24 * 512 + 6 * 4 + 4].copy_from_slice(&7u32.to_le_bytes()); // FAT[6]=7
    refix_deleted_checksum(&mut patched, SET, 3);
    let (_f, dev) = dev_for(&patched);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "OLD.BIN")
        .unwrap();
    assert!(
        read_file(&dev, &e).unwrap().is_empty(),
        "非回访链越过可达 → 界卫空交付"
    );
}

#[test]
fn deleted_loop_prefix_within_reachable_delivers_nothing() {
    // 同 scan 侧构造：无检测时交付 8192B（同一 4096B 簇读两次）——前缀回访必须空交付
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "OLD.BIN", &[7u8; 4000], &[252], false)
        .delete("/", "OLD.BIN")
        .build();
    let mut patched = image.clone();
    let stream = SET + 32;
    patched[stream + 8..stream + 16].copy_from_slice(&8192u64.to_le_bytes());
    patched[stream + 24..stream + 32].copy_from_slice(&8192u64.to_le_bytes());
    patched[24 * 512 + 252 * 4..24 * 512 + 252 * 4 + 4].copy_from_slice(&252u32.to_le_bytes());
    refix_deleted_checksum(&mut patched, SET, 3);
    let (_f, dev) = dev_for(&patched);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "OLD.BIN")
        .unwrap();
    assert!(
        read_file(&dev, &e).unwrap().is_empty(),
        "前缀回访 → 确定性空，不得重复交付同一簇"
    );
}

#[test]
fn deleted_noncontiguous_exact_fit_is_complete_and_delivers_full() {
    // 边界相等：fc=252、链 252→253、need=2=reachable、无回访 → 界卫与回访检测都不得误拒
    let data: Vec<u8> = (0..8192u32).map(|i| (i % 233) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "G.BIN", &data, &[252, 253], false)
        .delete("/", "G.BIN")
        .build();
    let (_f, dev) = dev_for(&image);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "G.BIN")
        .unwrap();
    assert_eq!(
        e.quality,
        RecoverQuality::Complete,
        "链足 need 且前缀全空闲 → Complete（边界相等不误拒）"
    );
    assert_eq!(read_file(&dev, &e).unwrap(), data, "无回访 → 全量交付");
}

#[test]
fn vdl_lt_dl_delivers_vdl_only() {
    // 磁盘上有 9000 字节真实数据，但 VDL=5000 → 交付绝不越过 VDL
    let data: Vec<u8> = (0..9000u32).map(|i| (i % 233) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_with_vdl("/", "V.BIN", &data, 5000)
        .build();
    let (_f, dev) = dev_for(&image);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "V.BIN")
        .unwrap();
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
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "A.BIN")
        .unwrap();
    let bytes = read_file(&dev, &e).unwrap();
    assert_eq!(bytes.len(), 4096, "live 只信链：链只剩首簇 → 诚实短前缀");
    assert_eq!(bytes, data[..4096]);
}

#[test]
fn live_loop_chain_delivers_prefix_up_to_first_revisit() {
    // qual-t4 追加：live 链自环 FAT[6]=6 → 只交付首个回访点之前的簇（4096），绝不让同一簇重复出现；
    // 质量仍是 entry 层 checksum 语义（本测钉住该刻意行为，M1c 若升级链感知分级再改）
    let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
    let mut image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "L.BIN", &data, &[6, 7, 8], false)
        .build();
    image[FAT_B + 6 * 4..FAT_B + 6 * 4 + 4].copy_from_slice(&6u32.to_le_bytes()); // 自环
    let (_f, dev) = dev_for(&image);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "L.BIN")
        .unwrap();
    assert_eq!(
        e.quality,
        RecoverQuality::Complete,
        "live 质量=entry 层语义（刻意）"
    );
    let bytes = read_file(&dev, &e).unwrap();
    assert_eq!(bytes.len(), 4096, "首个回访点之前恰一簇");
    assert_eq!(bytes, data[..4096]);
}

#[test]
fn live_polluted_lengths_stay_chain_bounded() {
    // qual-t4 G3：live 链式单簇 [100]、VDL/DL 均污染成 12288（need=3）→ 交付只来自链上实簇：
    // 恰 4096B（== 真数据），绝不按 DL 放大或连续猜读（链界住）。live 无 refix → 质量断言略。
    // （只污染 DL 时 VDL 先行截断，"恰 4096"丧失判别力——两条长度一起污染；簇位取堆中段 100，
    // 101/102 在设备内可读 ⇒ 判别落在簇源=链，不靠"簇在设备外早停"）
    let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "L.BIN", &data, &[100], false)
        .build();
    let mut patched = image.clone();
    let stream = SET + 32;
    patched[stream + 8..stream + 16].copy_from_slice(&12288u64.to_le_bytes()); // VDL 污染
    patched[stream + 24..stream + 32].copy_from_slice(&12288u64.to_le_bytes()); // DL 污染
    let (_f, dev) = dev_for(&patched);
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "L.BIN")
        .unwrap();
    assert_eq!(e.size_bytes, 12288);
    let bytes = read_file(&dev, &e).unwrap();
    assert_eq!(bytes.len(), 4096, "被链界住（单簇），非被 DL 放大");
    assert_eq!(bytes, data);
}

#[test]
fn wild_first_cluster_bounded_not_panic() {
    // 野生 first_cluster + 大 DL → u64 累积 + 同界截断；不得 panic、不得伪造
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "X.TXT", b"x")
        .build();
    let (_f, dev) = dev_for(&image);
    let e = ExfatEntry {
        name: "WILD.BIN".into(),
        path: "/".into(),
        size_bytes: 1 << 20,
        data_length: 1 << 20,
        first_cluster: 0xFFFF_FE00,
        deleted: true,
        is_dir: false,
        contiguous: true,
        quality: RecoverQuality::MaybeDamaged,
        ext: "bin".into(),
    };
    let bytes = read_file(&dev, &e).unwrap();
    assert!(bytes.is_empty(), "界截断：确定性为空");
}

#[test]
fn truncated_device_returns_read_prefix() {
    let data: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "A.BIN", &data)
        .build();
    // 截断到簇 6 全 + 簇 7 前 200 字节（文件簇 6,7,8 起于 heap+4*4096）
    let cut = 32 * 512 + 4 * 4096 + 4096 + 200;
    let truncated = image[..cut].to_vec();
    let (_f, dev) = dev_for(&truncated);
    // 根目录在簇 5（完整）→ 可扫出条目
    let e = scan(&dev)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "A.BIN")
        .unwrap();
    let bytes = read_file(&dev, &e).unwrap();
    assert_eq!(bytes.len(), 4096 + 200);
    assert_eq!(bytes, data[..4096 + 200]);
}

#[test]
fn polluted_lengths_read_file_stays_bounded_without_panic() {
    // qual-t6 C1 / 探针 D 三档（每档各钉一种旧式崩法，重算还原校验以过 T4 门槛）：
    // (a) DL=u64::MAX → debug 乘法溢出 panic；(b) VDL=DL=(1<<52)+4096 → release 巨分配 abort；
    // (c) VDL=u64::MAX（直构）→ 越 DL 交付。三档均须 Ok 且长度受 min(VDL,DL)/可达簇数约束
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "PHOTO.JPG", &[1u8; 9000])
        .delete("/", "PHOTO.JPG")
        .build();
    let mut patched = image.clone();
    let stream = SET + 32;
    patched[stream + 24..stream + 32].copy_from_slice(&u64::MAX.to_le_bytes()); // DataLength
    refix_deleted_checksum(&mut patched, SET, 3);
    let (_f, dev) = dev_for(&patched);
    let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
    assert_eq!(e.quality, RecoverQuality::MaybeDamaged);
    assert!(
        read_file(&dev, &e).unwrap().is_empty(),
        "need ≫ 可达簇数 → 确定性空，且不得在容量提示上乘爆"
    );

    // release 半壁：VDL=DL=(1<<52)+4096 → 旧式容量提示 min(size, need*cb) ≈ 4.5PB 巨分配，
    // release 真 SIGABRT（乘法不溢出，故 debug 档不 panic）；新式三重钳位仍须 Ok(empty)
    let mut patched2 = image.clone();
    patched2[stream + 8..stream + 16].copy_from_slice(&((1u64 << 52) + 4096).to_le_bytes());
    patched2[stream + 24..stream + 32].copy_from_slice(&((1u64 << 52) + 4096).to_le_bytes());
    refix_deleted_checksum(&mut patched2, SET, 3);
    let (_f2, dev2) = dev_for(&patched2);
    let e2 = scan(&dev2)
        .unwrap()
        .into_iter()
        .find(|e| e.deleted)
        .unwrap();
    assert!(
        read_file(&dev2, &e2).unwrap().is_empty(),
        "巨 need 不得触发巨分配"
    );

    // VDL 半壁：直构条目绕开 T4 的 vdl ≤ dl 门槛（VDL=u64::MAX、DL=9000）→ 不崩，
    // 交付按 min(VDL,DL) 封顶在 DL（多读的簇在 truncate 前就被 size 截住）
    let data: Vec<u8> = (0..9000u32).map(|i| (i % 197) as u8).collect();
    let image3 = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "V.BIN", &data)
        .build();
    let (_f3, dev3) = dev_for(&image3);
    let base = scan(&dev3)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "V.BIN")
        .unwrap();
    let poisoned = ExfatEntry {
        size_bytes: u64::MAX,
        ..base
    };
    let bytes = read_file(&dev3, &poisoned).unwrap();
    assert_eq!(bytes, data, "VDL 无上界 → 仍只交付 min(VDL,DL)=DL");
}

#[test]
fn deleted_with_unreadable_bitmap_uses_stale_chain() {
    // 位图不可读（0x81 首簇越界）→ 无从证伪 → 仍沿 stale 链交付全量（钉 `None => true` 分支）。
    // 构型刻意碎片化 [6,9,7]（物理序 ≠ 连续序）：add_file_chained 物理连续时退 contiguous 兜底
    // 字节相同，M4 变异（None => true → false）会逃逸（qual-t6 复审 Minor A）
    let data: Vec<u8> = (0..9000u32).map(|i| (i % 227) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file_in_clusters("/", "G.BIN", &data, &[6, 9, 7], false)
        .delete("/", "G.BIN")
        .build();
    let mut patched = image.clone();
    patched[ROOT_B + 32 + 20..ROOT_B + 32 + 24].copy_from_slice(&9999u32.to_le_bytes());
    let (_f, dev) = dev_for(&patched);
    let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
    assert_eq!(e.quality, RecoverQuality::MaybeDamaged, "位图不可读 → 封顶");
    assert_eq!(
        read_file(&dev, &e).unwrap(),
        data,
        "位图不可读 → 无从证伪 → stale 链照用"
    );
}

#[test]
fn vdl_zero_with_dl_positive_returns_empty() {
    // VDL=0、DL=9000（夹具 vdl=9000 需重算校验）→ 交付空，绝不下探 [VDL,DL)
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_file("/", "Z.BIN", &[3u8; 9000])
        .delete("/", "Z.BIN")
        .build();
    let mut patched = image.clone();
    let stream = SET + 32;
    patched[stream + 8..stream + 16].copy_from_slice(&0u64.to_le_bytes()); // ValidDataLength=0
    refix_deleted_checksum(&mut patched, SET, 3);
    let (_f, dev) = dev_for(&patched);
    let e = scan(&dev).unwrap().into_iter().find(|e| e.deleted).unwrap();
    assert_eq!(e.size_bytes, 0);
    assert_eq!(e.data_length, 9000);
    assert!(read_file(&dev, &e).unwrap().is_empty(), "VDL=0 → 空交付");
}

#[test]
fn pick_bitmap_prefers_active_and_falls_back() {
    // 纯函数级：texFAT 双位图选择（随 pick_bitmap 迁入本模块）
    let both = vec![(false, 2, 32u64), (true, 40, 32u64)];
    assert_eq!(pick_bitmap(&both, 1), Some((40, 32)));
    assert_eq!(pick_bitmap(&both, 0), Some((2, 32)));
    let only_first = vec![(false, 2, 32u64)];
    assert_eq!(
        pick_bitmap(&only_first, 1),
        Some((2, 32)),
        "缺失活动位图时回退任一可用"
    );
    assert_eq!(pick_bitmap(&[], 0), None);
}
