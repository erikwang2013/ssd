#!/usr/bin/env bash
# © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
# 容器内 deb 装/卸验证：无 docker 静默跳过。
set -euo pipefail
cd "$(dirname "$0")/.."

command -v docker >/dev/null 2>&1 || { echo "skip: 无 docker"; exit 0; }
bash scripts/build-deb.sh
deb=$(ls -t dist/xiaodun_*_amd64.deb | sed -n 1p)

docker run --rm -v "$PWD/dist:/pkg:ro" ubuntu:24.04 bash -c '
  set -e
  # 注意：勿加 pipefail——三条 dpkg -L 管道是已实测安全（0/200）的短输出；dpkg-deb -I 那条才是不安全的（12%），已在 build-deb 里 || true
  apt-get update -qq && apt-get install -y -qq udev policykit-1 >/dev/null
  dpkg -i /pkg/'"$(basename "$deb")"' || apt-get -f install -y -qq
  dpkg-query -W -f='"'"'${Status}\n'"'"' xiaodun | grep -q '"'"'install ok installed'"'"'
  dpkg -L xiaodun | grep -qx /usr/libexec/xiaodun/xd-daemon
  dpkg -L xiaodun | grep -qx /usr/share/polkit-1/actions/com.erik.xiaodun.policy
  dpkg -L xiaodun | grep -qx /usr/lib/udev/rules.d/71-xiaodun-uaccess.rules
  /usr/libexec/xiaodun/xd-daemon </dev/null >/dev/null 2>/tmp/banner || true
  grep -q "小盾 xd-daemon" /tmp/banner
  dpkg -r xiaodun
  test ! -e /usr/libexec/xiaodun/xd-daemon
  echo "DEB E2E OK"
'
