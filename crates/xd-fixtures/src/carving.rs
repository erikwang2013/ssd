// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 雕刻夹具：规范结构合成 JPEG/PNG（供走链器测试）+ 自由区间埋数据。

/// 与 `xd-carving::crc32` **同源标准的第二份实现**（位算法，非表拷贝）：
/// 夹具不得依赖被测引擎 crate（引擎 → fixtures 会成环），且夹具与被测实现
/// 共用同一份代码会让 CRC 校验测试自证。两副本由 xd-carving 侧
/// `crc32_matches_fixtures_copy` 对照测试钉住一致。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// PNG chunk：长度（大端，不含自身）+ 类型 + 数据 + CRC（对 类型+数据 计算）。
/// （pub 供 xd-carving 的 png 测试构造非常规块：非空 IEND/超限长度字段等。）
pub fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(ty);
    out.extend_from_slice(data);
    // 量小无妨：ty+data 拼接后一次算，不引增量接口
    let mut crc_in = Vec::with_capacity(4 + data.len());
    crc_in.extend_from_slice(ty);
    crc_in.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_in).to_be_bytes());
}

/// 结构规范的最小 JPEG（SOI/APP0/SOF0/SOS/熵段/EOI；熵段长 entropy_len，纯 0xAA 无 FF）。
pub fn mini_jpeg(entropy_len: usize) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8]; // SOI
    v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]); // APP0, len=16
    v.extend_from_slice(b"JFIF\0");
    v.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    v.extend_from_slice(&[
        0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00,
    ]); // SOF0
    v.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]); // SOS
    v.extend(std::iter::repeat_n(0xAAu8, entropy_len)); // 熵段（无 0xFF）
    v.extend_from_slice(&[0xFF, 0xD9]); // EOI
    v
}

/// 规范结构的最小 PNG（IHDR + 一个 IDAT + IEND，全部 CRC 正确）。
/// IHDR：宽 1 高 1 位深 8 色型 6；IDAT 为传入 payload（可为任意字节，不需要真实
/// zlib——走链器不解释 IDAT）；IEND 空。
pub fn mini_png(payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    chunk(&mut v, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    chunk(&mut v, b"IDAT", payload);
    chunk(&mut v, b"IEND", &[]);
    v
}

/// 70B 已知真实 1×1 PNG 资产（三 chunk CRC 均正确）。
pub const TINY_PNG: &[u8] = include_bytes!("../assets/tiny.png");

/// 把 `data` 写进镜像的绝对字节 `offset`（测试专用：调用方负责选空闲簇区间内的偏移）。
pub fn plant_in_run(img: &mut [u8], offset: u64, data: &[u8]) {
    let o = offset as usize;
    img[o..o + data.len()].copy_from_slice(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mini_fixtures_are_wellformed() {
        // mini_jpeg：SOI 开头 EOI 结尾、长度自洽；mini_png：magic 开头 IEND 结尾，资产同构
        let j = mini_jpeg(100);
        assert_eq!(
            (j[0], j[1], j[j.len() - 2], j[j.len() - 1]),
            (0xFF, 0xD8, 0xFF, 0xD9)
        );
        assert_eq!(&j[2..6], &[0xFF, 0xE0, 0x00, 0x10], "APP0 长度字段=16");
        // 定长 45 = SOI 2 + APP0 18 + SOF0 13 + SOS 10 + EOI 2（更深走链归 T4）
        assert_eq!(j.len(), 45 + 100, "总长 = 45 + entropy_len");
        let p = mini_png(b"abc");
        assert_eq!(&p[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(
            &p[p.len() - 12..p.len() - 8],
            &[0, 0, 0, 0],
            "IEND 长度字段为 0"
        );
        assert_eq!(&p[p.len() - 8..p.len() - 4], b"IEND");
        assert_eq!(
            &p[p.len() - 4..],
            &[0xAE, 0x42, 0x60, 0x82],
            "IEND CRC 已知向量"
        );
        // 资产 8 magic + 25 IHDR + 25 IDAT + 12 IEND（计划写 68，实算为 70）
        assert_eq!(TINY_PNG.len(), 70);
        assert_eq!(
            &TINY_PNG[TINY_PNG.len() - 4..],
            &[0xAE, 0x42, 0x60, 0x82],
            "资产尾即 IEND 空 chunk 的已知 CRC"
        );
    }
}
