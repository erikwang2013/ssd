#!/usr/bin/env bash
# 生成确定性测试镜像：默认 1 MiB，字节模式 (i*7+13) mod 256。
set -euo pipefail
out="${1:-$(dirname "$0")/test.img}"
size="${2:-1048576}"
python3 - "$out" "$size" <<'PY'
import sys
out, size = sys.argv[1], int(sys.argv[2])
with open(out, "wb") as f:
    f.write(bytes((i * 7 + 13) % 256 for i in range(size)))
PY
echo "wrote ${out} (${size} bytes)"
