// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
use super::*;
use crate::testutil::dev_for;

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
    assert_eq!(gone.size_bytes, 700);
    assert_eq!(gone.quality, RecoverQuality::Complete); // 全簇空闲
}

#[test]
fn deleted_name_loses_first_char_or_uses_lfn() {
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_file("/", "GONE.JPG", &[1u8; 300])
        .delete("/", "GONE.JPG")
        .build();
    let (_f, dev) = dev_for(&image);
    let entries = scan(&dev).unwrap();
    let d = entries.iter().find(|e| e.deleted).unwrap();
    assert!(
        d.name.starts_with('?'),
        "expected '?ONE.JPG', got {}",
        d.name
    );
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
    assert!(entries.iter().any(|e| e.name == "PHOTOS" && e.is_dir));
}

#[test]
fn scans_fat32_and_fat12_images() {
    for mut builder in [
        xd_fixtures::FatImageBuilder::fat32(),
        xd_fixtures::FatImageBuilder::fat12(),
    ] {
        let image = builder.add_file("/", "K.TXT", b"ok").build();
        let (_f, dev) = dev_for(&image);
        let entries = scan(&dev).unwrap();
        assert!(
            entries.iter().any(|e| e.name == "K.TXT"),
            "scan failed for {:?}",
            entries
        );
    }
}

#[test]
fn skips_volume_label_entries() {
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_file("/", "REAL.TXT", b"x")
        .build();
    let mut patched = image.clone();
    let slot = 18 * 512; // FAT16 根目录第 1 槽
    patched[slot + 11] = 0x08; // 把该条目改成卷标属性
    let (_f, dev) = dev_for(&patched);
    let entries = scan(&dev).unwrap();
    assert!(entries.is_empty(), "卷标不应出现在结果中: {entries:?}");
}

#[test]
fn bad_subdir_chain_does_not_abort_scan() {
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_subdir("/", "PHOTOS")
        .add_file("/PHOTOS", "IMG.JPG", &[3u8; 100])
        .add_file("/", "ROOT.TXT", b"root")
        .build();
    let mut patched = image.clone();
    let base = 18 * 512; // FAT16 根目录区起点
    let pos = patched[base..base + 32 * 8]
        .chunks(32)
        .position(|c| &c[..5] == b"PHOTO")
        .unwrap();
    patched[base + pos * 32 + 26..base + pos * 32 + 28].copy_from_slice(&0xFFFFu16.to_le_bytes());
    let (_f, dev) = dev_for(&patched);
    let entries = scan(&dev).unwrap();
    assert!(
        entries.iter().any(|e| e.name == "ROOT.TXT"),
        "坏子目录链不得中止全盘"
    );
    assert!(
        entries.iter().any(|e| e.name == "PHOTOS"),
        "坏目录项本身仍应列出"
    );
    assert!(
        !entries.iter().any(|e| e.name == "IMG.JPG"),
        "不可读目录不得产出条目"
    );
}

#[test]
fn scan_errors_when_fat32_root_cluster_out_of_range() {
    // I2：根不可枚举必须 Err（与空盘 Ok([]) 区分）
    let image = xd_fixtures::FatImageBuilder::fat32()
        .add_file("/", "A.TXT", b"x")
        .build();
    let mut patched = image.clone();
    patched[44..48].copy_from_slice(&1_000_000u32.to_le_bytes()); // bpb 只验 ≥2
    let (_f, dev) = dev_for(&patched);
    assert!(matches!(scan(&dev), Err(FatError::InvalidBpb(m)) if m.contains("根目录不可读")));
}

#[test]
fn scan_errors_when_root_region_unreadable() {
    // I2：截断到 boot+FAT 区内（root_start=9216 在设备外）→ 首读 0 字节 → Err
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_file("/", "A.TXT", b"x")
        .build();
    let truncated = image[..5120].to_vec();
    let (_f, dev) = dev_for(&truncated);
    assert!(matches!(scan(&dev), Err(FatError::InvalidBpb(m)) if m.contains("根目录不可读")));
}

#[test]
fn unreadable_subdir_marks_entry_maybe_damaged() {
    // Rec3：子目录不可枚举 → 已 push 的目录条目标记 MaybeDamaged，其余照常
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_subdir("/", "PHOTOS")
        .add_file("/PHOTOS", "IMG.JPG", &[3u8; 100])
        .add_file("/", "ROOT.TXT", b"root")
        .build();
    let mut patched = image.clone();
    let base = 18 * 512;
    let pos = patched[base..base + 32 * 8]
        .chunks(32)
        .position(|c| &c[..5] == b"PHOTO")
        .unwrap();
    patched[base + pos * 32 + 26..base + pos * 32 + 28].copy_from_slice(&5000u16.to_le_bytes()); // 5000 > data_cluster_count()+1
    let (_f, dev) = dev_for(&patched);
    let entries = scan(&dev).unwrap();
    let dir = entries.iter().find(|e| e.name == "PHOTOS").unwrap();
    assert!(dir.is_dir);
    assert_eq!(dir.quality, RecoverQuality::MaybeDamaged);
    assert!(
        entries
            .iter()
            .any(|e| e.name == "ROOT.TXT" && e.quality == RecoverQuality::Complete)
    );
    assert!(!entries.iter().any(|e| e.name == "IMG.JPG"));
}

#[test]
fn deleted_entry_without_cluster_info_grades_maybe_damaged() {
    // M3：first_cluster < 2（簇信息缺失）→ MaybeDamaged
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_file("/", "GONE.BIN", &[4u8; 512])
        .delete("/", "GONE.BIN")
        .build();
    let mut patched = image.clone();
    let de = patched
        .windows(32)
        .position(|w| w[0] == 0xE5 && &w[8..11] == b"BIN")
        .unwrap();
    patched[de + 26..de + 28].copy_from_slice(&0u16.to_le_bytes());
    let (_f, dev) = dev_for(&patched);
    let entries = scan(&dev).unwrap();
    let gone = entries.iter().find(|e| e.deleted).unwrap();
    assert_eq!(gone.quality, RecoverQuality::MaybeDamaged);
}

#[test]
fn deleted_entry_with_out_of_range_cluster_grades_maybe_damaged() {
    // M3：first_cluster 越界（> count+1）且 size > 0 → MaybeDamaged
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_file("/", "GONE.BIN", &[4u8; 512])
        .delete("/", "GONE.BIN")
        .build();
    let mut patched = image.clone();
    let de = patched
        .windows(32)
        .position(|w| w[0] == 0xE5 && &w[8..11] == b"BIN")
        .unwrap();
    patched[de + 26..de + 28].copy_from_slice(&5000u16.to_le_bytes());
    let (_f, dev) = dev_for(&patched);
    let entries = scan(&dev).unwrap();
    let gone = entries.iter().find(|e| e.deleted).unwrap();
    assert_eq!(gone.quality, RecoverQuality::MaybeDamaged);
}

#[test]
fn ext_is_lowercase_suffix_or_empty() {
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_file("/", "A.JPG", b"x")
        .add_file("/", "NOEXT", b"y")
        .build();
    let (_f, dev) = dev_for(&image);
    let entries = scan(&dev).unwrap();
    assert_eq!(
        entries.iter().find(|e| e.name == "A.JPG").unwrap().ext,
        "jpg"
    );
    assert_eq!(entries.iter().find(|e| e.name == "NOEXT").unwrap().ext, "");
}

#[test]
fn observer_streams_post_order_with_final_quality() {
    // 后序：子项 IN.TXT 先于目录 DIR 回调，根文件 ROOT.TXT 在 DIR 后
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_subdir("/", "DIR")
        .add_file("/DIR", "IN.TXT", b"x")
        .add_file("/", "ROOT.TXT", b"y")
        .build();
    let (_f, dev) = dev_for(&image);
    let mut seen: Vec<String> = Vec::new();
    let entries = scan_with_observer(&dev, &mut |e| seen.push(e.name.clone())).unwrap();
    assert_eq!(
        seen,
        vec!["IN.TXT", "DIR", "ROOT.TXT"],
        "后序：子项在目录前"
    );
    assert_eq!(entries.len(), 3, "返回值与整表同源");
}

#[test]
fn observer_sees_downgraded_dir_quality() {
    // 坏目录链（首簇越界，既有手法）→ observer 收到 DIR 时 quality 已是终值 MaybeDamaged
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_subdir("/", "DIR")
        .add_file("/DIR", "IN.TXT", b"x")
        .build();
    let mut patched = image.clone();
    let base = 18 * 512; // FAT16 根目录区起点
    let pos = patched[base..base + 32 * 8]
        .chunks(32)
        .position(|c| &c[..3] == b"DIR")
        .unwrap();
    patched[base + pos * 32 + 26..base + pos * 32 + 28].copy_from_slice(&5000u16.to_le_bytes()); // > data_cluster_count()+1
    let (_f, dev) = dev_for(&patched);
    let mut seen: Vec<(String, RecoverQuality)> = Vec::new();
    scan_with_observer(&dev, &mut |e| seen.push((e.name.clone(), e.quality))).unwrap();
    let dir = seen.iter().find(|(n, _)| n == "DIR").unwrap();
    assert_eq!(dir.1, RecoverQuality::MaybeDamaged, "流式层拿到终值分级");
}

#[test]
fn observer_stream_matches_table_with_volume_label() {
    let image = xd_fixtures::FatImageBuilder::fat16()
        .add_file("/", "REAL.TXT", b"x")
        .add_file("/", "KEEP.TXT", b"y")
        .build();
    let mut patched = image.clone();
    patched[18 * 512 + 11] = 0x08; // 首槽改卷标属性（既有手法）
    let (_f, dev) = dev_for(&patched);
    let mut n = 0usize;
    let entries = scan_with_observer(&dev, &mut |_| n += 1).unwrap();
    assert_eq!(entries.len(), 1, "KEEP.TXT 在列、卷标不在");
    assert_eq!(
        n,
        entries.len(),
        "回调条数须与返回表长一致（卷标不得进回调流）"
    );
}
