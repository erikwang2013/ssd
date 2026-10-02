// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 移动端 FFI 占位（M4 接入 flutter_rust_bridge）。

pub fn core_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_workspace() {
        // 与 workspace 版本同源：core_version() 必须走 env!（手写死串会在发版时戳穿——v0.2.0 修正）
        assert_eq!(super::core_version(), env!("CARGO_PKG_VERSION"));
        assert!(!super::core_version().is_empty());
    }
}
