//! 移动端 FFI 占位（M4 接入 flutter_rust_bridge）。

pub fn core_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_workspace() {
        assert_eq!(super::core_version(), "0.1.0");
    }
}
