#!/usr/bin/env bash
# © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
# 真块设备端到端（Linux）：镜像 → 环回只读设备 → LinuxBlockDevice 字节级 + daemon device.list
# + M1b quick 真扫描全链路 + M1c deep 深扫（恢复率门禁的真设备臂，carved 恰 1 条）
# + M1d 导出：环回挂载后导出到挂载点断言 -32006（同盘判定真集成），umount 后导出到普通目录
#   并逐字节比对（carved 埋点原字节）。
# 无免密 sudo（本机日常）自动跳过；GitHub ubuntu-latest 免密 sudo → 真跑。
set -euo pipefail
cd "$(dirname "$0")/.."

sudo -n true 2>/dev/null || { echo "skip: 无免密 sudo（真块设备 e2e 需 root 建环回）"; exit 0; }

img=$(mktemp /tmp/xd-loop-$$-XXXX.img)
tmpdb=$(mktemp /tmp/xd-loop-db-XXXXXX)
# 步骤 3c 的挂载点/导出目录（trap 需要，提前声明为可清态；root 属主文件非 root 删不掉 → sudo）
loop=""; mnt=""; outdir=""; expdir=""
rm -f "$tmpdb"   # 让 sqlite 自建；步骤 2/3 共用——防 sudo 下写 root/真实 HOME 状态库（同增补 1 类纪律）
# trap 兜底：清理失败不得反噬脚本退出码（set -euo pipefail）——tmpdb 由 root 属主 daemon 建，
# 非 root 在 sticky /tmp 上 rm -f 会 EPERM，必须 sudo + || true；其余清理同款兜底防中断。
trap 'if mountpoint -q "${mnt:-/nonexistent}" 2>/dev/null; then sudo umount "$mnt" || true; fi;
      if [ -n "$loop" ]; then sudo losetup -d "$loop" || true; fi;
      rm -f "$img" || true;
      [ -z "$tmpdb" ] || sudo rm -f "$tmpdb" || true;
      for d in "$mnt" "$outdir" "$expdir"; do [ -z "$d" ] || sudo rm -rf "$d" || true; done' EXIT

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

# 3b) M1c：深扫真链路（mode:deep → finished → results）。镜像（gen_fat_image）在已删照片释放的
#     簇区埋了一枚结构完整 JPEG（偏移 26112 = 数据区簇 3 起点）——深扫必须雕出且**恰 1 条**：
#     恢复率门禁（§8.2）在真块设备上的那一臂（假阳性 0 = carved 计数恰 1）。
printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":22,\"method\":\"scan.start\",\"params\":{\"device\":\"unix:$loop\",\"mode\":\"deep\"}}" >&"${XD[1]}"
deep_start=""; deep_task=""; deep_fin=""
while :; do
  IFS= read -r -t 30 line <&"${XD[0]}" || { echo "FAIL: 等深扫 finished 超时/断流"; exit 1; }
  case "$line" in
    *'"id":22'*) deep_start="$line"; deep_task=$(sed -n 's/.*"taskId":\([0-9]*\).*/\1/p' <<<"$line");;
    *'"method":"scan.finished"'*) deep_fin="$line"; break;;
  esac
done
grep -qF '"fs":"fat"' <<<"$deep_start" || { echo "FAIL: 深扫 start 未认 FAT"; echo "$deep_start"; exit 1; }
grep -qF '"totalBytes":2136576}' <<<"$deep_start" || { echo "FAIL: 深扫 totalBytes 非 Σ空闲区间 2136576"; echo "$deep_start"; exit 1; }
grep -qF '"state":"completed"' <<<"$deep_fin" || { echo "FAIL: 深扫未 completed"; echo "$deep_fin"; exit 1; }
printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":23,\"method\":\"scan.results\",\"params\":{\"taskId\":$deep_task,\"offset\":0,\"limit\":100,\"deletedOnly\":false}}" >&"${XD[1]}"
while :; do
  IFS= read -r -t 30 line <&"${XD[0]}" || { echo "FAIL: 等深扫 results 超时/断流"; exit 1; }
  case "$line" in *'"id":23'*) dres_line="$line"; break;; esac
done
grep -qF '"quality":"carved"' <<<"$dres_line" || { echo "FAIL: 深扫 results 未含 carved 条目"; echo "$dres_line"; exit 1; }
# grep 一律带后随定界（, 或 }）：防字段号子串误命中（如 byteOffset 261120）
grep -qF '"byteOffset":26112,' <<<"$dres_line" || { echo "FAIL: carved byteOffset 非埋点 26112"; echo "$dres_line"; exit 1; }
grep -qF '"sizeBytes":2045}' <<<"$dres_line" || { echo "FAIL: carved sizeBytes 非 2045"; echo "$dres_line"; exit 1; }
[ "$(grep -oF '"quality":"carved"' <<<"$dres_line" | wc -l)" = 1 ] || { echo "FAIL: carved 条目非恰 1 条（假阳性？）"; echo "$dres_line"; exit 1; }
echo "deep: task $deep_task completed，恰 1 条 carved（byteOffset=26112）经 IPC 可见"

# 3c) M1d 导出真集成：同盘判定（-32006）的真块设备臂——把源挂载起来，导出目标取其上的既有
#     目录（-o ro 挂载不可新建；-32006 在写前拦截，与目标可写性无关）。挂载点用 mktemp -d
#     而非固定 /mnt/xd-test（路径无断言意义，避免 CI /mnt 权限面）。
mnt=$(mktemp -d)
sudo mount -o ro "$loop" "$mnt" || { echo "FAIL: 环回设备挂载失败（内核拒认夹具 FAT？）"; exit 1; }
[ -d "$mnt/DCIM" ] || { echo "FAIL: 挂载后未见 DCIM（夹具几何漂移？）"; exit 1; }
carved_idx=$(grep -o '"idx":[0-9]*[^}]*"quality":"carved"' <<<"$dres_line" \
  | sed -n 's/.*"idx":\([0-9]*\).*/\1/p')
[ -n "$carved_idx" ] || { echo "FAIL: 未能从深扫结果提取 carved idx"; echo "$dres_line"; exit 1; }
printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":30,\"method\":\"export.start\",\"params\":{\"taskId\":$deep_task,\"idxs\":[$carved_idx],\"targetDir\":\"$mnt/DCIM\"}}" >&"${XD[1]}"
while :; do
  IFS= read -r -t 30 line <&"${XD[0]}" || { echo "FAIL: 等 -32006 响应超时/断流"; exit 1; }
  # 只认响应行（"id":30 后随逗号定界；通知行无 id）
  case "$line" in *'"id":30,'*) e1="$line"; break;; esac
done
grep -qF '"code":-32006' <<<"$e1" || { echo "FAIL: 导出到源设备挂载点未回 -32006"; echo "$e1"; exit 1; }
sudo umount "$mnt"; rmdir "$mnt"; mnt=""
echo "-32006: 目标在源设备（环回挂载点）上被拒"

# 3d) 导出到普通目录（mktemp -d）：export.start → finished → 落盘字节 == 镜像内埋点原字节。
#     进程为 root（CI 免密 sudo）且无 PKEXEC_UID → worker 按设计不降权（stderr 有留痕），
#     落盘 root 属主；outdir/expdir 清理走 sudo。
outdir=$(mktemp -d); expdir=$(mktemp -d)
dd if="$img" of="$expdir/expected.bin" bs=1 skip=26112 count=2045 status=none   # 埋点原字节
printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":31,\"method\":\"export.start\",\"params\":{\"taskId\":$deep_task,\"idxs\":[$carved_idx],\"targetDir\":\"$outdir\"}}" >&"${XD[1]}"
exp_id=""; fin_line=""
while :; do
  IFS= read -r -t 30 line <&"${XD[0]}" || { echo "FAIL: 等导出 finished 超时/断流"; exit 1; }
  case "$line" in
    *'"id":31,'*) exp_id=$(sed -n 's/.*"exportId":\([0-9]*\).*/\1/p' <<<"$line");;
    *'"method":"export.finished"'*) fin_line="$line";;
  esac
  [ -n "$exp_id" ] && [ -n "$fin_line" ] && break   # 抢跑（finished 先于响应）也在此收口
done
grep -qF "\"exportId\":$exp_id," <<<"$fin_line" || { echo "FAIL: finished exportId 与响应不符"; echo "$fin_line"; exit 1; }
grep -qF '"succeeded":1' <<<"$fin_line" || { echo "FAIL: 导出未 succeeded=1"; echo "$fin_line"; exit 1; }
exported=$(ls -A "$outdir")
n=$(printf '%s\n' "$exported" | wc -l)
[ "$n" = 1 ] && [ -f "$outdir/$exported" ] || { echo "FAIL: 目标目录落盘件数非 1（got: $exported）"; exit 1; }
cmp "$outdir/$exported" "$expdir/expected.bin" || { echo "FAIL: 导出字节与埋点原字节不符"; exit 1; }
echo "export: task $deep_task idx=$carved_idx → $exported 逐字节 == 埋点（26112,2045）"
sudo rm -rf "$outdir" "$expdir"; outdir=""; expdir=""

eval "exec ${XD[1]}>&-"   # 关 stdin → daemon 收 EOF 退出
wait "$XD_PID"
echo "scan: task $scan_task completed，删除文件 ?MG_0001.JPG 经 IPC 可见"

# 4) 只读铁律：源镜像扫描前后 sha256 不变
[ "$before" = "$(sha256sum "$img" | cut -d' ' -f1)" ] || { echo "FAIL: 源镜像被改写"; exit 1; }
echo "LOOP E2E OK"
