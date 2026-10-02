// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
// 生成一张含"已删除照片"的 exFAT 镜像（1 MiB）：
//   cargo run -p xd-fixtures --example gen_exfat_image -- /tmp/xd-exfat.img
fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/xd-exfat.img".into());
    let photo: Vec<u8> = (0..65_536u32).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let image = xd_fixtures::ExfatImageBuilder::new()
        .add_subdir("/", "DCIM")
        .add_file("/DCIM", "IMG_0001.JPG", &photo)
        .delete("/DCIM", "IMG_0001.JPG")
        .build();
    std::fs::write(&path, &image).unwrap();
    println!("wrote {} ({} bytes)", path, image.len());
}
