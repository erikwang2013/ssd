#!/usr/bin/env bash
# © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
# 组装 macOS staged zip（单架构 = 构建机架构）：xiaodun_ui.app + Contents/MacOS/xd-daemon。
# 引擎必须与主程序同目录（Contents/MacOS）：UI 按「宿主可执行文件同目录」发现 daemon
# （ui/lib/core_client/ipc_transport.dart::packagedDaemonPath）——否则提权引导退化旧支路。
# 版本号从 workspace Cargo.toml 注入（同 build-deb.sh）。签名/公证：调用 scripts/notarize.sh
# 真链（T10）；凭据缺失 ⇒ 具名 skip、产物 = 未签名包（首次打开需右键-打开，Gatekeeper）。
# 开发机（Linux）无法执行本脚本：以 CI dispatch 实证。
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
[ -n "$version" ] || { echo "FAIL: 未取到版本号"; exit 1; }

case "$(uname -m)" in
  arm64) arch=arm64 ;;
  x86_64) arch=x64 ;;
  *) echo "FAIL: 未知构建机架构 $(uname -m)（预期 arm64/x86_64）"; exit 1 ;;
esac
echo "packaging xiaodun v$version (macos-$arch)"

(cd ui && flutter build macos --release)
cargo build -q --locked --release -p xd-daemon

app=ui/build/macos/Build/Products/Release/xiaodun_ui.app
[ -d "$app" ] || { echo "FAIL: 未找到 Flutter 产物 $app"; exit 1; }

name=xiaodun-v$version-macos-$arch
stage=dist/$name
rm -rf "$stage"
mkdir -p "$stage"
ditto "$app" "$stage/xiaodun_ui.app"   # ditto：保符号链接/权限（cp -R 会跟丢 framework 链接）
install -m 755 target/release/xd-daemon "$stage/xiaodun_ui.app/Contents/MacOS/xd-daemon"

out=dist/$name.zip
rm -f "$out"

# 签名/公证真链（T10）：缺凭据 ⇒ notarize.sh 打具名 skip 行且不触碰 .app/.zip，
# 本产物即未签名交付物（命名/形状与 M1e 完全一致）。
bash scripts/notarize.sh "$stage/xiaodun_ui.app" "$out"

# staple 改写 .app ⇒ 交付 zip 在签名流程之后重出，保证包内 .app 与已装订票据一致；
# 未签名路径重出内容同形（名字不变），仅多一次 ditto。
rm -f "$out"
ditto -c -k --keepParent "$stage" "$out"   # zip 顶层 = $name/（与 Windows 包形一致）

echo "built: $out"
