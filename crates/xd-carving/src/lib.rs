// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 文件雕刻（carving v1：JPEG/PNG）：在未分配字节区间上做签名识别 + **头部结构验证** +
//! 结构走链重组，产出无文件名结果（byteOffset/size/complete）。
//!
//! 裁定（v1）：候选文件**只在其所在 run 内重组**——跨 run 拼接会跨过已分配区把他人数据
//! 缝进来，顺序不可验证，违反"宁可漏报不可错报"；坏读/区间尽头即诚实截断；同一候选内嵌的
//! 容器（EXIF 缩略图）不重复上报。
//! 说明：设计 §4.3 的"多线程并行块扫描"延后（雕刻 I/O 受限，顺序读已达 §4.6 底线；
//! 并行收益须实测后按需加，届时并行的是"多个 run 的读"而非签名匹配）。

mod carver;
pub mod crc32;
mod jpeg;
mod png;
pub mod signatures;

pub use carver::{
    CHUNK_BYTES, CarveEvent, CarveStats, CarvedEntry, MAX_FILE_BYTES, carve_runs, carve_runs_from,
};
pub use jpeg::carve_jpeg;
pub use png::carve_png;
pub use signatures::{Carved, Cursor};
