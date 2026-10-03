#!/usr/bin/env bash
# © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
# 签名/公证占位（M1e-tail T5 留的钩子；M2 实现）：
#   1. codesign --force --options runtime --timestamp 逐嵌套可执行——先 Contents/MacOS/xd-daemon，
#      再 Frameworks/*.framework，最后整个 .app（顺序错会让外层签名被内层破坏）；
#   2. xcrun notarytool submit --wait（Apple 凭据走 CI secrets，勿入仓）；
#   3. xcrun stapler staple。
# 现状：直接成功返回（包未签名）——见 README「打包」与 docs/security §11。
set -euo pipefail

zip="${1:-<未指定>}"
echo "skip: 签名/公证未实现（归 M2 占位）-> $zip"
