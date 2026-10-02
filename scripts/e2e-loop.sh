#!/usr/bin/env bash
# © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
# 真块设备端到端（Linux）：镜像 → 环回只读设备 → LinuxBlockDevice 字节级 + daemon device.list。
# 无免密 sudo（本机日常）自动跳过；GitHub ubuntu-latest 免密 sudo → 真跑。
set -euo pipefail
cd "$(dirname "$0")/.."

sudo -n true 2>/dev/null || { echo "skip: 无免密 sudo（真块设备 e2e 需 root 建环回）"; exit 0; }

img=$(mktemp /tmp/xd-loop-$$-XXXX.img)
loop=""
trap 'if [ -n "$loop" ]; then sudo losetup -d "$loop"; fi; rm -f "$img"' EXIT

cargo run -q --locked -p xd-fixtures --example gen_fat_image -- "$img"
cargo build -q --locked -p xd-daemon
before=$(sha256sum "$img" | cut -d' ' -f1)

loop=$(sudo losetup -r -f --show "$img")   # -r：内核强制只读
sudo chmod 666 "$loop"   # CI 一次性 VM 解 EACCES；内核 -r 只读兜底，权限面不超过生产 uaccess(rw)
echo "loop=$loop (sysfs ro=$(cat "/sys/block/$(basename "$loop")/ro"))"

# 1) LinuxBlockDevice：全量字节比对 + 偏移抽样（测试内断言）
XD_LOOP_DEV="$loop" XD_LOOP_IMG="$img" cargo test -q --locked -p xd-device --test loop_e2e -- --nocapture

# 2) daemon：--device 注册 + device.list 零 open() 路径出现 unix:$loop
out=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"device.list","params":null}' \
  | sudo ./target/debug/xd-daemon --device "$loop")
grep -q "unix:$loop" <<<"$out" || { echo "FAIL: device.list 未含 $loop"; echo "$out"; exit 1; }

# 3) 只读铁律：源镜像扫描前后 sha256 不变
[ "$before" = "$(sha256sum "$img" | cut -d' ' -f1)" ] || { echo "FAIL: 源镜像被改写"; exit 1; }
echo "LOOP E2E OK"
