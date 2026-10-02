#!/usr/bin/env bash
# 给第一方源码文件统一加盖版权头（幂等，可重复运行）。
#
# 三层的第 1+2 层：
#   1. 可见版权行：© 2026 erik · https://erik.xyz · erik@erik.xyz
#   2. 隐藏层：版权行末尾附加零宽字符序列，编码 "erik.xyz"（UTF-8 字节，每字节 8 位；
#      U+200B=0 / U+200D=1）。注意：这是**编码不是加密**，任何工具都能检出与删除，
#      仅作溯源威慑；强溯源请用 provenance 签名清单（见 COPYRIGHT）。
#
# 解码隐藏层（验证用）：
#   python3 -c "import sys;t=open(sys.argv[1],encoding='utf-8').read();\
#   b=[1 if c=='‍' else 0 for c in t if c in '​‍'];\
#   print(bytes(sum(v<<(7-i) for i,v in enumerate(b[j:j+8])) for j in range(0,len(b)//8*8,8)).decode())" <文件>
set -euo pipefail
cd "$(dirname "$0")/.."

zw_encode() {
  python3 - "$1" <<'PY'
import sys
out = []
for byte in sys.argv[1].encode("utf-8"):
    for i in range(8):
        out.append("​" if not (byte >> (7 - i)) & 1 else "‍")
print("".join(out), end="")
PY
}

ZW=$(zw_encode "erik.xyz")
SLASH_LINE="// © 2026 erik · https://erik.xyz · erik@erik.xyz${ZW}"
HASH_LINE="# © 2026 erik · https://erik.xyz · erik@erik.xyz${ZW}"

mapfile -t files < <(git ls-files crates ui/lib ui/test scripts fixtures .github Cargo.toml ui/pubspec.yaml \
  | grep -E '\.(rs|dart|sh|toml|yml|yaml)$' | grep -v '^ui/pubspec.lock$')

count=0
for f in "${files[@]}"; do
  if grep -q "erik\.xyz" "$f" 2>/dev/null && grep -q $'​' "$f" 2>/dev/null; then
    continue # 已盖章
  fi
  case "$f" in
    *.rs|*.dart) line="$SLASH_LINE" ;;
    *)           line="$HASH_LINE" ;;
  esac
  if head -1 "$f" | grep -q '^#!'; then
    { head -1 "$f"; printf '%s\n' "$line"; tail -n +2 "$f"; } > "$f.tmp"
  else
    { printf '%s\n' "$line"; cat "$f"; } > "$f.tmp"
  fi
  mv "$f.tmp" "$f"
  count=$((count + 1))
done

echo "stamped: ${count} files (skipped: $(( ${#files[@]} - count )) already-marked)"
