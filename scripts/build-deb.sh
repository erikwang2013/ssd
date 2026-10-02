#!/usr/bin/env bash
# © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
# 组装 deb：/usr/libexec/xiaodun/xd-daemon + polkit + udev + （Flutter bundle 到位后）opt/xiaodun。
# 版本号从 workspace Cargo.toml 注入，避免两处手改。
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
[ -n "$version" ] || { echo "FAIL: 未取到版本号"; exit 1; }
echo "packaging xiaodun v$version"

# XML 门禁（qual-m1e-t3：模板文本必须过真实 parser——曾因注释体含 -- 致 polkitd 拒载）
command -v xmllint >/dev/null || { echo "FAIL: 需 libxml2-utils（XML 门禁）"; exit 1; }
xmllint --noout packaging/polkit/*.policy

cargo build -q --locked --release -p xd-daemon

stage=$(mktemp -d /tmp/xd-deb-$$-XXXX)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/DEBIAN" "$stage/usr/libexec/xiaodun" \
         "$stage/usr/share/polkit-1/actions" "$stage/usr/lib/udev/rules.d"
cp packaging/deb/DEBIAN/control "$stage/DEBIAN/control"
sed -i "s/@VERSION@/$version/" "$stage/DEBIAN/control"
cp packaging/deb/DEBIAN/postinst "$stage/DEBIAN/postinst"
chmod 755 "$stage/DEBIAN/postinst"
install -m 755 target/release/xd-daemon "$stage/usr/libexec/xiaodun/xd-daemon"
install -m 644 packaging/polkit/com.erik.xiaodun.policy "$stage/usr/share/polkit-1/actions/"
install -m 644 packaging/udev/71-xiaodun-uaccess.rules "$stage/usr/lib/udev/rules.d/"

out=dist/xiaodun_${version}_amd64.deb
mkdir -p dist
dpkg-deb --build --root-owner-group "$stage" "$out"
echo "built: $out"
dpkg-deb -I "$out" | head -12 || true   # pipefail 下 dpkg-deb 第 13 行的 SIGPIPE 竞态（实测并行 3/10；|| true 后 0/60）
