// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 生成 T9 集成测试标准镜像（UI 客户端 ↔ 真 daemon 全链路）：
//   cargo run -p xd-fixtures --example make_carve_fixture -- <path>
//
// 埋点（FAT16，2.1MiB，几何与 gen_fat_image 同族）：
//   live    /DCIM/IMG_0001.JPG  活文件（结构完整 JPEG，4141B = 9 簇）——预览/导出须逐字节相等
//   deleted /DCIM/IMG_0002.JPG  已删（0xE5 目录项，65536B 非签名模式字节 = 128 簇）——回读须逐字节相等
//   carved  未分配簇（见 CARVE_OFFSET）埋 mini_jpeg(2000)（2045B）——深扫须雕出且回读一致
//   big     /BIG.TXT            目录项**声明** >64MiB（真实数据 3B；声明与数据不一致的坏卡形态）
//           ——fs.read 按声明 size 在读取前拒绝（-32009），无需 65MiB 镜像
// 侧车（期望字节，测试逐字节比对）：<path>.live.bin / <path>.deleted.bin / <path>.carved.bin
//
// 簇序（builder 最低空闲优先）：簇 2 = DCIM；3..11 = live；12..139 = deleted；140 = big；
// 埋点取 never-allocated 簇 141 → offset (50 + (141-2)) * 512 = 96768（不在删除件区间内，
// 保删除件回读字节原封）。deleted 的簇在 delete() 之后无后续 add → 保持全空闲（FAT=0）。
fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/xd-carve.img".into());
    let live = xd_fixtures::mini_jpeg(4096);
    // 与 gen_fat_image 同款模式字节：周期 256 覆盖全字节值，但相邻字节恒 +7 → 不含
    // FFD8/89PNG 签名（雕刻扫描不会误报第二枚）。
    let deleted: Vec<u8> = (0..65_536u32).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let mut image = xd_fixtures::FatImageBuilder::fat16()
        .add_subdir("/", "DCIM")
        .add_file("/DCIM", "IMG_0001.JPG", &live)
        .add_file("/DCIM", "IMG_0002.JPG", &deleted)
        .add_file("/", "BIG.TXT", b"big")
        .delete("/DCIM", "IMG_0002.JPG")
        .build();

    // BIG.TXT 声明放大到 64MiB+1（0x0400_0001 之上取整：68157441 = 65MiB+1）：模拟坏卡
    // 「目录项 size 与数据不符」，专供 -32009 边界。定位限定在 FAT16 根目录区（扇区 18 起
    // 512 项）内，避免全镜像搜索歧义。
    const ROOT_DIR_OFF: usize = 18 * 512;
    const ROOT_DIR_LEN: usize = 512 * 32;
    const BIG_DECLARED: u32 = 68_157_441; // > 64MiB = 67_108_864（fs.read 契约拒绝线）
    let root = &image[ROOT_DIR_OFF..ROOT_DIR_OFF + ROOT_DIR_LEN];
    let off = ROOT_DIR_OFF
        + root
            .windows(32)
            .position(|w| &w[..11] == b"BIG     TXT")
            .expect("BIG.TXT 目录项未找到（几何漂移？）");
    image[off + 28..off + 32].copy_from_slice(&BIG_DECLARED.to_le_bytes());

    const CARVE_OFFSET: u64 = 96_768; // 簇 141 起点
    let carved = xd_fixtures::mini_jpeg(2000);
    xd_fixtures::plant_in_run(&mut image, CARVE_OFFSET, &carved);

    std::fs::write(&path, &image).unwrap();
    std::fs::write(format!("{path}.live.bin"), &live).unwrap();
    std::fs::write(format!("{path}.deleted.bin"), &deleted).unwrap();
    std::fs::write(format!("{path}.carved.bin"), &carved).unwrap();
    println!(
        "wrote {path} ({} bytes; live {}B, deleted {}B, carved {}B @ {CARVE_OFFSET})",
        image.len(),
        live.len(),
        deleted.len(),
        carved.len()
    );
}
