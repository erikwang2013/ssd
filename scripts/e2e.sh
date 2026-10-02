#!/usr/bin/env bash
# © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
# M0 端到端冒烟：生成镜像 → 走 stdio 发 ping + device.list → 断言。
set -euo pipefail
cd "$(dirname "$0")/.."

bash fixtures/gen_image.sh /tmp/xiaodun-m0-test.img 65536 >/dev/null

out=$(printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"ping","params":null}' \
  '{"jsonrpc":"2.0","id":2,"method":"device.list","params":null}' \
  | cargo run -q --locked -p xd-daemon -- --image /tmp/xiaodun-m0-test.img)

echo "$out"
echo "$out" | grep -q '"pong":true'  || { echo "FAIL: ping"; exit 1; }
echo "$out" | grep -q '"sizeBytes":65536' || { echo "FAIL: device.list size"; exit 1; }
echo "$out" | grep -q '"kind":"image"' || { echo "FAIL: device kind"; exit 1; }
echo "E2E OK"
