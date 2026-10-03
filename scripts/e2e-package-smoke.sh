#!/usr/bin/env bash
# © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
# 产物冒烟（macOS）：解 zip → 布局/清单断言（引擎与主程序同目录 ⇒ UI 自动发现 daemon）→
# 以 --image 起 xd-daemon → 管道 ping → 断言 pong。
# 明文不用 --listen/--port-file/--owner-pid：提权会话链路由 T1/T4 测试覆盖（计划 T5）。
set -euo pipefail
cd "$(dirname "$0")/.."

zip="${1:-}"
if [ -z "$zip" ]; then
  zip=$(ls -t dist/xiaodun-v*-macos-*.zip 2>/dev/null | sed -n 1p || true)
fi
[ -n "$zip" ] && [ -f "$zip" ] || { echo "FAIL: 未找到 zip（先跑 scripts/package-macos.sh）"; exit 1; }
echo "smoke: $zip"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
ditto -x -k "$zip" "$tmp"

app=$(ls -d "$tmp"/xiaodun-v*-macos-*/xiaodun_ui.app 2>/dev/null | sed -n 1p || true)
[ -n "$app" ] || { echo "FAIL: zip 内未找到 xiaodun_ui.app"; exit 1; }
[ -x "$app/Contents/MacOS/xiaodun_ui" ] || { echo "FAIL: 缺主程序 Contents/MacOS/xiaodun_ui"; exit 1; }
[ -x "$app/Contents/MacOS/xd-daemon" ] || { echo "FAIL: 缺引擎 Contents/MacOS/xd-daemon（打包布局约定）"; exit 1; }

# 签名状态如实打印（T10）：签/未签都通过，但留证据（未签名包 codesign -dv 非零退出属预期；
# 公证/装订状态以 package-macos.sh 的 notarize 输出为准）。
codesign -dv "$app" 2>&1 || echo "note: codesign -dv 失败（未签名包属预期）——本产物未签名/未公证"

img=$tmp/test.img
dd if=/dev/zero of="$img" bs=1024 count=64 2>/dev/null

out=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"ping","params":null}' \
  | "$app/Contents/MacOS/xd-daemon" --image "$img") || { echo "FAIL: xd-daemon exit code $?"; exit 1; }
echo "$out"
echo "$out" | grep -q '"pong":true' || { echo "FAIL: ping 未收到 pong"; exit 1; }
echo "PACKAGE SMOKE OK (macOS)"
