#!/usr/bin/env bash
# © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
# 真块设备端到端（Linux）：镜像 → 环回只读设备 → LinuxBlockDevice 字节级 + daemon device.list
# + M1b 真扫描全链路（scan.start → scan.finished → scan.results）。
# 无免密 sudo（本机日常）自动跳过；GitHub ubuntu-latest 免密 sudo → 真跑。
set -euo pipefail
cd "$(dirname "$0")/.."

sudo -n true 2>/dev/null || { echo "skip: 无免密 sudo（真块设备 e2e 需 root 建环回）"; exit 0; }

img=$(mktemp /tmp/xd-loop-$$-XXXX.img)
tmpdb=$(mktemp /tmp/xd-loop-db-XXXXXX)
loop=""
rm -f "$tmpdb"   # 让 sqlite 自建；步骤 2/3 共用——防 sudo 下写 root/真实 HOME 状态库（同增补 1 类纪律）
trap 'if [ -n "$loop" ]; then sudo losetup -d "$loop"; fi; rm -f "$img"; [ -z "$tmpdb" ] || rm -f "$tmpdb"' EXIT

cargo run -q --locked -p xd-fixtures --example gen_fat_image -- "$img"
cargo build -q --locked -p xd-daemon
before=$(sha256sum "$img" | cut -d' ' -f1)

loop=$(sudo losetup -r -f --show "$img")   # -r：内核强制只读
command -v udevadm >/dev/null && sudo udevadm settle || true   # 等 change 事件处理完再改权限，否则会被 udevd 拉回 660
sudo chmod 666 "$loop"   # CI 一次性 VM 解 EACCES；内核 -r 只读兜底，权限面不超过生产 uaccess(rw)
echo "loop=$loop (sysfs ro=$(cat "/sys/block/$(basename "$loop")/ro"))"

# 1) LinuxBlockDevice：全量字节比对 + 偏移抽样（测试内断言）
XD_LOOP_DEV="$loop" XD_LOOP_IMG="$img" cargo test -q --locked -p xd-device --test loop_e2e -- --nocapture

# 2) daemon：--device 注册 + device.list 零 open() 路径出现 unix:$loop
out=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"device.list","params":null}' \
  | sudo ./target/debug/xd-daemon --device "$loop" --db "$tmpdb")
grep -q "unix:$loop" <<<"$out" || { echo "FAIL: device.list 未含 $loop"; echo "$out"; exit 1; }

# 3) M1b：环回设备真扫描全链路（start → 读至 scan.finished → results）。
#    扫描在 daemon 的 worker 线程异步跑：必须等 finished 再问 results，单管道一次性喂入会与
#    扫描竞态 —— 用 coproc 交互（计划许可的 mkfifo/coproc 方案）。export（镜像导出）归 M1d，
#    本脚本只断言扫描侧。
coproc XD { sudo ./target/debug/xd-daemon --device "$loop" --db "$tmpdb"; }
printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":20,\"method\":\"scan.start\",\"params\":{\"device\":\"unix:$loop\",\"mode\":\"quick\"}}" >&"${XD[1]}"
scan_task=""; scan_start=""; fin_line=""
while :; do
  IFS= read -r -t 30 line <&"${XD[0]}" || { echo "FAIL: 等 scan.finished 超时/断流"; exit 1; }
  case "$line" in
    *'"id":20'*) scan_start="$line"; scan_task=$(sed -n 's/.*"taskId":\([0-9]*\).*/\1/p' <<<"$line");;
    *'"method":"scan.finished"'*) fin_line="$line"; break;;
  esac
done
grep -qF '"fs":"fat"' <<<"$scan_start" || { echo "FAIL: scan.start 未认 FAT"; echo "$scan_start"; exit 1; }
grep -qF '"state":"completed"' <<<"$fin_line" || { echo "FAIL: scan 未 completed"; echo "$fin_line"; exit 1; }
printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":21,\"method\":\"scan.results\",\"params\":{\"taskId\":$scan_task,\"offset\":0,\"limit\":100,\"deletedOnly\":false}}" >&"${XD[1]}"
while :; do
  IFS= read -r -t 30 line <&"${XD[0]}" || { echo "FAIL: 等 results 超时/断流"; exit 1; }
  case "$line" in *'"id":21'*) res_line="$line"; break;; esac
done
# 删除项首字符丢失 → 扫描器重组为 '?'（dirent::assemble_sfn_name）
grep -qF '"name":"?MG_0001.JPG"' <<<"$res_line" || { echo "FAIL: results 未含删除文件 ?MG_0001.JPG"; echo "$res_line"; exit 1; }
eval "exec ${XD[1]}>&-"   # 关 stdin → daemon 收 EOF 退出
wait "$XD_PID"
echo "scan: task $scan_task completed，删除文件 ?MG_0001.JPG 经 IPC 可见"

# 4) 只读铁律：源镜像扫描前后 sha256 不变
[ "$before" = "$(sha256sum "$img" | cut -d' ' -f1)" ] || { echo "FAIL: 源镜像被改写"; exit 1; }
echo "LOOP E2E OK"
