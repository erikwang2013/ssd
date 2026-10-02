// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 生成一张含"已删除照片"的 FAT16 镜像到文件：
//   cargo run -p xd-fixtures --example gen_fat_image -- /tmp/xd-fat.img
fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/xd-fat.img".into());
    let photo: Vec<u8> = (0..65_536u32).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let mut image = xd_fixtures::FatImageBuilder::fat16()
        .add_subdir("/", "DCIM")
        .add_file("/DCIM", "IMG_0001.JPG", &photo)
        .delete("/DCIM", "IMG_0001.JPG")
        .build();
    // M1c 深扫雕刻可见性（e2e-loop.sh 的 deep 段）：FAT 删除只清链、照片数据仍留盘——在
    // 释放出的首簇埋一枚结构完整 JPEG。布局：reserved 1 + FAT 17 + 根目录 32 → 数据区自
    // 扇区 50；DCIM 占簇 2，照片占簇 3..=130 → 埋点 = 簇 3 起点 = (50+1)*512 = 26112，
    // 深扫必须雕出它且 byteOffset 精确（夹具几何漂移会在环回脚本里响亮失败）。
    let jpeg = xd_fixtures::mini_jpeg(2000);
    xd_fixtures::plant_in_run(&mut image, 26112, &jpeg);
    std::fs::write(&path, &image).unwrap();
    println!("wrote {} ({} bytes)", path, image.len());
}
