// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! IEEE CRC32（PNG chunk 校验；表驱动，无新依赖）。

const fn build_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const TABLE: [u32; 256] = build_table();

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// 增量 CRC（PNG 分块流式校验）：先 crc32_init，逐段 update，末段 finalize。
pub struct Crc32(u32);

impl Crc32 {
    pub fn new() -> Self {
        Self(0xFFFF_FFFF)
    }
    pub fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.0 = TABLE[((self.0 ^ b as u32) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }
    pub fn finalize(self) -> u32 {
        !self.0
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"IEND"), 0xAE42_6082); // PNG IEND 空 chunk 的已知 CRC
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn incremental_matches_oneshot() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let mut inc = Crc32::new();
        inc.update(&data[..10]);
        inc.update(&data[10..]);
        assert_eq!(inc.finalize(), crc32(data));
    }

    #[test]
    fn crc32_matches_fixtures_copy() {
        // 跨副本对拍：夹具侧第二份实现独立演化时此断言必红（夹具不得依赖本 crate，
        // 否则 CRC 验证测试会拿被测实现自证）。
        for sample in [&b"123456789"[..], b"IEND", b""] {
            assert_eq!(
                crc32(sample),
                xd_fixtures::crc32(sample),
                "CRC32 两副本对拍不一致（样本 {} 字节）",
                sample.len()
            );
        }
    }
}
