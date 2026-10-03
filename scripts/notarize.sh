#!/usr/bin/env bash
# © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
# 签名/公证真链（M2 T10 实现；M1e-tail T5 占位转正）：
#   1. 逐嵌套 codesign --force --options runtime --timestamp——先 Contents/MacOS/xd-daemon，
#      再 Frameworks（内层可执行 → framework 目录），最后整个 .app；不用 --deep
#      （顺序错/--deep 都会让外层签名被内层内容破坏）；
#   2. codesign --verify --strict 断言 seal 有效（F2 验收点：App Sandbox 已移除，
#      见 ui/macos/Runner/Release.entitlements）；
#   3. xcrun notarytool submit --wait（Apple 凭据全走环境变量，勿入仓）；
#   4. xcrun stapler staple + spctl -a -vv 断言 accepted（未公证时 spctl 必然拒
#      ⇒ 「签名有效」的机器断言以第 2 步为准，此步只在真公证路径执行）。
# 凭据门控：APPLE_* 五件套缺任一 ⇒ 打印具名 `skip:` 行并 exit 0——绝不产半签包
# （跳过时不触碰 .app/.zip，产物与 M1e 未签名包行为一致）。
# 用法：notarize.sh <app 路径> [zip 路径]
#   zip = notarytool 提交用包，签名后由本脚本重出（官方顺序：sign → zip → submit → staple）；
#   staple 会再次改写 .app ⇒ 最终交付 zip 由调用方（package-macos.sh）在 staple 后重出。
# 签名身份默认从临时 keychain 里取（Developer ID Application …）；可用 APPLE_SIGN_IDENTITY 覆盖。
set -euo pipefail

app="${1:-}"
[ -n "$app" ] || { echo "FAIL: 用法 notarize.sh <app 路径> [zip 路径]" >&2; exit 2; }
zip="${2:-}"

# --- 凭据门控（先于一切签名动作：绝不产半签包）---
missing=""
for v in APPLE_CERT_P12_BASE64 APPLE_CERT_PASSWORD APPLE_TEAM_ID APPLE_ID APPLE_APP_PASSWORD; do
  [ -n "${!v:-}" ] || missing="${missing:+$missing }$v"
done
if [ -n "$missing" ]; then
  echo "skip: 签名/公证跳过——缺凭据: $missing；本产物未签名/未公证（artifact 名不变）"
  exit 0
fi

[ -n "$zip" ] || { echo "FAIL: 凭据齐备但未给 zip 路径（notarytool 提交需要）" >&2; exit 2; }
[ -d "$app" ] || { echo "FAIL: 未找到 .app 目录: $app" >&2; exit 1; }

tmp=$(mktemp -d)
kc="$tmp/xd-sign.keychain-db"
kc_pw="xd-sign-$RANDOM$RANDOM"
cleanup() { security delete-keychain "$kc" 2>/dev/null || true; rm -rf "$tmp"; }
trap cleanup EXIT

# --- 证书入临时 keychain（base64 p12；密码只经参数传递，不落日志）---
printf '%s' "$APPLE_CERT_P12_BASE64" | base64 -d > "$tmp/cert.p12"
security create-keychain -p "$kc_pw" "$kc"
security set-keychain-settings -lut 21600 "$kc"
security unlock-keychain -p "$kc_pw" "$kc"
security import "$tmp/cert.p12" -k "$kc" -P "$APPLE_CERT_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kc_pw" "$kc" >/dev/null

identity="${APPLE_SIGN_IDENTITY:-}"
if [ -z "$identity" ]; then
  identity=$(security find-identity -v -p codesigning "$kc" \
    | sed -n 's/.*) \([0-9A-Fa-f]\{40\}\) ".*/\1/p' | head -1)
fi
[ -n "$identity" ] || { echo "FAIL: keychain 内无可用的代码签名身份（Developer ID Application 证书导入异常？）" >&2; exit 1; }

sign() { codesign --force --options runtime --timestamp --keychain "$kc" --sign "$identity" "$@"; }

# --- 1. 逐嵌套签名：引擎（裸 Mach-O）→ Frameworks 内层 → Frameworks 目录 → 整个 .app ---
if [ -f "$app/Contents/MacOS/xd-daemon" ]; then
  sign "$app/Contents/MacOS/xd-daemon"
fi
if [ -d "$app/Contents/Frameworks" ]; then
  while IFS= read -r -d '' f; do sign "$f"; done \
    < <(find "$app/Contents/Frameworks" -type f \( -name '*.dylib' -o -perm -u+x \) -print0)
  while IFS= read -r -d '' d; do sign "$d"; done \
    < <(find "$app/Contents/Frameworks" -type d -name '*.framework' -print0)
fi
sign "$app"

# --- 2. seal 断言（F2 验收点）：未公证时 spctl 必然拒 ⇒ 以 codesign --verify 通过为准 ---
codesign --verify --strict --verbose=2 "$app"

# --- 3. 提交公证：签名改写了 .app ⇒ 提交前重出 zip（顶层目录 = .app 所在 stage 目录）---
ditto -c -k --keepParent "$(dirname "$app")" "$zip"
xcrun notarytool submit "$zip" --wait \
  --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD"

# --- 4. 装订票据（再次改写 .app）并断言 Gatekeeper 接受 ---
xcrun stapler staple "$app"
spctl -a -vv "$app"

echo "notarized: $app（seal 有效，公证票据已 staple；交付 zip 由调用方在 staple 后重出）"
